//! Input event routing.
//!
//! The single exported function `handle_event` receives a `crossterm::Event`
//! and the mutable `AppState`, applies the appropriate mutation, and returns
//! an `Action` that tells `main.rs` what to do next (quit, save, etc.).
//!
//! All branching is on `AppState::mode` first, then the key/mouse event.
//! No rendering happens here.

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use serde_json::Value;
use tui_textarea::TextArea;

use crate::format::FileFormat;
use crate::state::{AppState, ConfirmAction, Focus, Mode, TreeEditKind};
use crate::tree::{split_path, JsonTree, NodeKind};
use crate::sync::{raw_to_tree, tree_to_raw, find_line_for_path, ParseResult};

/// High-level actions returned to the main loop after processing an event.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Continue running normally.
    Continue,
    /// Save the file immediately (Ctrl+S).
    Save,
    /// Quit the application.
    Quit,
    /// Load the file at the given path into the editor.
    OpenFile(PathBuf),
    /// Nothing happened (event was ignored).
    Noop,
}

/// Process one crossterm `Event`.
///
/// `textarea` is the `tui_textarea::TextArea` widget that owns the raw
/// pane content. We keep it separate from `AppState` because the widget
/// owns its own cursor state.
pub fn handle_event(
    event: &Event,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
) -> Action {
    match event {
        Event::Key(key) => handle_key(key, state, textarea),
        Event::Mouse(mouse) => handle_mouse(mouse, state, textarea),
        Event::Paste(text) => handle_paste(text, state, textarea),
        Event::Resize(_, _) => Action::Continue,
        _ => Action::Noop,
    }
}

// ── key dispatch ─────────────────────────────────────────────────────────────

fn handle_key(
    key: &KeyEvent,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
) -> Action {
    use Mode::*;

    // Global shortcuts that work in every mode.
    if key.modifiers == KeyModifiers::CONTROL {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                return maybe_quit(state);
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                // If no file path yet, prompt for one instead of defaulting.
                if state.file_path.is_none() {
                    state.mode = Mode::SaveAs { buffer: String::new() };
                    return Action::Continue;
                }
                return Action::Save;
            }
            KeyCode::Char('o') | KeyCode::Char('O') => {
                return open_file_prompt(state);
            }
            // Ctrl+V: paste from the system clipboard.
            KeyCode::Char('v') | KeyCode::Char('V') => {
                return paste_from_clipboard(state, textarea);
            }
            // Ctrl+Z / Ctrl+Y: undo / redo — global so they work from Normal
            // mode (tree pane) as well as Insert mode.  Any mode that captures
            // Ctrl+Z before this block (none currently) would shadow it, so
            // keeping undo global is safe.
            KeyCode::Char('z') | KeyCode::Char('Z') => {
                if let Some(snap) = state.apply_undo() {
                    let lines: Vec<String> = snap.text.lines().map(String::from).collect();
                    *textarea = TextArea::from(lines);
                    reparse_now(state);
                }
                return Action::Continue;
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(snap) = state.apply_redo() {
                    let lines: Vec<String> = snap.text.lines().map(String::from).collect();
                    *textarea = TextArea::from(lines);
                    reparse_now(state);
                }
                return Action::Continue;
            }
            // Ctrl+E: jump raw pane cursor to the parse-error location.
            KeyCode::Char('e') | KeyCode::Char('E') => {
                if let Some(msg) = state.parse_error.clone() {
                    if let Some(line) = extract_error_line(&msg) {
                        textarea.move_cursor(tui_textarea::CursorMove::Jump(line as u16, 0));
                        state.focus = Focus::RawPane;
                    }
                }
                return Action::Continue;
            }
            _ => {}
        }
    }

    // `?` opens the about screen from any mode except Insert (where it types).
    if key.modifiers == KeyModifiers::NONE
        && key.code == KeyCode::Char('?')
        && !matches!(state.mode, Mode::Insert)
    {
        state.mode = Mode::About;
        return Action::Continue;
    }

    match &state.mode.clone() {
        Normal => handle_normal(key, state, textarea),
        Insert => handle_insert(key, state, textarea),
        TreeEdit { kind, buffer } => {
            let kind = kind.clone();
            let buffer = buffer.clone();
            handle_tree_edit(key, state, textarea, kind, buffer)
        }
        Search => handle_search(key, state, textarea),
        Mode::Replace { replacement } => {
            let replacement = replacement.clone();
            handle_replace(key, state, textarea, replacement)
        }
        Confirm { action, .. } => {
            let action = action.clone();
            handle_confirm(key, state, textarea, action)
        }
        Mode::FilePicker { selected, scroll, .. } => {
            let selected = *selected;
            let scroll = *scroll;
            handle_file_picker(key, state, selected, scroll)
        }
        // About overlay: any key dismisses it.
        Mode::About => {
            state.mode = Mode::Normal;
            Action::Continue
        }
        Mode::SaveAs { buffer } => {
            let buffer = buffer.clone();
            handle_save_as(key, state, buffer)
        }
    }
}

// ── normal mode ───────────────────────────────────────────────────────────────
//
// Key-priority rules (top wins):
//   1. Pane-independent shortcuts checked first: Tab, q, /
//   2. Raw-pane shortcuts: `i`/Enter = enter Insert, arrows = scroll
//   3. Tree-pane shortcuts: navigation + editing operations
//
// The previous implementation had a broad `Char(_) if raw_pane` pattern that
// caught everything (including `/`, `i`, `q`) before the specific handlers.
// That is removed here. Every `Char` match now uses an explicit character or
// a focus guard so shortcuts are never silently swallowed.

fn handle_normal(
    key: &KeyEvent,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
) -> Action {
    let no_mod = key.modifiers == KeyModifiers::NONE;

    match key.code {
        // ── pane-independent ──────────────────────────────────────────────────

        // Tab: switch focus between panes.
        KeyCode::Tab if no_mod => {
            state.toggle_focus();
            Action::Continue
        }

        // q / Q: quit (with unsaved-changes prompt if needed).
        KeyCode::Char('q') | KeyCode::Char('Q') if no_mod => maybe_quit(state),

        // /: open search bar.  Works from both panes.
        KeyCode::Char('/') if no_mod => {
            state.mode = Mode::Search;
            state.search_query.clear();
            if let Some(tree) = state.tree.as_mut() {
                tree.set_search("");
            }
            // Ensure the tree pane is focused so the filter result is visible.
            state.focus = Focus::TreePane;
            Action::Continue
        }

        // ── raw pane ──────────────────────────────────────────────────────────

        // `i` or Enter: enter Insert mode.
        // Enter is included so users who don't know the Vim convention can
        // still start editing intuitively.
        KeyCode::Char('i') | KeyCode::Enter
            if state.focus == Focus::RawPane && no_mod =>
        {
            state.mode = Mode::Insert;
            Action::Continue
        }

        // Ctrl+A (raw pane, Normal mode): enter Insert mode and select all.
        // This lets the user immediately replace the whole buffer by typing or
        // pasting, matching the behaviour they expect from standard editors.
        // (Cmd+A is intercepted by the terminal itself and never reaches us.)
        KeyCode::Char('a')
            if state.focus == Focus::RawPane
                && key.modifiers == KeyModifiers::CONTROL =>
        {
            state.mode = Mode::Insert;
            raw_pane_select_all(textarea);
            Action::Continue
        }

        // Shift + Arrow (raw pane, Normal mode): enter Insert mode and begin /
        // extend a selection, matching standard editor behaviour (Shift+→ etc.).
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right
            if state.focus == Focus::RawPane
                && key.modifiers == KeyModifiers::SHIFT =>
        {
            state.mode = Mode::Insert;
            textarea.input(crossterm::event::Event::Key(*key));
            Action::Continue
        }

        // Arrow / page / home / end keys: scroll the raw pane without entering
        // Insert mode.  Forwarded directly to the textarea widget.
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Left
        | KeyCode::Right
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Home
        | KeyCode::End
            if state.focus == Focus::RawPane =>
        {
            textarea.input(crossterm::event::Event::Key(*key));
            Action::Continue
        }

        // ── tree pane: navigation ─────────────────────────────────────────────

        KeyCode::Up | KeyCode::Char('k') if state.focus == Focus::TreePane && no_mod => {
            state.tree_up(1);
            sync_raw_cursor_to_selection(state, textarea);
            Action::Continue
        }
        KeyCode::Down | KeyCode::Char('j') if state.focus == Focus::TreePane && no_mod => {
            state.tree_down(1);
            sync_raw_cursor_to_selection(state, textarea);
            Action::Continue
        }
        KeyCode::PageUp if state.focus == Focus::TreePane => {
            state.tree_up(state.tree_pane_height as usize);
            sync_raw_cursor_to_selection(state, textarea);
            Action::Continue
        }
        KeyCode::PageDown if state.focus == Focus::TreePane => {
            state.tree_down(state.tree_pane_height as usize);
            sync_raw_cursor_to_selection(state, textarea);
            Action::Continue
        }
        // Home / End: jump to the very first or last visible node.
        KeyCode::Home if state.focus == Focus::TreePane => {
            state.tree_selected = 0;
            state.ensure_visible();
            sync_raw_cursor_to_selection(state, textarea);
            Action::Continue
        }
        KeyCode::End if state.focus == Focus::TreePane => {
            let last = state
                .tree
                .as_ref()
                .map(|t| t.visible_node_count().saturating_sub(1))
                .unwrap_or(0);
            state.tree_selected = last;
            state.ensure_visible();
            sync_raw_cursor_to_selection(state, textarea);
            Action::Continue
        }

        // ── tree pane: collapse / expand ──────────────────────────────────────

        // Space: toggle collapse on the selected container node.
        KeyCode::Char(' ') if state.focus == Focus::TreePane && no_mod => {
            if let Some(tree) = state.tree.as_mut() {
                tree.toggle_collapse(state.tree_selected);
            }
            // After collapsing, children disappear from the flat list.
            // Clamp before ensure_visible so the cursor doesn't point into
            // now-hidden rows (which would make subsequent expand attempts
            // silently do nothing — the index returns None from flat_nodes).
            state.clamp_tree_selection();
            state.ensure_visible();
            Action::Continue
        }

        // Right: expand a collapsed container, or move to its first child.
        KeyCode::Right if state.focus == Focus::TreePane && no_mod => {
            tree_right_action(state);
            Action::Continue
        }

        // Left: collapse an expanded container, or jump to its parent.
        KeyCode::Left if state.focus == Focus::TreePane && no_mod => {
            tree_left_action(state);
            Action::Continue
        }

        // ── tree pane: Enter — context-sensitive ──────────────────────────────
        // • Container node → toggle collapse/expand (same as Space)
        // • Leaf node      → open inline value editor
        KeyCode::Enter if state.focus == Focus::TreePane && no_mod => {
            tree_enter_action(state);
            Action::Continue
        }

        // ── tree pane: editing ────────────────────────────────────────────────

        // `a`: add item.  Detects array vs. object and skips key prompt for arrays.
        KeyCode::Char('a') if state.focus == Focus::TreePane && no_mod => {
            tree_add_action(state);
            Action::Continue
        }
        // `d`: delete selected node (confirm Y/n).
        KeyCode::Char('d') if state.focus == Focus::TreePane && no_mod => {
            tree_delete_prompt(state);
            Action::Continue
        }
        // `r`: rename key (objects only; error for array indices).
        KeyCode::Char('r') if state.focus == Focus::TreePane && no_mod => {
            tree_rename_prompt(state);
            Action::Continue
        }
        // `y`: copy node value to clipboard.
        KeyCode::Char('y') if state.focus == Focus::TreePane && no_mod => {
            copy_node_value(state);
            Action::Continue
        }
        // `g` / `l`: jump to the matching line in the raw pane.
        KeyCode::Char('g') | KeyCode::Char('l')
            if state.focus == Focus::TreePane && no_mod =>
        {
            jump_to_line_in_raw(state, textarea);
            Action::Continue
        }

        _ => Action::Noop,
    }
}

// ── normal-mode tree helpers ──────────────────────────────────────────────────

/// Enter key on a tree node:
/// * container → toggle collapse/expand
/// * leaf      → open the inline value editor (buffer starts empty; original
///               value is shown as a read-only label in the status bar)
fn tree_enter_action(state: &mut AppState) {
    let node_info = state
        .tree
        .as_ref()
        .and_then(|t| t.flat_nodes().get(state.tree_selected))
        .map(|n| (n.kind.is_container(), n.value_display(), n.kind.label().to_string()));

    let Some((is_container, val_display, type_label)) = node_info else {
        return;
    };

    if is_container {
        if let Some(tree) = state.tree.as_mut() {
            tree.toggle_collapse(state.tree_selected);
        }
        state.clamp_tree_selection();
        state.ensure_visible();
    } else {
        state.mode = Mode::TreeEdit {
            kind: TreeEditKind::EditValue {
                node_type: type_label,
                original: val_display,
            },
            // Empty buffer: the user types the replacement value from scratch.
            // The original is shown as a label so they know what they're replacing.
            buffer: String::new(),
        };
    }
}

/// Right arrow on a tree node:
/// * Collapsed container → expand it
/// * Expanded container  → move cursor to first child
/// * Leaf                → no-op
fn tree_right_action(state: &mut AppState) {
    let node_info = state
        .tree
        .as_ref()
        .and_then(|t| t.flat_nodes().get(state.tree_selected))
        .map(|n| (n.kind.is_container(), n.collapsed));

    match node_info {
        Some((true, true)) => {
            // Collapsed container → expand.
            if let Some(tree) = state.tree.as_mut() {
                tree.toggle_collapse(state.tree_selected);
            }
            state.clamp_tree_selection();
            state.ensure_visible();
        }
        Some((true, false)) => {
            // Already expanded → move to first child (next row).
            state.tree_down(1);
        }
        _ => {}
    }
}

/// Left arrow on a tree node:
/// * Expanded container → collapse it
/// * Any other node     → jump to parent container
fn tree_left_action(state: &mut AppState) {
    let node_info = state
        .tree
        .as_ref()
        .and_then(|t| t.flat_nodes().get(state.tree_selected))
        .map(|n| (n.kind.is_container(), n.collapsed, n.path.clone()));

    let Some((is_container, is_collapsed, path)) = node_info else {
        return;
    };

    if is_container && !is_collapsed {
        // Expanded container → collapse.
        if let Some(tree) = state.tree.as_mut() {
            tree.toggle_collapse(state.tree_selected);
        }
        state.clamp_tree_selection();
        state.ensure_visible();
    } else {
        // Move to parent node.
        if let Ok((parent_path, _)) = split_path(&path) {
            if !parent_path.is_empty() {
                let parent_idx = state
                    .tree
                    .as_ref()
                    .and_then(|t| t.index_of_path(parent_path));
                if let Some(idx) = parent_idx {
                    state.tree_selected = idx;
                    state.ensure_visible();
                }
            }
        }
    }
}

/// `a` key: add a new item to the selected container node.
/// * Object → prompt for key name first, then value
/// * Array  → prompt for value directly (arrays have no named keys)
/// * Leaf   → show an error
fn tree_add_action(state: &mut AppState) {
    let kind = state
        .tree
        .as_ref()
        .and_then(|t| t.flat_nodes().get(state.tree_selected))
        .map(|n| n.kind.clone());

    match kind {
        Some(NodeKind::Object { .. }) => {
            state.mode = Mode::TreeEdit {
                kind: TreeEditKind::AddKey,
                buffer: String::new(),
            };
        }
        Some(NodeKind::Array { .. }) => {
            // Arrays don't have string keys; go straight to value input.
            state.mode = Mode::TreeEdit {
                kind: TreeEditKind::AddValue { key: String::new() },
                buffer: String::new(),
            };
        }
        _ => {
            state.parse_error = Some(
                "Select an object { } or array [ ] node first, then press 'a' to add".to_string(),
            );
        }
    }
}

/// `d` key: confirm-delete the selected node.
fn tree_delete_prompt(state: &mut AppState) {
    let node_label = state
        .tree
        .as_ref()
        .and_then(|t| t.flat_nodes().get(state.tree_selected))
        .map(|n| {
            if n.key.is_empty() {
                format!("(root {})", n.kind.label())
            } else {
                n.key.clone()
            }
        })
        .unwrap_or_else(|| "node".to_string());

    state.mode = Mode::Confirm {
        question: format!("Delete '{}'? [y/N]", node_label),
        action: ConfirmAction::DeleteNode,
    };
}

/// `r` key: rename the selected node's key.
/// Disallowed on array indices (they're auto-numbered).
fn tree_rename_prompt(state: &mut AppState) {
    let node_info = state
        .tree
        .as_ref()
        .and_then(|t| t.flat_nodes().get(state.tree_selected))
        .map(|n| (n.key.clone(), n.key.parse::<usize>().is_ok()));

    let Some((current_key, is_array_index)) = node_info else {
        return;
    };

    if is_array_index || current_key.is_empty() {
        state.parse_error =
            Some("Cannot rename array items or the root node".to_string());
        return;
    }

    state.mode = Mode::TreeEdit {
        kind: TreeEditKind::RenameKey,
        buffer: current_key,
    };
}

// ── insert mode (raw pane) ────────────────────────────────────────────────────

/// Select all text in the textarea (equivalent of Ctrl+A / Cmd+A in a normal
/// editor).
///
/// We set the *anchor* at the very end of the document first, then move the
/// *cursor* back to the beginning.  This way:
///   * the entire document is highlighted, and
///   * the viewport scrolls to show the **top** (where the cursor lands),
///     rather than the bottom — which is what happens when you start at the
///     top and end at the bottom.
fn raw_pane_select_all(textarea: &mut TextArea<'static>) {
    // CursorMove::Top and CursorMove::Bottom both apply fit_col, which preserves
    // the current column (clamped to the target line's length).  They do NOT
    // reliably land at (0,0) or (last_row, last_col).
    //
    // CursorMove::Jump clamps out-of-range values to the actual document bounds,
    // so Jump(u16::MAX, u16::MAX) is always the true end of the document and
    // Jump(0, 0) is always the true beginning — independent of current column or
    // line lengths.
    //
    // We anchor at the end and move the cursor to the beginning so that the
    // viewport scrolls to show row 0 (the cursor's destination), not the bottom.
    textarea.move_cursor(tui_textarea::CursorMove::Jump(u16::MAX, u16::MAX)); // anchor at doc end
    textarea.start_selection();                                                // mark anchor here
    textarea.move_cursor(tui_textarea::CursorMove::Jump(0, 0));               // cursor → (0,0), viewport follows
}

fn handle_insert(
    key: &KeyEvent,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
) -> Action {
    match (key.modifiers, key.code) {
        // Exit insert mode
        (KeyModifiers::NONE, KeyCode::Esc) => {
            state.mode = Mode::Normal;
            Action::Continue
        }
        // Ctrl+A: select all text in the raw pane.
        (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
            raw_pane_select_all(textarea);
            Action::Continue
        }
        // All other keys: forward to textarea
        _ => {
            // Record undo snapshot before the mutation if the key modifies text.
            let modifying = matches!(
                key.code,
                KeyCode::Char(_)
                    | KeyCode::Enter
                    | KeyCode::Backspace
                    | KeyCode::Delete
            );
            if modifying {
                state.push_undo();
            }

            // Let tui-textarea handle the key.
            textarea.input(crossterm::event::Event::Key(*key));

            // Sync raw text — single allocation, no clone needed.
            state.raw_text = textarea.lines().join("\n");
            if modifying {
                state.modified = true;
                // Schedule a debounced reparse rather than reparsing on every
                // keystroke.  The event loop will reparse once typing pauses
                // for PARSE_DEBOUNCE milliseconds.  This keeps the UI
                // responsive with large files.
                state.parse_dirty = true;
                state.last_edit = Some(std::time::Instant::now());
            }
            Action::Continue
        }
    }
}

/// Immediately reparse `state.raw_text` and update the tree.
///
/// Used after undo/redo where the text change is complete and we want the
/// tree to reflect it right away (no debounce needed), and also by the
/// debounce timer in the main event loop.
///
/// Format auto-detection: if the current format fails to parse the buffer,
/// every other supported format is tried in order.  If one succeeds,
/// `state.format` is updated to match.  This handles the common case where
/// the user pastes YAML (or TOML/XML) while in Insert mode — the content
/// arrives as individual key events that bypass `insert_text`, so the first
/// chance to detect the format is here in the debounced reparse.
///
/// When a tree already exists, `update_value` is called instead of
/// constructing a new `JsonTree`, which preserves the collapsed-path set
/// without cloning it.
pub fn reparse_now(state: &mut AppState) {
    // An empty (or whitespace-only) buffer is not a parse error — it just means
    // there is no document yet.  Clear the tree and any stale error so the UI
    // shows the "no file" onboarding state rather than "(no valid JSON)".
    if state.raw_text.trim().is_empty() {
        state.tree = None;
        state.parse_error = None;
        state.parse_dirty = false;
        return;
    }

    let (detected, result) = match state.format.parse(&state.raw_text) {
        // Current format succeeded — no need to probe others.
        std::result::Result::Ok(value) => (state.format, std::result::Result::Ok(value)),
        // Current format failed — try other formats before giving up.
        std::result::Result::Err(original_err) => {
            let fallback = detect_format_and_parse(&state.raw_text, state.format);
            match fallback {
                Some((fmt, value)) => (fmt, std::result::Result::Ok(value)),
                None => (state.format, std::result::Result::Err(original_err)),
            }
        }
    };

    match result {
        std::result::Result::Ok(value) => {
            // Update format badge if we auto-detected a different format.
            state.format = detected;
            match state.tree.as_mut() {
                Some(tree) => tree.update_value(value),
                None => state.tree = Some(JsonTree::from_value(value, Default::default())),
            }
            state.parse_error = None;
            // Populate line numbers so raw↔tree click sync works immediately.
            refresh_line_numbers(state);
        }
        std::result::Result::Err(msg) => {
            state.parse_error = Some(msg);
        }
    }
    state.parse_dirty = false;
}

/// Re-populate `FlatNode::line_number` for every visible node.
///
/// Must be called after every successful parse — `reparse_now` does this
/// automatically.  Call it manually from `main.rs` after the initial load
/// and after `load_file`, where `reparse_now` is bypassed.
pub fn refresh_line_numbers(state: &mut AppState) {
    // Take the tree out to avoid a split-borrow conflict with state.raw_text.
    let mut tree = match state.tree.take() {
        Some(t) => t,
        None => return,
    };
    let raw = &state.raw_text;
    let fmt = state.format;
    tree.sync_line_numbers(|path| find_line_for_path(raw, path, fmt));
    state.tree = Some(tree);
}

// ── tree edit mode ────────────────────────────────────────────────────────────

fn handle_tree_edit(
    key: &KeyEvent,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
    kind: TreeEditKind,
    mut buffer: String,
) -> Action {
    match key.code {
        KeyCode::Esc => {
            state.mode = Mode::Normal;
            Action::Continue
        }
        KeyCode::Backspace => {
            buffer.pop();
            state.mode = Mode::TreeEdit { kind, buffer };
            Action::Continue
        }
        KeyCode::Enter => {
            commit_tree_edit(state, textarea, kind, buffer);
            Action::Continue
        }
        KeyCode::Char(c) => {
            buffer.push(c);
            state.mode = Mode::TreeEdit { kind, buffer };
            Action::Continue
        }
        _ => Action::Noop,
    }
}

fn commit_tree_edit(
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
    kind: TreeEditKind,
    buffer: String,
) {
    state.mode = Mode::Normal;

    // Extract the selected path via an immutable borrow that ends here so we
    // can call push_undo (&mut state) before the mutable tree borrow below.
    let selected_path = state
        .tree
        .as_ref()
        .and_then(|t| t.flat_nodes().get(state.tree_selected))
        .map(|n| n.path.clone())
        .unwrap_or_default();

    if state.tree.is_none() {
        return;
    }

    // AddKey is a two-phase UI flow (key → value); no mutation yet.
    if let TreeEditKind::AddKey = &kind {
        state.mode = Mode::TreeEdit {
            kind: TreeEditKind::AddValue { key: buffer },
            buffer: String::new(),
        };
        return;
    }

    // EditValue with no input → cancel silently.
    if let TreeEditKind::EditValue { .. } = &kind {
        if buffer.is_empty() {
            return;
        }
    }

    // Snapshot before mutation so Ctrl+Z can restore.
    state.push_undo();

    let Some(tree) = state.tree.as_mut() else {
        return;
    };

    let result = match kind {
        TreeEditKind::AddKey => unreachable!("handled above"),
        TreeEditKind::AddValue { key } => {
            let value = parse_value_input(&buffer);
            tree.add_key(&selected_path, key, value)
        }
        TreeEditKind::RenameKey => tree.rename_key_at_path(&selected_path, buffer),
        TreeEditKind::EditValue { .. } => {
            let value = parse_value_input(&buffer);
            tree.set_value_at_path(&selected_path, value)
        }
    };

    match result {
        Ok(()) => {
            let new_raw = tree_to_raw(tree, state.format);
            state.raw_text = new_raw.clone();
            state.modified = true;
            let lines: Vec<String> = new_raw.lines().map(String::from).collect();
            *textarea = TextArea::from(lines);
            state.parse_error = None;
        }
        Err(e) => {
            state.parse_error = Some(e);
        }
    }
}

/// Attempt to parse a user-typed value string into a `serde_json::Value`.
/// Fallback: treat as a JSON string literal.
fn parse_value_input(s: &str) -> Value {
    // Try to parse as a JSON value first.
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        return v;
    }
    // Fallback: treat as a plain string.
    Value::String(s.to_string())
}

// ── search mode ───────────────────────────────────────────────────────────────
//
// While in Search mode the tree shows only matching nodes (filtered view).
// The user can navigate with ↑/↓ to choose a result, then press Enter to
// jump to it — which:
//   1. Clears the filter and restores the full tree
//   2. Expands any collapsed ancestors so the node is visible
//   3. Selects and scrolls to the node in the tree pane
//   4. Jumps the raw pane cursor to the corresponding line
//
// Esc discards the filter and returns to Normal without navigating anywhere.

fn handle_search(
    key: &KeyEvent,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
) -> Action {
    match key.code {
        // ── cancel ────────────────────────────────────────────────────────────
        KeyCode::Esc => {
            search_cancel(state);
            Action::Continue
        }

        // ── commit: jump to the selected result ───────────────────────────────
        KeyCode::Enter => {
            search_commit(state, textarea);
            Action::Continue
        }

        // ── navigate among results ────────────────────────────────────────────
        // ↑ / k  →  previous match
        // ↓ / j  →  next match
        // j and k are consumed here (not typed into the query).
        KeyCode::Up | KeyCode::Char('k') => {
            search_move(state, Direction::Prev);
            Action::Continue
        }
        KeyCode::Down | KeyCode::Char('j') => {
            search_move(state, Direction::Next);
            Action::Continue
        }

        // ── open replace mode ─────────────────────────────────────────────────
        // Ctrl+R from search: keep the current filter active and enter replace.
        // Using Ctrl so that bare 'r' can still be part of a search query.
        // Jump to the first *leaf* match immediately so the user never starts
        // on a container node that cannot have its value replaced.
        KeyCode::Char('r') if key.modifiers == KeyModifiers::CONTROL => {
            let on_container = state
                .tree
                .as_ref()
                .and_then(|t| t.flat_nodes().get(state.tree_selected))
                .map(|n| n.kind.is_container())
                .unwrap_or(false);
            if on_container {
                search_move_leaf(state, Direction::Next);
            }
            state.mode = Mode::Replace {
                replacement: String::new(),
            };
            Action::Continue
        }

        // ── edit the query ────────────────────────────────────────────────────
        KeyCode::Backspace => {
            state.search_query.pop();
            search_apply_query(state);
            Action::Continue
        }
        KeyCode::Char(c) => {
            state.search_query.push(c);
            search_apply_query(state);
            Action::Continue
        }

        _ => Action::Noop,
    }
}

/// Direction for `search_move`.
enum Direction { Prev, Next }

/// Apply the current `search_query` to the tree and auto-select the first
/// matching node so the user gets immediate visual feedback.
fn search_apply_query(state: &mut AppState) {
    let query = state.search_query.clone();
    if let Some(tree) = state.tree.as_mut() {
        tree.set_search(&query);
    }
    // Auto-select the first match so the user can see what matched.
    if let Some(tree) = state.tree.as_ref() {
        if let Some(idx) = tree.first_search_match() {
            state.tree_selected = idx;
            state.ensure_visible();
        }
    }
}

/// Move the tree cursor to the previous or next match within the filtered list.
///
/// When `leaves_only` is true only leaf nodes (non-containers) are considered.
/// Replace mode uses this so it never lands on an object/array node that cannot
/// have its value edited.
fn search_move_impl(state: &mut AppState, dir: Direction, leaves_only: bool) {
    let Some(tree) = state.tree.as_ref() else { return };
    let nodes = tree.flat_nodes();
    let current = state.tree_selected;
    let count = nodes.len();
    if count == 0 { return; }

    let matches = |i: usize| {
        nodes[i].search_match && (!leaves_only || !nodes[i].kind.is_container())
    };

    let new_idx = match dir {
        Direction::Prev => {
            (0..count)
                .rev()
                .map(|offset| (current + count - 1 - offset) % count)
                .find(|&i| matches(i))
        }
        Direction::Next => {
            (1..=count)
                .map(|offset| (current + offset) % count)
                .find(|&i| matches(i))
        }
    };
    if let Some(idx) = new_idx {
        state.tree_selected = idx;
        state.ensure_visible();
    }
}

/// Move to the previous or next search match (any node type).
fn search_move(state: &mut AppState, dir: Direction) {
    search_move_impl(state, dir, false);
}

/// Move to the previous or next search match that is a replaceable leaf node.
fn search_move_leaf(state: &mut AppState, dir: Direction) {
    search_move_impl(state, dir, true);
}

/// Cancel search: clear filter, restore Normal mode, keep the cursor where
/// it was (clamped to the now-larger full tree).
fn search_cancel(state: &mut AppState) {
    state.mode = Mode::Normal;
    state.search_query.clear();
    if let Some(tree) = state.tree.as_mut() {
        tree.clear_search();
    }
    state.clamp_tree_selection();
    state.ensure_visible();
}

/// Commit search: restore full tree, jump to the selected node, and navigate
/// the raw pane to the corresponding line.
fn search_commit(state: &mut AppState, textarea: &mut TextArea<'static>) {
    // 1. Capture the path of the currently selected node in the filtered view.
    //    If the selected row isn't itself a match (e.g. it's an ancestor shown
    //    for context), fall back to the first explicit match.
    let target_path: Option<String> = state
        .tree
        .as_ref()
        .and_then(|t| {
            let nodes = t.flat_nodes();
            let sel = nodes.get(state.tree_selected);
            if sel.map(|n| n.search_match).unwrap_or(false) {
                sel.map(|n| n.path.clone())
            } else {
                t.first_search_match()
                    .and_then(|i| nodes.get(i))
                    .map(|n| n.path.clone())
            }
        });

    // 2. Clear the filter to restore the full, unfiltered tree.
    state.mode = Mode::Normal;
    state.search_query.clear();
    if let Some(tree) = state.tree.as_mut() {
        tree.clear_search();
    }

    let Some(path) = target_path else { return };

    // 3. Expand all collapsed ancestors so the node is reachable.
    if let Some(tree) = state.tree.as_mut() {
        tree.expand_to_path(&path);
    }

    // 4. Find the node's new index in the full flat list and select it.
    if let Some(tree) = state.tree.as_ref() {
        if let Some(idx) = tree.index_of_path(&path) {
            state.tree_selected = idx;
            state.ensure_visible();
        }
    }

    // 5. Sync the raw pane cursor to the line for this path.
    //    Focus stays on the tree pane so the user can keep navigating;
    //    the raw pane scrolls silently to stay in sync.
    jump_to_path_in_raw(state, textarea, &path);
    state.focus = Focus::TreePane;
}

// ── replace mode ──────────────────────────────────────────────────────────────
//
// Replace extends the search filter: the tree remains filtered while the user
// types a replacement value.  Key bindings:
//
//   ↑/k  ↓/j   — navigate among remaining matches
//   Enter       — replace the selected match and advance to the next one
//   Ctrl+A      — replace every match in one shot
//   Esc         — go back to Search (keeps filter; user can review / re-search)

fn handle_replace(
    key: &KeyEvent,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
    mut replacement: String,
) -> Action {
    match key.code {
        // Cancel → return to search mode (filter stays active).
        KeyCode::Esc => {
            state.mode = Mode::Search;
            Action::Continue
        }

        // Navigate matches without replacing (arrow keys only — no j/k so
        // those letters are free to type in the replacement text).
        KeyCode::Up => {
            search_move(state, Direction::Prev);
            state.mode = Mode::Replace { replacement };
            Action::Continue
        }
        KeyCode::Down => {
            search_move(state, Direction::Next);
            state.mode = Mode::Replace { replacement };
            Action::Continue
        }

        // Replace current match and advance to the next one.
        KeyCode::Enter => {
            replace_current(state, textarea, &replacement);
            // Preserve the replacement text so the user can keep pressing Enter.
            if matches!(state.mode, Mode::Replace { .. }) {
                state.mode = Mode::Replace { replacement };
            }
            Action::Continue
        }

        // Replace every match at once.
        KeyCode::Char('a') if key.modifiers == KeyModifiers::CONTROL => {
            replace_all(state, textarea, &replacement);
            Action::Continue
        }

        // Edit the replacement buffer.
        // No `no_mod` guard: we accept all printable chars including uppercase
        // (Shift modifier) and symbols.  Ctrl+A is caught by the arm above;
        // Ctrl+other combinations fall to _ => Noop intentionally.
        KeyCode::Backspace => {
            replacement.pop();
            state.mode = Mode::Replace { replacement };
            Action::Continue
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            replacement.push(c);
            state.mode = Mode::Replace { replacement };
            Action::Continue
        }

        _ => Action::Noop,
    }
}

/// Replace the value of the currently selected match node with `replacement`,
/// then advance the cursor to the next remaining match.
///
/// Container nodes matched by key name (objects / arrays) are skipped
/// — only leaf nodes (string / number / bool / null) are replaced.
fn replace_current(
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
    replacement: &str,
) {
    // Capture the selected node's info before we borrow mutably.
    let node_info = state
        .tree
        .as_ref()
        .and_then(|t| t.flat_nodes().get(state.tree_selected))
        .map(|n| (n.search_match, n.kind.is_container(), n.path.clone()));

    let Some((is_match, is_container, path)) = node_info else {
        return;
    };

    if !is_match || is_container {
        // Not a replaceable leaf — skip forward to the next leaf match so the
        // user doesn't have to manually navigate past containers.
        search_move_leaf(state, Direction::Next);
        return;
    }

    // Snapshot before replacing so the user can Ctrl+Z back.
    state.push_undo();

    let new_value = parse_value_input(replacement);
    let query = state.search_query.clone();

    if let Some(tree) = state.tree.as_mut() {
        if let Err(e) = tree.set_value_at_path(&path, new_value) {
            state.parse_error = Some(e);
            return;
        }
        let new_raw = tree_to_raw(tree, state.format);
        state.raw_text = new_raw.clone();
        state.modified = true;
        let lines: Vec<String> = new_raw.lines().map(String::from).collect();
        *textarea = TextArea::from(lines);
        // Re-apply the search so the replaced node drops out of the match list.
        tree.set_search(&query);
    }

    state.parse_error = None;
    // Advance to the next *leaf* match (wraps if at end; exits if none remain).
    let has_more = state
        .tree
        .as_ref()
        .map(|t| t.flat_nodes().iter().any(|n| n.search_match && !n.kind.is_container()))
        .unwrap_or(false);

    if has_more {
        search_move_leaf(state, Direction::Next);
    } else {
        // All matches replaced — exit to Normal and clear the filter.
        state.mode = Mode::Normal;
        state.search_query.clear();
        if let Some(tree) = state.tree.as_mut() {
            tree.clear_search();
        }
        state.clamp_tree_selection();
    }
}

/// Replace the value of **every** currently matched leaf node in one pass,
/// then exit to Normal mode and clear the filter.
fn replace_all(
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
    replacement: &str,
) {
    // Collect all matched leaf paths up-front so we don't mutate while iterating.
    let paths: Vec<String> = state
        .tree
        .as_ref()
        .map(|t| {
            t.flat_nodes()
                .iter()
                .filter(|n| n.search_match && !n.kind.is_container())
                .map(|n| n.path.clone())
                .collect()
        })
        .unwrap_or_default();

    if paths.is_empty() {
        // Nothing to replace — return to Normal.
        state.mode = Mode::Normal;
        state.search_query.clear();
        if let Some(tree) = state.tree.as_mut() {
            tree.clear_search();
        }
        return;
    }

    // Snapshot before replacing so the user can Ctrl+Z the whole batch.
    state.push_undo();

    // Apply all mutations without rebuilding the flat list on each one,
    // then do a single rebuild at the end — O(N) instead of O(N × M).
    let mut first_error: Option<String> = None;
    if let Some(tree) = state.tree.as_mut() {
        for path in &paths {
            let val = parse_value_input(replacement);
            if let Err(e) = tree.set_value_at_path_no_rebuild(path, val) {
                first_error = Some(e);
                break;
            }
        }
        // Single rebuild after all mutations.
        tree.rebuild();
        let new_raw = tree_to_raw(tree, state.format);
        state.raw_text = new_raw.clone();
        state.modified = true;
        let lines: Vec<String> = new_raw.lines().map(String::from).collect();
        *textarea = TextArea::from(lines);
        tree.clear_search();
    }

    if let Some(e) = first_error {
        state.parse_error = Some(e);
    } else {
        state.parse_error = None;
    }

    state.mode = Mode::Normal;
    state.search_query.clear();
    state.clamp_tree_selection();
}

// ── confirm mode ──────────────────────────────────────────────────────────────

fn handle_confirm(
    key: &KeyEvent,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
    action: ConfirmAction,
) -> Action {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            state.mode = Mode::Normal;
            execute_confirm_action(state, textarea, action)
        }
        _ => {
            // Any other key cancels.
            state.mode = Mode::Normal;
            Action::Continue
        }
    }
}

fn execute_confirm_action(
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
    action: ConfirmAction,
) -> Action {
    match action {
        ConfirmAction::DeleteNode => {
            // Extract path via immutable borrow (dropped before push_undo).
            let path = state
                .tree
                .as_ref()
                .and_then(|t| t.flat_nodes().get(state.tree_selected))
                .map(|n| n.path.clone())
                .unwrap_or_default();

            // Snapshot before the delete so Ctrl+Z restores it.
            state.push_undo();

            if let Some(tree) = state.tree.as_mut() {
                if let Err(e) = tree.delete_at_path(&path) {
                    state.parse_error = Some(e);
                    return Action::Continue;
                }
                let new_raw = tree_to_raw(tree, state.format);
                state.raw_text = new_raw.clone();
                state.modified = true;
                let lines: Vec<String> = new_raw.lines().map(String::from).collect();
                *textarea = TextArea::from(lines);
                state.clamp_tree_selection();
            }
            Action::Continue
        }
        ConfirmAction::QuitUnsaved => Action::Quit,
        ConfirmAction::SaveAs { path } => {
            state.file_path = Some(PathBuf::from(path));
            Action::Save
        }
        ConfirmAction::DiscardAndOpen { path } => {
            Action::OpenFile(PathBuf::from(path))
        }
    }
}

// ── file picker mode ──────────────────────────────────────────────────────────

/// Transition into `Mode::FilePicker`, starting in the directory of the
/// currently open file (or the working directory if no file is open).
fn open_file_prompt(state: &mut AppState) -> Action {
    let start_dir = state
        .file_path
        .as_ref()
        .and_then(|p| p.parent())
        .and_then(|d| {
            if d.as_os_str().is_empty() {
                None
            } else {
                Some(d.to_path_buf())
            }
        })
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        });

    let entries = crate::state::read_dir_entries(&start_dir);
    state.mode = Mode::FilePicker {
        current_dir: start_dir,
        entries,
        selected: 0,
        scroll: 0,
    };
    Action::Continue
}

/// Handle key events while the file-picker overlay is open.
///
/// Navigation:
///   `↑`/`k`  — move selection up
///   `↓`/`j`  — move selection down
///   `PgUp`   — jump up by picker height
///   `PgDn`   — jump down by picker height
///   `Enter`  — descend into directory or open JSON file
///   `Esc`    — close picker without opening anything
fn handle_file_picker(
    key: &KeyEvent,
    state: &mut AppState,
    mut selected: usize,
    scroll: usize,
) -> Action {
    // Extract entries count without borrowing state for the whole match.
    let entry_count = match &state.mode {
        Mode::FilePicker { entries, .. } => entries.len(),
        _ => return Action::Noop,
    };
    let page = state.picker_height as usize;

    match key.code {
        KeyCode::Esc => {
            state.mode = Mode::Normal;
            Action::Continue
        }
        KeyCode::Up | KeyCode::Char('k') => {
            selected = selected.saturating_sub(1);
            picker_set_selected(state, selected, scroll);
            Action::Continue
        }
        KeyCode::Down | KeyCode::Char('j') => {
            selected = (selected + 1).min(entry_count.saturating_sub(1));
            picker_set_selected(state, selected, scroll);
            Action::Continue
        }
        KeyCode::PageUp => {
            selected = selected.saturating_sub(page);
            picker_set_selected(state, selected, scroll);
            Action::Continue
        }
        KeyCode::PageDown => {
            selected = (selected + page).min(entry_count.saturating_sub(1));
            picker_set_selected(state, selected, scroll);
            Action::Continue
        }
        KeyCode::Enter => picker_open_selected(state),
        _ => Action::Noop,
    }
}

/// Update the `selected` and `scroll` fields inside `Mode::FilePicker`,
/// clamping the viewport so the selected row is always visible.
fn picker_set_selected(state: &mut AppState, selected: usize, mut scroll: usize) {
    let height = state.picker_height as usize;
    // Clamp scroll so the selected row is in view.
    if selected < scroll {
        scroll = selected;
    } else if height > 0 && selected >= scroll + height {
        scroll = selected - height + 1;
    }
    if let Mode::FilePicker {
        selected: ref mut s,
        scroll: ref mut sc,
        ..
    } = state.mode
    {
        *s = selected;
        *sc = scroll;
    }
}

// ── mouse dispatch ────────────────────────────────────────────────────────────

fn handle_mouse(
    mouse: &MouseEvent,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
) -> Action {
    // File-picker overlay intercepts all mouse events while it is open.
    if matches!(state.mode, Mode::FilePicker { .. }) {
        return handle_mouse_file_picker(mouse, state);
    }

    match mouse.kind {
        // ── left click ───────────────────────────────────────────────────────
        MouseEventKind::Down(MouseButton::Left) => {
            let col = mouse.column;
            if col >= state.tree_x_start {
                handle_mouse_tree_click(mouse, state, textarea);
                sync_raw_cursor_to_selection(state, textarea);
            } else if col >= state.scrollbar_x_start {
                handle_mouse_scrollbar_seek(mouse, state, textarea);
            } else {
                handle_mouse_raw_click(mouse, state, textarea);
            }
            Action::Continue
        }

        // ── right click: focus the pane under the cursor ─────────────────────
        MouseEventKind::Down(MouseButton::Right) => {
            if mouse.column >= state.tree_x_start {
                state.focus = Focus::TreePane;
            } else if mouse.column < state.scrollbar_x_start {
                state.focus = Focus::RawPane;
            }
            Action::Continue
        }

        // ── scrollbar drag: same seek logic as a click ───────────────────────
        MouseEventKind::Drag(MouseButton::Left) => {
            let col = mouse.column;
            if col >= state.scrollbar_x_start && col < state.tree_x_start {
                handle_mouse_scrollbar_seek(mouse, state, textarea);
            }
            Action::Continue
        }

        // ── scroll wheel: act on the focused pane only ───────────────────────
        MouseEventKind::ScrollDown => {
            if state.focus == Focus::TreePane {
                state.tree_down(3);
                sync_raw_cursor_to_selection(state, textarea);
            } else {
                for _ in 0..3 {
                    textarea.move_cursor(tui_textarea::CursorMove::Down);
                }
                sync_tree_to_raw_cursor(state, textarea);
            }
            Action::Continue
        }
        MouseEventKind::ScrollUp => {
            if state.focus == Focus::TreePane {
                state.tree_up(3);
                sync_raw_cursor_to_selection(state, textarea);
            } else {
                for _ in 0..3 {
                    textarea.move_cursor(tui_textarea::CursorMove::Up);
                }
                sync_tree_to_raw_cursor(state, textarea);
            }
            Action::Continue
        }

        _ => Action::Noop,
    }
}

/// How many milliseconds between two clicks counts as a double-click.
const DOUBLE_CLICK_MS: u128 = 400;

/// Returns `true` if this click is at the same screen cell as the previous
/// one and happened within `DOUBLE_CLICK_MS`.
fn is_double_click(mouse: &MouseEvent, last: &Option<(u16, u16, Instant)>) -> bool {
    match last {
        Some((col, row, t)) => {
            mouse.column == *col
                && mouse.row == *row
                && t.elapsed().as_millis() < DOUBLE_CLICK_MS
        }
        None => false,
    }
}

/// Handle a left-click inside the raw (Object Source) pane.
///
/// Single click  — move the textarea cursor to the clicked position and sync
///                 the tree selection to that line.
/// Double-click  — additionally select the word under the cursor and enter
///                 Insert mode so the user can immediately type a replacement.
fn handle_mouse_raw_click(
    mouse: &MouseEvent,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
) {
    state.focus = Focus::RawPane;
    let double = is_double_click(mouse, &state.last_click);
    state.last_click = Some((mouse.column, mouse.row, Instant::now()));

    // Forward click so the textarea positions its cursor at the clicked cell.
    textarea.input(Event::Mouse(*mouse));

    if double {
        // Select the word under the cursor and drop into Insert mode so the
        // user can immediately type a replacement value.
        textarea.move_cursor(tui_textarea::CursorMove::WordBack);
        textarea.start_selection();
        textarea.move_cursor(tui_textarea::CursorMove::WordForward);
        state.mode = Mode::Insert;
    }

    // Always sync the tree to wherever the cursor landed.
    sync_tree_to_raw_cursor(state, textarea);
}

/// Handle a left-click inside the tree pane.
///
/// Single click  — select the row (and always toggle collapse when the ▶/▼
///                 indicator is clicked directly).
/// Double-click  — trigger the Enter action: edit the value of a leaf node,
///                 or toggle collapse of a container node.
fn handle_mouse_tree_click(
    mouse: &MouseEvent,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
) {
    state.focus = Focus::TreePane;

    // Row 0 is the top border; content starts at row 1.
    if mouse.row == 0 {
        return;
    }
    let content_row = (mouse.row - 1) as usize;
    let flat_idx = state.tree_scroll + content_row;
    let visible = state
        .tree
        .as_ref()
        .map(|t| t.visible_node_count())
        .unwrap_or(0);

    if flat_idx >= visible {
        return;
    }

    let double = is_double_click(mouse, &state.last_click);
    state.last_click = Some((mouse.column, mouse.row, Instant::now()));

    // Snapshot node metadata before any mutable borrow.
    let (node_depth, is_container) = state
        .tree
        .as_ref()
        .and_then(|t| t.flat_nodes().get(flat_idx))
        .map(|n| (n.depth, n.kind.is_container()))
        .unwrap_or((0, false));

    // Column within the tree content area (0-based, past the border).
    let content_col = (mouse.column - state.tree_x_start).saturating_sub(1) as usize;
    let indicator_start = node_depth * 2; // "  ".repeat(depth) takes depth*2 cols
    let clicked_indicator =
        is_container && content_col >= indicator_start && content_col <= indicator_start + 1;

    if clicked_indicator {
        // Clicking the ▶/▼ icon always toggles collapse immediately.
        if let Some(tree) = state.tree.as_mut() {
            tree.toggle_collapse(flat_idx);
        }
        state.tree_selected = flat_idx;
        state.clamp_tree_selection();
        state.ensure_visible();
    } else if double {
        // Double-click → enter action (edit leaf / toggle container).
        state.tree_selected = flat_idx;
        state.ensure_visible();
        tree_enter_action(state);
        // Sync raw pane to the now-selected node.
        sync_raw_cursor_to_selection(state, textarea);
    } else {
        // Single click → select only.
        state.tree_selected = flat_idx;
        state.ensure_visible();
    }
}

/// Seek both panes when the user clicks or drags the unified scrollbar strip.
///
/// The click position is converted to a 0.0–1.0 fraction and applied
/// proportionally to both the raw pane (cursor jump) and the tree pane
/// (selection + scroll).  This keeps both panes in sync regardless of
/// which one currently has focus.
fn handle_mouse_scrollbar_seek(
    mouse: &MouseEvent,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
) {
    let strip_h = state.tree_pane_height as usize;
    if strip_h == 0 || mouse.row == 0 {
        return;
    }
    let click_row = (mouse.row - 1) as usize;

    // Normalise click position to 0.0–1.0.
    let fraction = if strip_h <= 1 {
        0.0f64
    } else {
        (click_row as f64 / (strip_h - 1) as f64).clamp(0.0, 1.0)
    };

    // Seek raw pane cursor.
    let raw_total = textarea.lines().len().max(1);
    let raw_target = ((fraction * (raw_total - 1) as f64).round() as u16)
        .min((raw_total - 1) as u16);
    textarea.move_cursor(tui_textarea::CursorMove::Jump(raw_target, 0));

    // Seek tree pane selection.
    let tree_total = state
        .tree
        .as_ref()
        .map(|t| t.visible_node_count())
        .unwrap_or(0);
    if tree_total > 0 {
        let tree_target = ((fraction * (tree_total - 1) as f64).round() as usize)
            .min(tree_total - 1);
        state.tree_selected = tree_target;
        state.ensure_visible();
    }
}

/// Handle mouse events while the file-picker overlay is open.
///
/// * Click inside the list → select that entry; click on an already-selected
///   entry opens it (same as pressing Enter).
/// * Click outside the overlay → close the picker.
/// * Scroll wheel → navigate the list.
fn handle_mouse_file_picker(mouse: &MouseEvent, state: &mut AppState) -> Action {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let in_x = mouse.column >= state.picker_x
                && mouse.column < state.picker_x.saturating_add(state.picker_w);
            let in_y = mouse.row >= state.picker_y
                && (mouse.row as usize)
                    < state.picker_y as usize + state.picker_height as usize;

            if !(in_x && in_y) {
                // Click outside the picker → dismiss.
                state.mode = Mode::Normal;
                return Action::Continue;
            }

            let click_row = (mouse.row - state.picker_y) as usize;
            let (current_selected, current_scroll, entry_count) = match &state.mode {
                Mode::FilePicker {
                    selected,
                    scroll,
                    entries,
                    ..
                } => (*selected, *scroll, entries.len()),
                _ => return Action::Noop,
            };
            let target_idx = current_scroll + click_row;
            if target_idx >= entry_count {
                return Action::Continue;
            }

            if target_idx == current_selected {
                // Second click on already-highlighted entry → open it.
                picker_open_selected(state)
            } else {
                picker_set_selected(state, target_idx, current_scroll);
                Action::Continue
            }
        }
        MouseEventKind::ScrollDown => {
            let (selected, scroll, count) = match &state.mode {
                Mode::FilePicker {
                    selected,
                    scroll,
                    entries,
                    ..
                } => (*selected, *scroll, entries.len()),
                _ => return Action::Noop,
            };
            let new_sel = (selected + 1).min(count.saturating_sub(1));
            picker_set_selected(state, new_sel, scroll);
            Action::Continue
        }
        MouseEventKind::ScrollUp => {
            let (selected, scroll) = match &state.mode {
                Mode::FilePicker { selected, scroll, .. } => (*selected, *scroll),
                _ => return Action::Noop,
            };
            let new_sel = selected.saturating_sub(1);
            picker_set_selected(state, new_sel, scroll);
            Action::Continue
        }
        _ => Action::Noop,
    }
}

/// Open the currently highlighted file-picker entry.  Shared between the Enter
/// key handler and the double-click mouse handler.
fn picker_open_selected(state: &mut AppState) -> Action {
    let entry = match &state.mode {
        Mode::FilePicker { entries, selected, .. } => entries.get(*selected).cloned(),
        _ => return Action::Noop,
    };
    let Some(entry) = entry else {
        return Action::Continue;
    };

    if entry.is_dir {
        let new_entries = crate::state::read_dir_entries(&entry.path);
        state.mode = Mode::FilePicker {
            current_dir: entry.path,
            entries: new_entries,
            selected: 0,
            scroll: 0,
        };
        return Action::Continue;
    }

    // File selected — guard against unsaved changes.
    let path_str = entry.path.display().to_string();
    state.mode = Mode::Normal;
    if state.modified {
        state.mode = Mode::Confirm {
            question: format!(
                "Discard unsaved changes and open '{}'? [y/N]",
                entry.name
            ),
            action: crate::state::ConfirmAction::DiscardAndOpen { path: path_str },
        };
        return Action::Continue;
    }
    Action::OpenFile(entry.path)
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build the quit action, asking for confirmation if there are unsaved changes.
fn maybe_quit(state: &mut AppState) -> Action {
    if state.modified {
        state.mode = Mode::Confirm {
            question: "Unsaved changes. Quit anyway? [y/N]".to_string(),
            action: ConfirmAction::QuitUnsaved,
        };
        Action::Continue
    } else {
        Action::Quit
    }
}

// ── paste handlers ────────────────────────────────────────────────────────────
//
// Two entry points share one implementation (`insert_text`):
//
//   handle_paste   — called when the terminal sends a bracketed-paste sequence
//                    (Event::Paste).  Reliable on Linux/WSL; unreliable on some
//                    macOS terminal configs.
//
//   paste_from_clipboard — called by the Ctrl+V global shortcut.  Reads from
//                    the system clipboard via arboard.  Works on every platform
//                    regardless of terminal bracketed-paste support.

fn handle_paste(
    text: &str,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
) -> Action {
    insert_text(text, state, textarea)
}

/// Read the system clipboard and insert its text content at the cursor.
fn paste_from_clipboard(state: &mut AppState, textarea: &mut TextArea<'static>) -> Action {
    let text = match arboard::Clipboard::new() {
        Ok(mut cb) => cb.get_text().unwrap_or_default(),
        Err(_) => return Action::Continue,
    };
    insert_text(&text, state, textarea)
}

/// Try to parse `text` with `preferred` first, then fall back to every other
/// supported format.  Returns `(detected_format, parsed_value)` on success.
///
/// Preference order when the preferred format fails:
///   JSON → YAML → TOML → XML
/// (JSON is tried first because it is the most strict and least likely to
/// accept content that was meant for another format.)
fn detect_format_and_parse(
    text: &str,
    preferred: FileFormat,
) -> Option<(FileFormat, serde_json::Value)> {
    if let Ok(v) = preferred.parse(text) {
        return Some((preferred, v));
    }
    for fmt in [FileFormat::Json, FileFormat::Yaml, FileFormat::Toml, FileFormat::Xml] {
        if fmt == preferred {
            continue;
        }
        if let Ok(v) = fmt.parse(text) {
            return Some((fmt, v));
        }
    }
    None
}

/// Insert `text` at the current textarea cursor position.
///
/// This directly manipulates the line buffer rather than relying on
/// `textarea.input(Event::Paste(…))`, which requires the terminal to send
/// bracketed-paste sequences — something not all macOS setups do.
///
/// After insertion the full buffer is checked for valid content; if it parses
/// successfully the buffer is replaced with the pretty-printed form.  If the
/// pasted content is a different format than the current one (e.g. YAML pasted
/// into a JSON session), `state.format` is updated to match.
fn insert_text(
    text: &str,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
) -> Action {
    if text.is_empty() {
        return Action::Continue;
    }

    state.mode = Mode::Insert;
    state.focus = Focus::RawPane;
    state.push_undo();

    // Normalise line endings.
    let text = text.replace("\r\n", "\n").replace('\r', "\n");

    // Snapshot current content and cursor (character-based column).
    let (cursor_row, cursor_col) = textarea.cursor();
    let mut lines: Vec<String> = textarea.lines().iter().map(|l| l.to_string()).collect();
    if lines.is_empty() {
        lines.push(String::new());
    }
    let cursor_row = cursor_row.min(lines.len().saturating_sub(1));

    // Split the current line at the cursor (convert char-col → byte offset).
    let cur = lines[cursor_row].clone();
    let byte_off = cur
        .char_indices()
        .nth(cursor_col)
        .map(|(i, _)| i)
        .unwrap_or(cur.len());
    let before = &cur[..byte_off];
    let after = &cur[byte_off..];

    // Splice the pasted text into the line list.
    let parts: Vec<&str> = text.split('\n').collect();
    let mut new_lines: Vec<String> = lines[..cursor_row].to_vec();
    if parts.len() == 1 {
        new_lines.push(format!("{}{}{}", before, parts[0], after));
    } else {
        new_lines.push(format!("{}{}", before, parts[0]));
        for mid in &parts[1..parts.len() - 1] {
            new_lines.push(mid.to_string());
        }
        new_lines.push(format!("{}{}", parts[parts.len() - 1], after));
    }
    new_lines.extend_from_slice(&lines[cursor_row + 1..]);

    // Calculate where the cursor should end up.
    let new_row = (cursor_row + parts.len() - 1) as u16;
    let new_col = (if parts.len() == 1 {
        cursor_col + parts[0].chars().count()
    } else {
        parts[parts.len() - 1].chars().count()
    }) as u16;

    // Rebuild the textarea and restore cursor.
    let new_text = new_lines.join("\n");
    *textarea = TextArea::from(new_lines);
    textarea.move_cursor(tui_textarea::CursorMove::Jump(new_row, new_col));

    state.raw_text = new_text.clone();
    state.modified = true;

    // Auto-format if the full buffer now parses cleanly.
    // Try the current format first; if it fails, probe all other formats so
    // that pasting YAML (or TOML/XML) into a JSON session auto-switches the
    // format badge without requiring a file open.
    if let Some((detected, value)) = detect_format_and_parse(&new_text, state.format) {
        // Switch format if the pasted content required a different codec.
        state.format = detected;

        let formatted = state.format.serialize(&value);
        let collapsed = state
            .tree
            .as_ref()
            .map(|t| t.take_collapsed())
            .unwrap_or_default();
        // Always rebuild the textarea and tree from the formatted text so
        // indentation is canonical — even when `formatted == new_text`.
        let fmt_lines: Vec<String> = formatted.lines().map(String::from).collect();
        *textarea = TextArea::from(fmt_lines);
        state.raw_text = formatted.clone();
        // Mark the parse as clean so the debounce timer does not trigger a
        // redundant reparse (which could reset the format we just detected).
        state.parse_dirty = false;
        state.last_edit = None;
        match raw_to_tree(&formatted, collapsed, state.format) {
            ParseResult::Ok(tree) => {
                state.tree = Some(tree);
                state.parse_error = None;
            }
            ParseResult::Err(msg) => {
                state.parse_error = Some(msg);
            }
        }
        return Action::Continue;
    }

    // Not yet parseable — keep the buffer as-is and schedule a debounced reparse.
    state.parse_dirty = true;
    state.last_edit = Some(std::time::Instant::now());
    Action::Continue
}

/// Copy the value of the currently selected tree node to the clipboard.
fn copy_node_value(state: &AppState) {
    let Some(tree) = state.tree.as_ref() else {
        return;
    };
    let Some(node) = tree.flat_nodes().get(state.tree_selected) else {
        return;
    };
    let text = node.value_display();
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text);
    }
}

/// Silently move the raw-pane cursor to the line corresponding to the
/// currently selected tree node **without** changing focus.
///
/// Uses the cached `FlatNode::line_number` when available (O(1)); falls back
/// to a full `find_line_for_path` scan otherwise.
fn sync_raw_cursor_to_selection(state: &AppState, textarea: &mut TextArea<'static>) {
    let node = state
        .tree
        .as_ref()
        .and_then(|t| t.flat_nodes().get(state.tree_selected));
    let line = node
        .and_then(|n| n.line_number)
        .or_else(|| {
            node.map(|n| n.path.as_str())
                .and_then(|p| find_line_for_path(&state.raw_text, p, state.format))
        });
    if let Some(line) = line {
        textarea.move_cursor(tui_textarea::CursorMove::Jump(line as u16, 0));
    }
}

/// Silently sync the tree selection to whichever node owns the raw pane's
/// current cursor line **without** changing focus.
fn sync_tree_to_raw_cursor(state: &mut AppState, textarea: &TextArea<'static>) {
    let cursor_line = textarea.cursor().0;
    if let Some(idx) = state
        .tree
        .as_ref()
        .and_then(|t| t.find_node_at_line(cursor_line))
    {
        state.tree_selected = idx;
        state.ensure_visible();
    }
}

/// Jump the raw pane to the line corresponding to the currently selected
/// tree node, then switch focus there.  Called by the `g`/`l` keybinding.
fn jump_to_line_in_raw(state: &mut AppState, textarea: &mut TextArea<'static>) {
    let path = state
        .tree
        .as_ref()
        .and_then(|t| t.flat_nodes().get(state.tree_selected))
        .map(|n| n.path.clone());

    if let Some(path) = path {
        jump_to_path_in_raw(state, textarea, &path);
        state.focus = Focus::RawPane;
    }
}

/// Move the raw-pane textarea cursor to the line that corresponds to `path`.
///
/// Does NOT change the active focus pane — callers decide whether to switch.
fn jump_to_path_in_raw(
    state: &AppState,
    textarea: &mut TextArea<'static>,
    path: &str,
) {
    if let Some(line) = find_line_for_path(&state.raw_text, path, state.format) {
        textarea.move_cursor(tui_textarea::CursorMove::Jump(line as u16, 0));
    }
}

// ── save-as mode ──────────────────────────────────────────────────────────────

/// Handle key events while the user is typing a filename for `Ctrl+S` on a
/// new, unsaved buffer.
///
/// * Printable chars — append to buffer
/// * Backspace       — remove last char
/// * Enter           — confirm: set file path and trigger save
/// * Esc             — cancel: return to Normal without saving
fn handle_save_as(key: &KeyEvent, state: &mut AppState, mut buffer: String) -> Action {
    match key.code {
        KeyCode::Esc => {
            state.mode = Mode::Normal;
            Action::Continue
        }
        KeyCode::Backspace => {
            buffer.pop();
            state.mode = Mode::SaveAs { buffer };
            Action::Continue
        }
        KeyCode::Enter => {
            let name = buffer.trim().to_string();
            if name.is_empty() {
                return Action::Continue;
            }
            // Apply the extension for the active format if the user omitted it.
            let path = if std::path::Path::new(&name)
                .extension()
                .is_some()
            {
                std::path::PathBuf::from(name)
            } else {
                let ext = state.format.default_extension();
                std::path::PathBuf::from(format!("{}.{}", name, ext))
            };
            state.file_path = Some(path);
            state.mode = Mode::Normal;
            Action::Save
        }
        KeyCode::Char(c) if key.modifiers == KeyModifiers::NONE
            || key.modifiers == KeyModifiers::SHIFT =>
        {
            buffer.push(c);
            state.mode = Mode::SaveAs { buffer };
            Action::Continue
        }
        _ => Action::Noop,
    }
}

// ── jump to parse error ───────────────────────────────────────────────────────

/// Try to extract a 0-based line number from a parse-error string.
///
/// Handles our own error formats:
/// * JSON: `"JSON error at L:C — …"`      (L is 1-based)
/// * YAML/TOML/generic: `"… at line L …"` (L is 1-based)
pub fn extract_error_line(msg: &str) -> Option<usize> {
    // "JSON error at L:C — …"
    if let Some(rest) = msg.strip_prefix("JSON error at ") {
        if let Some(colon) = rest.find(':') {
            if let Ok(line) = rest[..colon].parse::<usize>() {
                return Some(line.saturating_sub(1));
            }
        }
    }
    // Generic "… at line N …" used by serde_yaml, toml, etc.
    for window in msg.split_whitespace().collect::<Vec<_>>().windows(3) {
        if window[0] == "at" && window[1] == "line" {
            let num = window[2].trim_end_matches(|c: char| !c.is_ascii_digit());
            if let Ok(line) = num.parse::<usize>() {
                return Some(line.saturating_sub(1));
            }
        }
    }
    None
}
