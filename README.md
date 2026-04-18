# jsonedit

A dual-pane terminal JSON editor built with Rust + Ratatui.

```
┌─────────────────┬───────────────────┐
│  JSON Source    │   Tree View       │
│  (raw editor)   │   (navigator)     │
│                 │                   │
└─────────────────┴───────────────────┘
│ [MODE]  filepath *  ⚠ parse error   │
└─────────────────────────────────────┘
```

## Installation

Requires Rust 1.75+ (stable).

```bash
cargo build --release
# Binary is at target/release/jsonedit
```

## Usage

```bash
jsonedit path/to/file.json   # open a file
jsonedit                     # start with an empty document
```

## Keybindings

### Global (any mode)

| Key         | Action                          |
|-------------|----------------------------------|
| `Ctrl+S`    | Save to disk                    |
| `Ctrl+O`    | Open file picker                 |
| `Ctrl+Q`    | Quit (prompts if unsaved)        |
| `Tab`       | Switch focus between panes      |

### Normal Mode

| Key                   | Action                                         |
|-----------------------|------------------------------------------------|
| `i`                   | Enter Insert mode (raw pane focused)           |
| `q` / `Q`             | Quit                                           |
| `/`                   | Open search bar                                |

### File Picker (`Ctrl+O`)

Opens a centered modal overlay for browsing the filesystem.
Only directories and `.json` files are shown.

| Key           | Action                                        |
|---------------|-----------------------------------------------|
| `↑` / `k`     | Move selection up                             |
| `↓` / `j`     | Move selection down                           |
| `PgUp`        | Jump up by one page                           |
| `PgDn`        | Jump down by one page                         |
| `Enter`       | Enter directory, or open highlighted JSON file |
| `Esc`         | Close picker without opening anything         |

If there are unsaved changes when a file is selected, a confirmation
prompt appears before discarding them.

### Normal Mode — Raw Pane Focused

| Key                   | Action                                |
|-----------------------|---------------------------------------|
| Any printable key     | Enter Insert mode and start typing    |

### Normal Mode — Tree Pane Focused

| Key                        | Action                                         |
|----------------------------|------------------------------------------------|
| `↑` / `k`                  | Move cursor up                                 |
| `↓` / `j`                  | Move cursor down                               |
| `PgUp` / `PgDn`            | Scroll by page                                 |
| `Space` / `←` / `→`        | Toggle collapse / expand container             |
| `a`                        | Add new key (prompts in status bar)            |
| `d`                        | Delete selected node (confirm Y/n)             |
| `r`                        | Rename selected key                            |
| `Enter`                    | Edit value of selected leaf                    |
| `y`                        | Copy value to clipboard                        |
| `g` / `l`                  | Jump to corresponding line in raw pane         |
| `↑` / `↓` (mouse scroll)   | Scroll tree                                    |

### Insert Mode (raw pane editing)

| Key           | Action                           |
|---------------|----------------------------------|
| `Esc`         | Return to Normal mode            |
| `Ctrl+Z`      | Undo                             |
| `Ctrl+Y`      | Redo                             |
| All other keys | Standard text editing           |

The raw pane re-parses JSON on every keystroke. If the JSON is valid, the
tree view updates immediately. If invalid, the last valid tree is preserved
and a parse error is shown in the status bar.

### Tree Edit Mode (add / rename / edit prompts)

| Key        | Action                           |
|------------|----------------------------------|
| `Enter`    | Confirm the edit                 |
| `Esc`      | Cancel                           |
| `Backspace`| Delete last character in prompt  |

When editing values, the input is parsed as JSON first. If it doesn't parse
as valid JSON, it is treated as a plain string.

### Search Mode

| Key        | Action                                      |
|------------|---------------------------------------------|
| Type       | Filter tree to matching keys/values         |
| `Enter`    | Jump to first match and close search bar    |
| `Esc`      | Clear filter and return to Normal mode      |

### Confirm Mode

| Key         | Action           |
|-------------|------------------|
| `y` / `Y`   | Confirm action   |
| Any other   | Cancel           |

## Architecture

```
src/
├── main.rs     Entry point, terminal setup, event loop
├── state.rs    AppState, Mode enum, undo/redo stack
├── tree.rs     JSON tree: FlatNode, collapse state, search filter
├── sync.rs     raw↔tree sync: parse and serialize
├── events.rs   Input event routing by Mode
└── ui.rs       Pure Ratatui render function
```

- **AppState** is the single source of truth; render and event code never
  hold their own copies.
- **Undo/redo** operates on full string snapshots (100 levels deep).
- **Live sync** — every keystroke re-parses the raw buffer; valid parses
  update the tree immediately while invalid states keep the last tree.
- **Tree mutations** (add / rename / delete / edit) are serialized back to
  the raw pane immediately.

## Dependencies

| Crate            | Purpose                              |
|------------------|--------------------------------------|
| `ratatui`        | TUI layout and widgets               |
| `crossterm`      | Terminal backend + events + mouse    |
| `tui-textarea`   | Editable text area widget            |
| `serde_json`     | JSON parsing and serialization       |
| `arboard`        | Clipboard access                     |
| `color-eyre`     | Error reporting                      |
# oe
