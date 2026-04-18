# jsonedit — Product Specification

> This document captures the full feature specification for the `jsonedit`
> TUI application. It is the canonical reference for what the MVP delivers
> and what remains for future work.

---

## Stack & Crates

| Crate           | Role                                             |
|-----------------|--------------------------------------------------|
| `ratatui`       | TUI rendering and layout                         |
| `crossterm`     | Terminal backend, keyboard + mouse event handling|
| `serde_json`    | JSON parsing, serialization, and tree representation |
| `tui-textarea`  | Editable text area for the raw pane              |
| `arboard`       | Clipboard access (copy node value)               |
| `color-eyre`    | Error handling                                   |

---

## Application Layout

Split the terminal window into two vertical panes side by side:

### LEFT PANE — Raw JSON Editor

- A full editable text area showing the raw JSON source
- Syntax highlighting: keys in one color, string values in another,
  numbers/booleans/null each distinctly colored
- Inline error highlighting: if the JSON is invalid, underline or
  highlight the offending region in red and show a status bar message
  with the parse error and line/column number
- The pane has a visible border and a title: "JSON Source"

### RIGHT PANE — Tree View

- A navigable, interactive tree rendering of the parsed JSON object
- Each node shows its key (or array index) and value type
- Leaf nodes show their actual value inline
- The pane has a visible border and a title: "Tree View"

### STATUS BAR

- A single line at the bottom of the screen
- Shows: current file path, modified indicator (`*`), parse error message
  if JSON is invalid, active keyboard shortcuts, and current mode

---

## Input / Output

- Accept a file path as a CLI argument: `jsonedit path/to/file.json`
- On launch, load and parse the file into both panes
- `Ctrl+S` saves the current raw pane content back to the original file
- If no file path is given, start with an empty document and save to
  `output.json` on first `Ctrl+S`
- Do not auto-save; only write to disk on explicit `Ctrl+S`

---

## Sync Strategy: Live (as you type)

- Every keystroke in the raw pane triggers a re-parse of the JSON
- If parsing succeeds, immediately update the tree view. Preserve the
  user's current tree scroll position and expanded/collapsed state where
  the structure allows it.
- If parsing fails, leave the tree view showing the last valid parse
  and display the error in the status bar. Do not clear the tree on
  every keystroke error.
- Every keystroke in the tree pane (add/rename/delete) immediately
  re-serializes the tree to JSON and updates the raw pane

---

## Editing Features

### Raw Pane (text area)

- Full multi-line text editing: insert, delete, backspace, arrow keys,
  Home, End, Page Up, Page Down
- Syntax highlighting applied on each render pass
- Inline error highlight on the invalid token/region
- `Ctrl+Z` / `Ctrl+Y` for undo/redo (100+ levels deep, string snapshots)

### Tree Pane

| Key        | Action                                               |
|------------|------------------------------------------------------|
| `↑` / `↓`  | Navigate nodes                                       |
| `Space`    | Collapse / expand container                          |
| `a`        | Add new key (prompt in status bar for key, then value) |
| `d`        | Delete node (confirm Y/n in status bar)              |
| `r`        | Rename key (inline edit in status bar)               |
| `Enter`    | Edit leaf value (inline edit in status bar)          |
| `y`        | Copy value to clipboard                              |
| `g` / `l`  | Jump to corresponding line in raw pane               |

---

## Search / Filter

- Press `/` in either pane to open a search bar in the status bar area
- As the user types, the tree view filters to show only matching keys
  and their ancestor path (context preserved)
- Matching key names are highlighted in the tree
- Press `Escape` to clear the filter and return to the full tree
- Press `Enter` to jump to the first match and close the search bar

---

## Navigation & Input Handling

- Mouse + keyboard both supported
- Mouse scroll scrolls within the focused tree pane
- `Tab` switches focus between left and right pane
- `Esc` closes any active modal/prompt
- `q` or `Ctrl+Q` quits (prompt to save if unsaved changes exist)

---

## Visual Style

- Base UI chrome (borders, titles, status bar): `Style::default()` — inherits
  terminal theme
- Semantic colors for JSON tokens (ANSI-16):

  | Token           | Color              |
  |-----------------|--------------------|
  | Keys            | `Color::Yellow`    |
  | String values   | `Color::Green`     |
  | Numbers         | `Color::Cyan`      |
  | Booleans / null | `Color::Magenta`   |
  | Errors          | `Color::Red`       |
  | Structural chars| `Color::White`     |

- Selected tree node: reverse video (no hardcoded color)
- All borders use the default terminal foreground color

---

## Architecture

### Module Map

```
src/
├── main.rs     Entry point, terminal setup/teardown, main event loop
├── state.rs    AppState, Mode enum, UndoStack
├── tree.rs     JsonTree, FlatNode, collapse state, search filter
├── sync.rs     raw_to_tree(), tree_to_raw(), find_line_for_path()
├── events.rs   handle_event() routing by Mode, Action enum
└── ui.rs       draw() — pure render function
```

### AppState Fields

| Field              | Type                  | Purpose                             |
|--------------------|-----------------------|-------------------------------------|
| `file_path`        | `Option<PathBuf>`     | Open file path                      |
| `modified`         | `bool`                | Dirty flag                          |
| `raw_text`         | `String`              | Mirror of textarea content          |
| `undo_stack`       | `UndoStack`           | Undo/redo history                   |
| `tree`             | `Option<JsonTree>`    | Last valid parsed tree              |
| `tree_selected`    | `usize`               | Flat-list cursor index              |
| `tree_scroll`      | `usize`               | Tree viewport offset                |
| `search_query`     | `String`              | Current filter string               |
| `parse_error`      | `Option<String>`      | Last parse error message            |
| `mode`             | `Mode`                | Current interaction mode            |
| `focus`            | `Focus`               | Which pane has keyboard focus       |
| `tree_pane_height` | `u16`                 | Layout cache for scrolling          |

### Mode Enum

```rust
enum Mode {
    Normal,
    Insert,
    TreeEdit { kind: TreeEditKind, buffer: String },
    Search,
    Confirm { question: String, action: ConfirmAction },
}
```

### Undo/Redo

- Stack operates on full raw-text string snapshots
- Maximum depth: 100 entries
- Cleared on `Ctrl+Z` → `Ctrl+Y` sequence

---

## Deliverables

- [x] `Cargo.toml` — all dependencies pinned to current stable
- [x] `src/main.rs` — entry point, terminal setup, main event loop
- [x] `src/state.rs` — AppState, Mode, UndoStack
- [x] `src/events.rs` — input event routing by Mode
- [x] `src/ui.rs` — full Ratatui render for both panes + status bar
- [x] `src/tree.rs` — JSON tree, collapse state, search filter
- [x] `src/sync.rs` — parse raw→tree and serialize tree→raw
- [x] `README.md` — keybindings and how to run
- [x] `SPEC.md` — this document
- [x] `todo.md` — MVP tracking
