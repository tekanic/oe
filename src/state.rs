//! Application state: all mutable data that drives the TUI.
//!
//! This module owns `AppState` (the single source of truth) and the
//! `Mode` enum that controls which input handler is active. Rendering
//! and event-handling modules borrow `AppState` immutably or mutably as
//! needed, but never store their own copies.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use crate::format::FileFormat;
use crate::tree::JsonTree;

// ── undo / redo ──────────────────────────────────────────────────────────────

/// Maximum number of snapshots kept in the undo stack.
pub const UNDO_LIMIT: usize = 100;

/// A single undo/redo entry is just the raw text at that moment.
/// Simple string-snapshot strategy is sufficient for an MVP editor.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub text: String,
    /// Cursor row in the textarea at the time of the snapshot (reserved for future use).
    #[allow(dead_code)]
    pub cursor_row: usize,
    /// Cursor col in the textarea at the time of the snapshot (reserved for future use).
    #[allow(dead_code)]
    pub cursor_col: usize,
}

/// Manages an undo/redo history for the raw text buffer.
///
/// Uses `VecDeque` so that evicting the oldest entry (when the stack is full)
/// is O(1) instead of the O(N) shift that `Vec::remove(0)` would require.
#[derive(Debug, Default)]
pub struct UndoStack {
    /// Past states; the back of the deque is the most-recent past.
    past: VecDeque<Snapshot>,
    /// Future states (populated when the user undoes).
    future: VecDeque<Snapshot>,
}

impl UndoStack {
    /// Record a new state, clearing the redo future.
    pub fn push(&mut self, snap: Snapshot) {
        self.past.push_back(snap);
        if self.past.len() > UNDO_LIMIT {
            self.past.pop_front(); // O(1) with VecDeque
        }
        self.future.clear();
    }

    /// Pop the most-recent past state and return it, pushing the
    /// current state onto the redo stack.
    pub fn undo(&mut self, current: Snapshot) -> Option<Snapshot> {
        let prev = self.past.pop_back()?;
        self.future.push_back(current);
        Some(prev)
    }

    /// Pop the most-recent future state and return it, pushing the
    /// current state onto the undo stack.
    pub fn redo(&mut self, current: Snapshot) -> Option<Snapshot> {
        let next = self.future.pop_back()?;
        self.past.push_back(current);
        Some(next)
    }

    #[allow(dead_code)]
    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    #[allow(dead_code)]
    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }
}

// ── mode ─────────────────────────────────────────────────────────────────────

/// Which interaction mode the application is in.
///
/// The mode gates which key-bindings are active and what the status
/// bar renders.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Mode {
    /// Normal browsing — both panes are navigable but not being edited.
    #[default]
    Normal,
    /// The raw-JSON left pane has keyboard focus and text is being typed.
    Insert,
    /// A tree-pane inline edit is in progress (add/rename/edit value).
    TreeEdit {
        /// What kind of edit is happening.
        kind: TreeEditKind,
        /// Text the user has typed so far.
        buffer: String,
    },
    /// The search / filter bar is open at the bottom.
    Search,
    /// A yes/no confirmation prompt is active (e.g. delete node, quit).
    Confirm {
        /// Human-readable question shown in the status bar.
        question: String,
        /// What to do when the user answers.
        action: ConfirmAction,
    },
    /// A find-and-replace operation is in progress.
    ///
    /// The tree filter (from the preceding Search mode) remains active so
    /// the user can see and navigate the current matches while typing the
    /// replacement.  The query lives in `AppState::search_query`.
    Replace {
        /// The replacement value the user is typing.
        replacement: String,
    },
    /// A file-picker overlay is open; the user is browsing the filesystem
    /// to choose a file to open.
    FilePicker {
        /// The directory currently displayed in the picker.
        current_dir: PathBuf,
        /// Sorted list of entries visible in the picker.
        entries: Vec<FileEntry>,
        /// Which entry is highlighted (0-based index into `entries`).
        selected: usize,
        /// Scroll offset (row of `entries` at the top of the visible area).
        scroll: usize,
    },
    /// The about / help overlay is visible.
    About,
    /// The user is typing a filename for a first-time save.
    SaveAs {
        /// Filename the user is typing.
        buffer: String,
    },
}

/// One row in the file-picker overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Display name (e.g. `"data.json"` or `"subdir"`).
    pub name: String,
    /// `true` for directories, `false` for files.
    pub is_dir: bool,
    /// Absolute path to the entry.
    pub path: PathBuf,
}

/// Specifies what kind of tree inline-edit is in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeEditKind {
    /// Adding a new key to an object: prompt for the key name first.
    AddKey,
    /// Adding a value after the key was entered (or directly for array items).
    /// `key` is empty when adding to an array.
    AddValue { key: String },
    /// Renaming an existing object key.
    RenameKey,
    /// Editing the value of a leaf node.
    /// `node_type` is the current JSON type name ("string", "number", …)
    /// shown as a hint in the status bar prompt.
    /// `original` is the pre-edit display value shown as a read-only label
    /// so the user can see what they are replacing.
    EditValue { node_type: String, original: String },
}

/// What to do when a `Confirm` prompt resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    /// Delete the currently selected tree node.
    DeleteNode,
    /// Quit the application even though there are unsaved changes.
    QuitUnsaved,
    /// Save to a path entered by the user (no-path first-save).
    #[allow(dead_code)]
    SaveAs { path: String },
    /// Discard unsaved changes and open the given file.
    DiscardAndOpen { path: String },
}

// ── file picker helpers ───────────────────────────────────────────────────────

/// Read `dir` and return a sorted list of `FileEntry` items to show in the
/// file-picker overlay.
///
/// Rules:
/// * The first entry is always `..` (parent), except at the filesystem root.
/// * Directories come next, sorted case-insensitively.
/// * Only `.json` files are listed (hidden files excluded).
/// * Files are sorted case-insensitively after directories.
pub fn read_dir_entries(dir: &std::path::Path) -> Vec<FileEntry> {
    let mut dirs: Vec<FileEntry> = Vec::new();
    let mut files: Vec<FileEntry> = Vec::new();

    // Parent entry.
    let mut entries = Vec::new();
    if let Some(parent) = dir.parent() {
        entries.push(FileEntry {
            name: "..".to_string(),
            is_dir: true,
            path: parent.to_path_buf(),
        });
    }

    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // Skip hidden entries.
            if name.starts_with('.') {
                continue;
            }
            let Ok(file_type) = entry.file_type() else { continue };
            let path = entry.path();
            if file_type.is_dir() {
                dirs.push(FileEntry { name, is_dir: true, path });
            } else if file_type.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(FileFormat::is_supported_extension)
                    .unwrap_or(false)
            {
                files.push(FileEntry { name, is_dir: false, path });
            }
        }
    }

    dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    entries.extend(dirs);
    entries.extend(files);
    entries
}

// ── focus ─────────────────────────────────────────────────────────────────────

/// Which pane currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    RawPane,
    TreePane,
}

// ── app state ─────────────────────────────────────────────────────────────────

/// Central application state. Passed by `&mut` to event handlers and
/// by `&` to the render function.
pub struct AppState {
    // ── file ──
    /// Path to the file currently open, if any.
    pub file_path: Option<PathBuf>,
    /// Whether the buffer differs from the on-disk content.
    pub modified: bool,
    /// The format of the currently open file.
    pub format: FileFormat,

    // ── raw pane ──
    /// Current raw text in the editor (the textarea owns its own cursor;
    /// this field is a mirror kept in sync for operations that need the
    /// full string outside of the widget, e.g. undo snapshots).
    pub raw_text: String,
    /// Undo/redo history for the raw text buffer.
    pub undo_stack: UndoStack,

    // ── tree pane ──
    /// The parsed JSON tree (always reflects the *last successful* parse).
    pub tree: Option<JsonTree>,
    /// Index of the currently selected tree node (flat list index).
    pub tree_selected: usize,
    /// How many rows the tree view has been scrolled by.
    pub tree_scroll: usize,
    /// Free-text search / filter query. Empty string means no filter.
    pub search_query: String,

    // ── status / errors ──
    /// The most-recent JSON parse error, if any.
    pub parse_error: Option<String>,

    // ── interaction ──
    /// Current interaction mode.
    pub mode: Mode,
    /// Which pane has keyboard focus.
    pub focus: Focus,

    // ── layout cache (updated each render frame) ──
    /// Height of the tree pane in rows (set by the render pass).
    pub tree_pane_height: u16,
    /// Height of the file-picker list area in rows (set by the render pass).
    pub picker_height: u16,
    /// X column where the file-picker list area begins (set by the render pass).
    pub picker_x: u16,
    /// Y row where the file-picker list area begins (set by the render pass).
    pub picker_y: u16,
    /// Width of the file-picker list area (set by the render pass).
    pub picker_w: u16,
    /// X column where the tree pane begins.
    /// Set by the render pass; used by mouse-click hit-testing.
    pub tree_x_start: u16,
    /// X column where the scrollbar strip begins (tree_x_start - 3).
    /// Set by the render pass; used by mouse-click hit-testing.
    pub scrollbar_x_start: u16,

    // ── debounced parse ───────────────────────────────────────────────────────
    /// Set to `true` when the raw text has been edited but the tree has not
    /// yet been re-parsed (i.e. a parse is pending after the debounce delay).
    pub parse_dirty: bool,
    /// Timestamp of the most-recent raw-text edit.  The event loop re-parses
    /// once `last_edit.elapsed() >= PARSE_DEBOUNCE`.
    pub last_edit: Option<Instant>,

    // ── double-click detection ────────────────────────────────────────────────
    /// Screen position and timestamp of the most recent left mouse-button press.
    /// Used to detect double-clicks: two presses at the same (col, row) within
    /// `DOUBLE_CLICK_MS` milliseconds.
    pub last_click: Option<(u16, u16, Instant)>,
}

impl AppState {
    /// Create a fresh `AppState`.
    pub fn new(file_path: Option<PathBuf>, initial_text: String) -> Self {
        let format = file_path
            .as_deref()
            .map(FileFormat::from_path)
            .unwrap_or_default();
        Self {
            file_path,
            modified: false,
            format,
            raw_text: initial_text,
            undo_stack: UndoStack::default(),
            tree: None,
            tree_selected: 0,
            tree_scroll: 0,
            search_query: String::new(),
            parse_error: None,
            mode: Mode::default(),
            focus: Focus::default(),
            tree_pane_height: 0,
            picker_height: 0,
            picker_x: 0,
            picker_y: 0,
            picker_w: 0,
            tree_x_start: 0,
            scrollbar_x_start: 0,
            parse_dirty: false,
            last_edit: None,
            last_click: None,
        }
    }

    /// Returns `true` if the search bar / filter is currently active.
    #[allow(dead_code)]
    pub fn is_searching(&self) -> bool {
        self.mode == Mode::Search
    }

    /// Toggle focus between the two panes.
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::RawPane => Focus::TreePane,
            Focus::TreePane => Focus::RawPane,
        };
    }

    /// Clamp `tree_selected` to valid bounds.
    pub fn clamp_tree_selection(&mut self) {
        let max = self
            .tree
            .as_ref()
            .map(|t| t.visible_node_count().saturating_sub(1))
            .unwrap_or(0);
        self.tree_selected = self.tree_selected.min(max);
    }

    /// Move tree cursor down by `n` rows.
    pub fn tree_down(&mut self, n: usize) {
        let max = self
            .tree
            .as_ref()
            .map(|t| t.visible_node_count().saturating_sub(1))
            .unwrap_or(0);
        self.tree_selected = (self.tree_selected + n).min(max);
        self.scroll_tree_to_cursor();
    }

    /// Move tree cursor up by `n` rows.
    pub fn tree_up(&mut self, n: usize) {
        self.tree_selected = self.tree_selected.saturating_sub(n);
        self.scroll_tree_to_cursor();
    }

    /// Ensure the selected row is visible in the viewport.
    /// Public so callers that set `tree_selected` directly (e.g. after a
    /// search commit) can also trigger the scroll.
    pub fn ensure_visible(&mut self) {
        let h = self.tree_pane_height as usize;
        if h == 0 {
            return;
        }
        if self.tree_selected < self.tree_scroll {
            self.tree_scroll = self.tree_selected;
        } else if self.tree_selected >= self.tree_scroll + h {
            self.tree_scroll = self.tree_selected - h + 1;
        }
    }

    /// Private alias kept for callers inside this file.
    fn scroll_tree_to_cursor(&mut self) {
        self.ensure_visible();
    }

    /// Record the current raw text as a new undo snapshot. Call this
    /// **before** applying a change so the pre-change state can be
    /// restored.
    pub fn push_undo(&mut self) {
        let snap = Snapshot {
            text: self.raw_text.clone(),
            cursor_row: 0,
            cursor_col: 0,
        };
        self.undo_stack.push(snap);
    }

    /// Apply an undo, returning `true` if something changed.
    pub fn apply_undo(&mut self) -> Option<Snapshot> {
        let current = Snapshot {
            text: self.raw_text.clone(),
            cursor_row: 0,
            cursor_col: 0,
        };
        let prev = self.undo_stack.undo(current)?;
        self.raw_text = prev.text.clone();
        self.modified = true;
        Some(prev)
    }

    /// Apply a redo, returning the snapshot if something changed.
    pub fn apply_redo(&mut self) -> Option<Snapshot> {
        let current = Snapshot {
            text: self.raw_text.clone(),
            cursor_row: 0,
            cursor_col: 0,
        };
        let next = self.undo_stack.redo(current)?;
        self.raw_text = next.text.clone();
        self.modified = true;
        Some(next)
    }
}
