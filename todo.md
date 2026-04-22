# roe — Object Editor Todo

This file tracks all work items, their status, and notes.

---

## Legend

- `[x]` — completed
- `[ ]` — not started
- `[~]` — in progress / partial

---

## Phase 1: Project Scaffold

- [x] Initialize Rust project (`cargo init`)
- [x] Write `Cargo.toml` with all dependencies
- [x] Create `src/` module structure (`main`, `state`, `tree`, `sync`, `events`, `ui`, `format`)
- [x] `.gitignore`

---

## Phase 2: Core Data Layer

- [x] **`src/state.rs`** — `AppState`, `Mode` enum, `UndoStack`
  - [x] `Mode`: Normal, Insert, TreeEdit, Search, Replace, Confirm, FilePicker, About
  - [x] `UndoStack`: push, undo, redo (100 levels)
  - [x] `Focus` enum: RawPane / TreePane
  - [x] `AppState::tree_up / tree_down` with viewport clamping
  - [x] `AppState::push_undo / apply_undo / apply_redo`
  - [x] Layout cache fields: `tree_pane_height`, `picker_height`, `picker_x/y/w`, `tree_x_start`, `scrollbar_x_start`
  - [x] Debounced-reparse fields: `parse_dirty`, `last_edit`

- [x] **`src/tree.rs`** — `JsonTree`, `FlatNode`, collapse, search
  - [x] `flatten_value()` recursive walk → `Vec<FlatNode>`
  - [x] Collapse/expand by path in `HashSet`
  - [x] `set_search()` — filter flat list + mark ancestor paths
  - [x] `set_value_at_path()`, `delete_at_path()`, `add_key()`, `rename_key_at_path()`
  - [x] `split_path()`, `parse_path_parts()` helpers
  - [x] `sync_line_numbers()` — populate `FlatNode::line_number` from raw text
  - [x] `find_node_at_line()` — reverse lookup: line → flat index
  - [x] Unit tests for flatten, collapse, path splitting

- [x] **`src/sync.rs`** — raw ↔ tree
  - [x] `raw_to_tree()` — parse + transplant collapse state
  - [x] `tree_to_raw()` — pretty-print with 2-space indent
  - [x] `find_line_for_path()` — heuristic line number lookup
  - [x] Unit tests for roundtrip (JSON, YAML, TOML, CloudFormation YAML)

- [x] **`src/format.rs`** — multi-format codec
  - [x] `FileFormat` enum: JSON, YAML, TOML, XML
  - [x] `parse()` / `serialize()` / `line_for_path()` per format
  - [x] `from_path()` / `from_extension()` detection
  - [x] Auto-detection on paste (tries all formats if current fails)
  - [x] Auto-prettify on file open and `load_file()`

---

## Phase 2b: File Picker

- [x] `Mode::FilePicker` state with `current_dir`, `entries`, `selected`, `scroll`
- [x] `FileEntry` struct (`name`, `is_dir`, `path`)
- [x] `read_dir_entries()` — sorts dirs first, filters by supported extensions, skips hidden
- [x] `Action::OpenFile(PathBuf)` in `events.rs`
- [x] `Ctrl+O` global shortcut (starts in current file's directory or cwd)
- [x] Keyboard: ↑↓/j/k/PgUp/PgDn navigation, Enter to descend or open, Esc to cancel
- [x] Mouse: click to select, click selected entry to open, click outside to dismiss, scroll wheel
- [x] Unsaved-changes guard: `ConfirmAction::DiscardAndOpen`
- [x] `load_file()` in `main.rs` — resets all state, reloads textarea, re-parses
- [x] `draw_file_picker()` overlay — centered 70×70% popup
- [x] Picker geometry cached in `AppState` (`picker_x`, `picker_y`, `picker_w`, `picker_height`)

---

## Phase 3: Input Handling

- [x] **`src/events.rs`** — `handle_event()` routing by Mode
  - [x] Normal mode: focus switch (Tab), tree navigation, tree op triggers
  - [x] Insert mode: textarea forwarding, undo/redo, debounced reparse
  - [x] TreeEdit mode: key-by-key buffer + commit on Enter (add key, add value, rename, edit value)
  - [x] Search mode: live filter + jump-to-first-match on Enter
  - [x] Replace mode: Ctrl+R from Search; Enter replaces + advances, Ctrl+A replaces all, Esc back
  - [x] Confirm mode: Y accepts, anything else cancels
  - [x] Global shortcuts: `Ctrl+S` save, `Ctrl+Q` quit, `Ctrl+O` open, `Ctrl+Z/Y` undo/redo, `Ctrl+V` paste, `?` about
  - [x] Clipboard copy (`c` in tree pane) and paste (`Ctrl+V`)
  - [x] Mouse: hover-aware scroll, raw-pane click→tree sync, tree click→raw sync, file-picker mouse, scrollbar drag, right-click focus, single-click collapse toggle

- [x] Bidirectional raw↔tree sync
  - [x] Click in Object Source → tree selection follows cursor line
  - [x] Tree keyboard navigation (↑↓/PgUp/PgDn/Home/End) → raw cursor follows silently
  - [x] Scroll wheel in either pane → syncs the other
  - [x] `refresh_line_numbers()` called after every successful parse

---

## Phase 4: Rendering

- [x] **`src/ui.rs`** — `draw()` pure render function
  - [x] Layout: vertical split main/status-bar; horizontal split raw/scrollbar-strip/tree
  - [x] Raw pane: `tui-textarea` widget, border, focus highlight, format badge (hidden when empty)
  - [x] Tree pane: `List` widget with collapse indicators (▶/▼), centered placeholder
  - [x] `build_tree_item()` — per-node colored spans (key/value/type-hint/search-highlight)
  - [x] Scrollbar strip: 3-col strip between panes showing proportional position thumb for each pane
  - [x] Status bar: mode indicator, file path + dirty flag, parse error, context-sensitive hints
  - [x] Mode-specific status bar prompts (TreeEdit, Search, Replace, Confirm, FilePicker, About)
  - [x] About overlay (`?`) with full keybinding reference
  - [x] File picker overlay

---

## Phase 5: Entry Point

- [x] **`src/main.rs`** — terminal setup and event loop
  - [x] CLI argument parsing (optional file path)
  - [x] `enable_raw_mode` + `EnterAlternateScreen` + `EnableMouseCapture` + `EnableBracketedPaste`
  - [x] Teardown always runs (even on error)
  - [x] 16 ms poll loop (≈60 fps)
  - [x] `save_file()` — write textarea content to disk
  - [x] `load_file()` — reset state, reparse, prettify
  - [x] Debounced reparse (250 ms after last keystroke)

---

## Phase 6: Documentation

- [x] `README.md` — keybindings table, usage, architecture, dependencies
- [x] `SPEC.md` — full product specification
- [x] `todo.md` — this file

---

## Remaining Work

### High Priority

- [x] **Interactive save-as prompt** — `Mode::SaveAs { buffer }` added.
  `Ctrl+S` with no file path opens a status-bar prompt; Enter confirms (auto-
  appends format extension if omitted), Esc cancels.

- [x] **Array index display** — Array child nodes now render as `[0]`, `[1]`
  etc. in cyan, clearly distinct from object keys (yellow).

- [x] **Jump to parse error** — `Ctrl+E` jumps the raw pane cursor to the
  error line (parsed from the error message for JSON, YAML, TOML).  Hint
  appears in the status bar whenever a parse error is active.

### Medium Priority

- [ ] **Full per-token syntax highlighting in raw pane** — `tui-textarea`
  supports custom line styles but not character-level spans. Requires a custom
  renderer or rendering the text as a `Paragraph` in read-only mode.

- [ ] **Persistent collapse state across sessions** — Collapse state survives
  re-parses within a session but is lost on restart. Could write a small
  sidecar file (e.g. `.filename.roe`) next to the opened file.

- [ ] **Config file** — Allow users to customize colors and key bindings via
  a `~/.config/roe/config.toml` file.

### Low Priority

- [ ] **Jump-to-line accuracy** — `find_line_for_path` is a heuristic text
  scan; replace with a proper source-map built during parse for O(1) lookups
  and correct handling of duplicate keys.

- [ ] **Large file performance** — Virtualize the tree list and defer line-number
  syncing for files with 10 k+ nodes.

- [ ] **Schema validation** — Optionally validate the open document against a
  JSON Schema file and annotate the tree with validation errors.

- [ ] **XML write support** — `serialize()` for XML is a stub; currently
  round-trips through JSON representation.
