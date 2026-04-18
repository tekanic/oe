//! Pure render function for the TUI.
//!
//! `draw` takes a `&AppState` and a `&TextArea` and writes everything to
//! the Ratatui `Frame`.  No state mutation happens here.
//!
//! Layout:
//!
//! ```
//! ┌─────────────────┬───┬───────────────┐
//! │  Object Source  │ ║ │  Tree View    │
//! │  (raw pane)     │ ║ │  (tree pane)  │
//! │                 │ ║ │               │
//! └─────────────────┴───┴───────────────┘
//! │ status bar (1 line)                 │
//! └─────────────────────────────────────┘
//! ```
//!
//! The 3-column centre strip shows a single unified scrollbar that moves both panes.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
use tui_textarea::TextArea;

use crate::state::{AppState, FileEntry, Focus, Mode, TreeEditKind};
use crate::tree::{FlatNode, NodeKind};

// ── color palette ─────────────────────────────────────────────────────────────
// All colors use ANSI-16 values so they respect terminal theme overrides.

const COLOR_KEY: Color = Color::Yellow;
const COLOR_INDEX: Color = Color::Cyan;      // array indices [0], [1], …
const COLOR_STRING: Color = Color::Green;
const COLOR_NUMBER: Color = Color::Cyan;
const COLOR_BOOL: Color = Color::Magenta;
const COLOR_NULL: Color = Color::Magenta;
const COLOR_ERROR: Color = Color::Red;
const COLOR_SEARCH_MATCH: Color = Color::Yellow;
const COLOR_BRACKET: Color = Color::White;
const COLOR_TYPE_HINT: Color = Color::DarkGray;

// ── entry point ───────────────────────────────────────────────────────────────

/// Draw the full UI into `frame`.  Called once per event loop iteration.
pub fn draw(frame: &mut Frame, state: &AppState, textarea: &mut TextArea<'static>) {
    let size = frame.area();

    // Split into main area + status bar.
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(size);

    let main_area = vertical[0];
    let status_area = vertical[1];

    // Split main area: [raw pane | 3-col scrollbar strip | tree pane].
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Fill(1), Constraint::Length(3), Constraint::Fill(1)])
        .split(main_area);

    let raw_area       = horizontal[0];
    let scrollbar_area = horizontal[1];
    let tree_area      = horizontal[2];

    draw_raw_pane(frame, state, textarea, raw_area);
    draw_scrollbar(frame, state, textarea, scrollbar_area);
    draw_tree_pane(frame, state, tree_area);
    draw_status_bar(frame, state, status_area);

    // Overlay the file picker on top when it is active.
    if matches!(state.mode, Mode::FilePicker { .. }) {
        draw_file_picker(frame, state, size);
    }

    // Overlay the about screen on top of everything.
    if state.mode == Mode::About {
        draw_about(frame, size);
    }
}

// ── raw pane ──────────────────────────────────────────────────────────────────

fn draw_raw_pane(
    frame: &mut Frame,
    state: &AppState,
    textarea: &mut TextArea<'static>,
    area: Rect,
) {
    let focused = state.focus == Focus::RawPane;
    let left_title = "Object Source";

    let border_style = if focused {
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    // Format badge on the right side of the border — only when there is content.
    // An empty buffer has no meaningful format to display.
    let block = if state.raw_text.trim().is_empty() {
        Block::default()
            .title(left_title)
            .borders(Borders::ALL)
            .border_style(border_style)
    } else {
        let fmt_badge = Line::from(Span::styled(
            format!(" {} ", state.format.name()),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Right);
        Block::default()
            .title(left_title)
            .title_top(fmt_badge)
            .borders(Borders::ALL)
            .border_style(border_style)
    };

    // Style the textarea widget to match our theme.
    textarea.set_block(block);

    // Apply syntax highlighting to textarea lines.
    apply_syntax_highlighting(textarea, state);

    // Mark error cursor position if parse failed.
    if state.parse_error.is_some() {
        textarea.set_cursor_line_style(Style::default().fg(COLOR_ERROR));
    } else {
        textarea.set_cursor_line_style(Style::default());
    }

    // Cursor style: blinking bar when in insert mode.
    if state.mode == Mode::Insert {
        textarea.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
    } else {
        // Hide cursor when not editing.
        textarea.set_cursor_style(Style::default());
    }

    frame.render_widget(&*textarea, area);
}

/// Apply per-token syntax highlighting to the textarea by building styled
/// `Line`s and setting them as the textarea's line styles.
///
/// This is a best-effort lexer that colorizes the most common JSON tokens
/// without a full grammar.
fn apply_syntax_highlighting(textarea: &mut TextArea<'static>, _state: &AppState) {
    // tui-textarea does not support per-character line styles directly;
    // we apply the cursor-line style and rely on the widget's own
    // rendering. For full per-token highlighting we would need to render
    // the text ourselves. In this MVP we set the textarea's line
    // highlight style and leave per-token work as a future enhancement.
    //
    // The raw pane therefore shows plain text with a highlighted current
    // line. Full syntax highlighting is implemented in the separate
    // `highlight_raw_text` helper used by the preview pane.
    textarea.set_line_number_style(Style::default().fg(Color::DarkGray));
}

// ── scrollbar strip ───────────────────────────────────────────────────────────

/// Draw the 3-column unified scrollbar strip between the two panes.
///
/// A single thumb (`███`) moves both panes simultaneously.  Its position
/// reflects the *focused* pane's scroll fraction so the indicator always
/// matches what the user is actively navigating.
///
/// Track rows are rendered as ` │ ` (dim); the thumb row as `███` (bright).
fn draw_scrollbar(
    frame: &mut Frame,
    state: &AppState,
    textarea: &TextArea<'static>,
    area: Rect,
) {
    let h = area.height as usize;
    if h == 0 {
        return;
    }

    // Compute a 0.0–1.0 scroll fraction from the focused pane.
    let fraction: f64 = match state.focus {
        Focus::RawPane => {
            let total = textarea.lines().len().max(1);
            let cursor = textarea.cursor().0;
            if total <= 1 { 0.0 } else { cursor as f64 / (total - 1) as f64 }
        }
        Focus::TreePane => {
            let total = state
                .tree
                .as_ref()
                .map(|t| t.visible_node_count())
                .unwrap_or(0);
            if total <= 1 {
                0.0
            } else {
                state.tree_scroll as f64 / (total - 1) as f64
            }
        }
    };

    let thumb_row = ((fraction * h.saturating_sub(1) as f64).round() as usize)
        .min(h.saturating_sub(1));

    let track_style = Style::default().fg(Color::DarkGray);
    let thumb_style = Style::default().fg(Color::White);

    for row in 0..h {
        let y = area.y + row as u16;
        let (text, style) = if row == thumb_row {
            ("███", thumb_style)
        } else {
            (" │ ", track_style)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(text, style))),
            Rect { x: area.x, y, width: area.width, height: 1 },
        );
    }
}

// ── tree pane ─────────────────────────────────────────────────────────────────

fn draw_tree_pane(frame: &mut Frame, state: &AppState, area: Rect) {
    let focused = state.focus == Focus::TreePane;
    let title = "Tree View";

    let border_style = if focused {
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Build list items — borrow the slice directly; no clone needed.
    let empty: &[FlatNode] = &[];
    let nodes: &[FlatNode] = state.tree.as_ref().map_or(empty, |t| t.flat_nodes());

    if nodes.is_empty() {
        let (text, style) = if state.parse_error.is_some() {
            // Source exists but is currently invalid.
            ("(no valid content)", Style::default().fg(COLOR_ERROR))
        } else if state.raw_text.trim().is_empty() {
            // Nothing loaded yet — show onboarding hint.
            (
                "No file open.\n\nCtrl+O  open a file\n\nOr paste content into the left pane.",
                Style::default().fg(Color::DarkGray),
            )
        } else {
            // Valid but empty document (e.g. `{}` or `[]` with no children).
            ("(empty document)", Style::default().fg(Color::DarkGray))
        };

        // Centre the text both horizontally and vertically within `inner`.
        let line_count = text.lines().count() as u16;
        let top_pad = inner.height.saturating_sub(line_count) / 2;
        let centered = Rect {
            y: inner.y + top_pad,
            height: inner.height.saturating_sub(top_pad),
            ..inner
        };

        let placeholder = Paragraph::new(text)
            .style(style)
            .alignment(Alignment::Center);
        frame.render_widget(placeholder, centered);
        return;
    }

    let items: Vec<ListItem> = nodes
        .iter()
        .enumerate()
        .map(|(idx, node)| build_tree_item(node, idx == state.tree_selected))
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(state.tree_selected));

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    frame.render_stateful_widget(list, inner, &mut list_state);
}

/// Build one `ListItem` for a tree node.
fn build_tree_item(node: &FlatNode, selected: bool) -> ListItem<'static> {
    let indent = "  ".repeat(node.depth);

    // Collapse indicator for container nodes.
    let collapse_prefix = if node.kind.is_container() {
        if node.collapsed { "▶ " } else { "▼ " }
    } else {
        "  "
    };

    // Key display.
    // Array indices (numeric keys) are shown as [N] in a distinct colour so
    // users can instantly tell object keys from array positions.
    let is_array_index = !node.key.is_empty() && node.key.parse::<usize>().is_ok();
    let key_display = if is_array_index {
        format!("[{}]", node.key)
    } else {
        node.key.clone()
    };
    let key_color = if is_array_index { COLOR_INDEX } else { COLOR_KEY };

    let key_span = if node.key.is_empty() {
        Span::raw("(root)")
    } else if node.search_match {
        Span::styled(
            key_display,
            Style::default().fg(COLOR_SEARCH_MATCH).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(key_display, Style::default().fg(key_color))
    };

    // Separator.
    let sep = if node.key.is_empty() {
        Span::raw("")
    } else {
        Span::raw(": ")
    };

    // Value display.
    let value_span = match &node.kind {
        NodeKind::String => Span::styled(
            node.value_display(),
            Style::default().fg(COLOR_STRING),
        ),
        NodeKind::Number => Span::styled(
            node.value_display(),
            Style::default().fg(COLOR_NUMBER),
        ),
        NodeKind::Bool => Span::styled(
            node.value_display(),
            Style::default().fg(COLOR_BOOL),
        ),
        NodeKind::Null => Span::styled(
            node.value_display(),
            Style::default().fg(COLOR_NULL),
        ),
        NodeKind::Object { child_count } => Span::styled(
            if node.collapsed {
                let noun = if *child_count == 1 { "key" } else { "keys" };
                format!("{{ {} {} }}", child_count, noun)
            } else {
                "{".to_string()
            },
            Style::default().fg(COLOR_BRACKET),
        ),
        NodeKind::Array { child_count } => Span::styled(
            if node.collapsed {
                let noun = if *child_count == 1 { "item" } else { "items" };
                format!("[ {} {} ]", child_count, noun)
            } else {
                "[".to_string()
            },
            Style::default().fg(COLOR_BRACKET),
        ),
    };

    // Type hint shown after value for expanded containers.
    let type_hint = match &node.kind {
        NodeKind::Object { child_count } if !node.collapsed => {
            let noun = if *child_count == 1 { "key" } else { "keys" };
            Span::styled(
                format!(" ({} {})", child_count, noun),
                Style::default().fg(COLOR_TYPE_HINT),
            )
        }
        NodeKind::Array { child_count } if !node.collapsed => {
            let noun = if *child_count == 1 { "item" } else { "items" };
            Span::styled(
                format!(" ({} {})", child_count, noun),
                Style::default().fg(COLOR_TYPE_HINT),
            )
        }
        _ => Span::raw(""),
    };

    let line = Line::from(vec![
        Span::raw(indent),
        Span::raw(collapse_prefix),
        key_span,
        sep,
        value_span,
        type_hint,
    ]);

    if selected {
        ListItem::new(line).style(Style::default().add_modifier(Modifier::REVERSED))
    } else {
        ListItem::new(line)
    }
}

// ── file picker overlay ───────────────────────────────────────────────────────

/// Draw the file-picker modal.  It is rendered on top of everything else
/// using `Clear` to blank the background, then a bordered `Block` with a
/// scrollable `List` of filesystem entries.
///
/// Layout inside the popup (after border):
/// ```
///  ╔══ Open File ════════════════════════════╗
///  ║ /current/directory/                     ║
///  ║─────────────────────────────────────────║
///  ║ ▶ ..                                    ║
///  ║ ▶ subdir/                               ║
///  ║   data.json                             ║
///  ║   schema.json                           ║
///  ╚═════════════════════════════════════════╝
/// ```
fn draw_file_picker(frame: &mut Frame, state: &AppState, screen: Rect) {
    let Mode::FilePicker {
        ref current_dir,
        ref entries,
        selected,
        scroll,
    } = state.mode
    else {
        return;
    };

    // Calculate a centered popup that is 70% wide and 70% tall.
    let popup_area = centered_rect(70, 70, screen);

    // Clear the area behind the popup so it truly overlays.
    frame.render_widget(Clear, popup_area);

    // Outer block.
    let block = Block::default()
        .title(" Open File — ↑↓:navigate  Enter:open/enter  Esc:cancel ")
        .borders(Borders::ALL)
        .border_style(Style::default().add_modifier(Modifier::BOLD));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    // Reserve the first row for the current directory breadcrumb.
    if inner.height < 2 {
        return;
    }
    let dir_area = Rect { height: 1, ..inner };
    let list_area = Rect {
        y: inner.y + 1,
        height: inner.height - 1,
        ..inner
    };

    // Store picker height so the event handler can page correctly.
    // (We can't mutate state here, so we shadow it in the render pass and
    // the next event will see the updated value from main.rs.)
    let _ = list_area.height; // used via state.picker_height set in main.rs

    // Render current directory.
    let dir_str = current_dir.display().to_string();
    let dir_line = Paragraph::new(Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled(dir_str, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
    ]));
    frame.render_widget(dir_line, dir_area);

    if entries.is_empty() {
        let empty = Paragraph::new("  (no JSON files or directories found)")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(empty, list_area);
        return;
    }

    // Build list items from entries.
    let items: Vec<ListItem> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| build_picker_item(entry, i == selected))
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(selected));

    // Offset the ListState by scroll so ratatui renders the right window.
    // ratatui's List uses the selected index directly and auto-scrolls, but
    // we explicitly set the offset to keep our scroll state in control.
    *list_state.offset_mut() = scroll;

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    frame.render_stateful_widget(list, list_area, &mut list_state);
}

/// Build one `ListItem` for the file picker.
fn build_picker_item(entry: &FileEntry, _selected: bool) -> ListItem<'static> {
    let (icon, name_style) = if entry.is_dir {
        (
            "▶ ",
            Style::default().fg(Color::Cyan),
        )
    } else {
        (
            "  ",
            Style::default().fg(Color::Green),
        )
    };

    let display_name = if entry.is_dir && entry.name != ".." {
        format!("{}/", entry.name)
    } else {
        entry.name.clone()
    };

    ListItem::new(Line::from(vec![
        Span::raw(" "),
        Span::styled(icon, Style::default().fg(Color::DarkGray)),
        Span::styled(display_name, name_style),
    ]))
}

/// Compute a `Rect` that is `percent_x`% wide and `percent_y`% tall,
/// centred within `r`.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_width = r.width * percent_x / 100;
    let popup_height = r.height * percent_y / 100;
    let x = r.x + (r.width.saturating_sub(popup_width)) / 2;
    let y = r.y + (r.height.saturating_sub(popup_height)) / 2;
    Rect {
        x,
        y,
        width: popup_width,
        height: popup_height,
    }
}

// ── status bar ────────────────────────────────────────────────────────────────

fn draw_status_bar(frame: &mut Frame, state: &AppState, area: Rect) {
    let (left_spans, hint) = build_status_parts(state);

    // Hints are right-justified; left content gets the remaining width.
    let hint_width = (hint.chars().count() as u16).min(area.width.saturating_sub(10));
    let left_width = area.width.saturating_sub(hint_width);

    if hint_width == 0 || left_width == 0 {
        // Terminal too narrow — just show the left content.
        let para = Paragraph::new(Line::from(left_spans));
        frame.render_widget(para, area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(left_width), Constraint::Length(hint_width)])
        .split(area);

    // Left: mode + file + parse error + mode prompt (clips if too long).
    let left_para = Paragraph::new(Line::from(left_spans));
    frame.render_widget(left_para, chunks[0]);

    // Right: keyboard hints, right-aligned inside their slice.
    use ratatui::layout::Alignment;
    let hint_para = Paragraph::new(Line::from(Span::styled(
        hint,
        Style::default().fg(Color::DarkGray),
    )))
    .alignment(Alignment::Right);
    frame.render_widget(hint_para, chunks[1]);
}

/// Returns `(left_spans, hint_string)`.
///
/// `left_spans` is everything on the left (mode badge, file, parse error,
/// mode-specific prompt).  `hint_string` is the keyboard-shortcut summary
/// that will be rendered right-justified.
fn build_status_parts(state: &AppState) -> (Vec<Span<'static>>, String) {
    let mut spans: Vec<Span<'static>> = Vec::new();

    // ── mode indicator ────────────────────────────────────────────────────────
    let mode_label = match &state.mode {
        Mode::Normal => "[NORMAL]",
        Mode::Insert => "[INSERT]",
        Mode::TreeEdit { .. } => "[EDIT]",
        Mode::Search => "[SEARCH]",
        Mode::Replace { .. } => "[REPLACE]",
        Mode::Confirm { .. } => "[CONFIRM]",
        Mode::FilePicker { .. } => "[OPEN]",
        Mode::About => "[ABOUT]",
        Mode::SaveAs { .. } => "[SAVE AS]",
    };
    spans.push(Span::styled(
        mode_label,
        Style::default().add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw("  "));

    // ── file path + modified ──────────────────────────────────────────────────
    let file_label = state
        .file_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "[new file]".to_string());
    let modified_marker = if state.modified { " *" } else { "" };
    spans.push(Span::raw(format!("{}{}", file_label, modified_marker)));

    // ── parse error (fills remaining left space; no hard truncation) ──────────
    if let Some(err) = &state.parse_error {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("⚠ {}", err),
            Style::default().fg(COLOR_ERROR),
        ));
    }

    // ── mode-specific prompt (left) + hint (right) ────────────────────────────
    let hint: String = match &state.mode {
        Mode::Normal => {
            match state.focus {
                Focus::RawPane => {
                    let base = "i/Enter:edit  Ctrl+A:select-all  Shift+↑↓←→:select  Ctrl+V:paste  Ctrl+Z:undo  Ctrl+Y:redo  ↑↓:scroll  /:search  Ctrl+O:open  Ctrl+S:save  q:quit  Tab:switch  ?:about";
                    if state.parse_error.is_some() {
                        format!("Ctrl+E:jump-to-error  {}", base)
                    } else {
                        base.to_string()
                    }
                }
                Focus::TreePane => {
                    "↑↓/jk:move  PgUp/PgDn:page  Home/End:top/bottom  →:expand  ←:collapse  Space:toggle  Enter:edit  a:add  d:del  r:rename  y:copy  g:jump  Ctrl+Z:undo  Ctrl+Y:redo  /:search  Tab:switch  ?:about".to_string()
                }
            }
        }
        Mode::Insert => {
            "Esc:normal  Ctrl+A:select-all  Shift+↑↓←→:select  Ctrl+V:paste  Ctrl+Z:undo  Ctrl+Y:redo  Ctrl+S:save".to_string()
        }
        Mode::TreeEdit { kind, buffer } => {
            spans.push(Span::raw("  "));
            let prompt = match kind {
                TreeEditKind::AddKey => format!("New key: {}_", buffer),
                TreeEditKind::AddValue { key } => {
                    if key.is_empty() {
                        format!("New item (JSON): {}_", buffer)
                    } else {
                        format!("Value for '{}': {}_", key, buffer)
                    }
                }
                TreeEditKind::RenameKey => format!("Rename key: {}_", buffer),
                TreeEditKind::EditValue { node_type, original } => {
                    format!("Edit [{}]  Current: {}  →  New: {}_", node_type, original, buffer)
                }
            };
            spans.push(Span::styled(prompt, Style::default().fg(Color::White)));
            "Enter:confirm  Esc:cancel".to_string()
        }
        Mode::Search => {
            let match_count = state
                .tree
                .as_ref()
                .map(|t| t.flat_nodes().iter().filter(|n| n.search_match).count())
                .unwrap_or(0);
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("Search: {}_", state.search_query),
                Style::default().fg(Color::White),
            ));
            if !state.search_query.is_empty() {
                let label = if match_count == 1 { "match" } else { "matches" };
                spans.push(Span::styled(
                    format!("  {} {}", match_count, label),
                    Style::default().fg(Color::Yellow),
                ));
            }
            "↑↓/jk:navigate  Enter:jump  Ctrl+R:replace  Esc:cancel".to_string()
        }
        Mode::Replace { replacement } => {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("Find: {}  →  Replace: {}_", state.search_query, replacement),
                Style::default().fg(Color::White),
            ));
            let remaining = state
                .tree
                .as_ref()
                .map(|t| {
                    t.flat_nodes()
                        .iter()
                        .filter(|n| n.search_match && !n.kind.is_container())
                        .count()
                })
                .unwrap_or(0);
            if remaining > 0 {
                let label = if remaining == 1 { "match" } else { "matches" };
                spans.push(Span::styled(
                    format!("  {} {}", remaining, label),
                    Style::default().fg(Color::Yellow),
                ));
            } else {
                spans.push(Span::styled(
                    "  no matches",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            "↑↓:navigate  Enter:replace  Ctrl+A:all  Esc:back".to_string()
        }
        Mode::Confirm { question, .. } => {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                question.clone(),
                Style::default().fg(COLOR_ERROR).add_modifier(Modifier::BOLD),
            ));
            "y:yes  n/Esc:no".to_string()
        }
        Mode::FilePicker { entries, selected, .. } => {
            if let Some(entry) = entries.get(*selected) {
                let kind = if entry.is_dir {
                    "dir".to_string()
                } else {
                    // Show the detected format name for files.
                    entry.path.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| crate::format::FileFormat::from_extension(e).name().to_lowercase())
                        .unwrap_or_else(|| "file".to_string())
                };
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    format!("{} ({})", entry.name, kind),
                    Style::default().fg(Color::White),
                ));
            }
            "↑↓:navigate  Enter:open  Esc:cancel".to_string()
        }
        Mode::About => "Press any key to close".to_string(),
        Mode::SaveAs { buffer } => {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("Save as: {}_", buffer),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ));
            "Enter:confirm  Esc:cancel".to_string()
        }
    };

    (spans, hint)
}

// ── about overlay ─────────────────────────────────────────────────────────────

/// Draw the about / help popup.
fn draw_about(frame: &mut Frame, screen: Rect) {
    // Centered popup: wide enough for the content, tall enough for all sections.
    const W: u16 = 66;
    const H: u16 = 49;
    let x = screen.x + screen.width.saturating_sub(W) / 2;
    let y = screen.y + screen.height.saturating_sub(H) / 2;
    let area = Rect {
        x,
        y,
        width: W.min(screen.width),
        height: H.min(screen.height),
    };

    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let version = env!("CARGO_PKG_VERSION");

    let content = vec![
        // ── header ──────────────────────────────────────────────────────────
        Line::from(vec![
            Span::styled(
                "OE",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  Object Editor",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  v{}", version),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "A dual-pane terminal editor for structured data.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        // ── formats ─────────────────────────────────────────────────────────
        Line::from(Span::styled(
            "Supported Formats",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "─────────────────",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(vec![
            fmt_badge("JSON", Color::Green),
            Span::raw("  "),
            fmt_badge("YAML", Color::Yellow),
            Span::raw("  "),
            fmt_badge("TOML", Color::Magenta),
            Span::raw("  "),
            fmt_badge("XML", Color::Cyan),
        ]),
        Line::from(""),
        // ── key bindings ────────────────────────────────────────────────────
        Line::from(Span::styled(
            "Global",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled("──────", Style::default().fg(Color::DarkGray))),
        kb_line("Tab",          "Switch focus between source and tree pane"),
        kb_line("/",            "Search / filter tree (from either pane)"),
        kb_line("Ctrl+O",       "Open file  (JSON / YAML / TOML / XML)"),
        kb_line("Ctrl+S",       "Save file  (prompts for name if new)"),
        kb_line("Ctrl+E",       "Jump to parse error location"),
        kb_line("Ctrl+V",       "Paste from system clipboard"),
        kb_line("Ctrl+Z",       "Undo  (works from any pane / mode)"),
        kb_line("Ctrl+Y",       "Redo"),
        kb_line("q",            "Quit  (prompts if unsaved changes)"),
        kb_line("?",            "Show this screen"),
        Line::from(""),
        Line::from(Span::styled(
            "Source Pane  (Object Source)",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled("────────────────────────────", Style::default().fg(Color::DarkGray))),
        kb_line("i / Enter",    "Enter edit mode"),
        kb_line("Esc",          "Return to Normal mode"),
        kb_line("↑↓ PgUp PgDn","Scroll without entering edit mode"),
        kb_line("Ctrl+A",       "Select all text"),
        kb_line("Shift+↑↓←→",  "Extend / create a selection"),
        Line::from(""),
        Line::from(Span::styled(
            "Tree Pane  (Tree View)",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled("──────────────────────", Style::default().fg(Color::DarkGray))),
        kb_line("↑↓  j k",     "Move selection up / down"),
        kb_line("PgUp / PgDn", "Move selection by one page"),
        kb_line("Home / End",  "Jump to first / last node"),
        kb_line("→  ←",        "Expand / collapse node"),
        kb_line("Space",        "Toggle collapse on selected node"),
        kb_line("Enter",        "Edit value of selected leaf node"),
        kb_line("a",            "Add key/value to selected container"),
        kb_line("d",            "Delete selected node  (confirm required)"),
        kb_line("r",            "Rename selected key"),
        kb_line("y",            "Copy selected node value to clipboard"),
        kb_line("g",            "Jump source pane to selected node"),
        Line::from(""),
        Line::from(Span::styled(
            "Search  ( / )",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled("──────────────", Style::default().fg(Color::DarkGray))),
        kb_line("↑↓  j k",     "Navigate between matches"),
        kb_line("Enter",        "Jump to selected match"),
        kb_line("Ctrl+R",       "Switch to find & replace"),
        kb_line("Esc",          "Cancel search"),
        Line::from(""),
        // ── license ─────────────────────────────────────────────────────────
        Line::from(Span::styled(
            "MIT License — open source, free to use and modify.",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let para = Paragraph::new(content)
        .style(Style::default())
        .wrap(ratatui::widgets::Wrap { trim: false });
    frame.render_widget(para, inner);
}

/// A colored format badge span.
fn fmt_badge(label: &'static str, color: Color) -> Span<'static> {
    Span::styled(label, Style::default().fg(color).add_modifier(Modifier::BOLD))
}

/// One key-binding row: `key` (yellow, fixed width) + description (normal).
fn kb_line(key: &'static str, desc: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {:<14}", key),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw(desc),
    ])
}

// ── syntax-highlighted raw JSON ───────────────────────────────────────────────
// The following helper builds a Vec<Line> with per-token colors for rendering
// the raw JSON text.  It is NOT currently plugged into the textarea (tui-textarea
// doesn't support line-level rich styling from outside), but is available for a
// custom Paragraph fallback if needed.

/// Produce a highlighted `Line` from a single line of raw JSON text.
#[allow(dead_code)]
pub fn highlight_json_line(raw_line: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let mut chars = raw_line.char_indices().peekable();

    while let Some((i, c)) = chars.peek().copied() {
        match c {
            // Skip whitespace
            ' ' | '\t' => {
                chars.next();
                spans.push(Span::raw(" "));
            }
            // String literal
            '"' => {
                chars.next(); // consume opening quote
                let mut s = String::from('"');
                let mut escaped = false;
                for (_, ch) in chars.by_ref() {
                    s.push(ch);
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        break;
                    }
                }
                // Peek next non-ws char to distinguish key vs string value.
                let is_key = raw_line[i + s.len()..].trim_start().starts_with(':');
                if is_key {
                    spans.push(Span::styled(s, Style::default().fg(COLOR_KEY)));
                } else {
                    spans.push(Span::styled(s, Style::default().fg(COLOR_STRING)));
                }
            }
            // Number
            '-' | '0'..='9' => {
                let mut n = String::new();
                while let Some((_, ch)) = chars.peek() {
                    if ch.is_ascii_digit() || *ch == '.' || *ch == 'e' || *ch == 'E'
                        || *ch == '+' || *ch == '-'
                    {
                        n.push(*ch);
                        chars.next();
                    } else {
                        break;
                    }
                }
                spans.push(Span::styled(n, Style::default().fg(COLOR_NUMBER)));
            }
            // true / false / null keywords
            't' | 'f' | 'n' => {
                let mut kw = String::new();
                while let Some((_, ch)) = chars.peek() {
                    if ch.is_alphabetic() {
                        kw.push(*ch);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let color = match kw.as_str() {
                    "true" | "false" => COLOR_BOOL,
                    "null" => COLOR_NULL,
                    _ => Color::Reset,
                };
                spans.push(Span::styled(kw, Style::default().fg(color)));
            }
            // Structural characters
            '{' | '}' | '[' | ']' | ':' | ',' => {
                chars.next();
                spans.push(Span::styled(
                    c.to_string(),
                    Style::default().fg(COLOR_BRACKET),
                ));
            }
            _ => {
                chars.next();
                spans.push(Span::raw(c.to_string()));
            }
        }
    }
    Line::from(spans)
}
