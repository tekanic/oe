# oe — Object Editor

A dual-pane terminal editor for structured data, built with Rust + Ratatui.

Supports **JSON**, **YAML**, **TOML**, and **XML**.

```
┌──────────────────────┬───┬──────────────────────┐
│   Object Source      │ █ │   Tree View           │
│   (raw editor)       │ │ │   (navigator)         │
│                      │ │ │                       │
└──────────────────────┴───┴──────────────────────┘
│ [MODE]  filepath *  ⚠ parse error  hints         │
└───────────────────────────────────────────────────┘
```

The center strip is a unified scrollbar that moves both panes simultaneously.

---

## Installation

Requires Rust 1.75+ (stable).

```bash
git clone <repo>
cd JSON-Editor
cargo build --release
# Binary is at target/release/oe
```

---

## Usage

```bash
oe path/to/file.json    # open a JSON file
oe data.yaml            # open a YAML file
oe config.toml          # open a TOML file
oe                      # start with an empty document
```

Files are auto-detected by extension. On open, the document is parsed and
pretty-printed using a canonical 2-space indent. Paste detection also tries
all supported formats automatically.

---

## Keybindings

### Global (any mode)

| Key        | Action                                      |
|------------|---------------------------------------------|
| `Ctrl+S`   | Save to disk (prompts for filename if new)  |
| `Ctrl+O`   | Open file picker                            |
| `Ctrl+Q`   | Quit (prompts if unsaved changes)           |
| `Ctrl+Z`   | Undo                                        |
| `Ctrl+Y`   | Redo                                        |
| `Ctrl+V`   | Paste from clipboard                        |
| `Ctrl+E`   | Jump raw pane cursor to parse error line    |
| `Tab`      | Switch focus between panes                  |
| `?`        | Toggle About / keybindings overlay          |

---

### Normal Mode — Raw Pane Focused

| Key                | Action                                            |
|--------------------|---------------------------------------------------|
| `i`                | Enter Insert mode                                 |
| Any printable key  | Enter Insert mode and start typing                |
| Double-click word  | Select word and enter Insert mode                 |

---

### Normal Mode — Tree Pane Focused

| Key                     | Action                                          |
|-------------------------|-------------------------------------------------|
| `↑` / `k`               | Move cursor up one row                          |
| `↓` / `j`               | Move cursor down one row                        |
| `PgUp`                  | Move cursor up one page                         |
| `PgDn`                  | Move cursor down one page                       |
| `Home` / `g`            | Jump to first node                              |
| `End` / `G`             | Jump to last node                               |
| `Space` / `←` / `→`    | Toggle collapse / expand container              |
| `Enter`                 | Edit leaf value, or toggle container            |
| `a`                     | Add new key / array item                        |
| `d`                     | Delete selected node (confirm required)         |
| `r`                     | Rename selected key                             |
| `c`                     | Copy node value to clipboard                    |
| `/`                     | Open search / filter bar                        |

Tree navigation syncs the raw pane cursor to the corresponding line.

---

### Insert Mode (raw pane editing)

| Key             | Action                                          |
|-----------------|-------------------------------------------------|
| `Esc`           | Return to Normal mode                           |
| `Ctrl+Z`        | Undo                                            |
| `Ctrl+Y`        | Redo                                            |
| All other keys  | Standard text editing (arrows, backspace, etc.) |

The raw pane re-parses after 250 ms of inactivity. If valid, the tree
updates immediately. If invalid, the last good tree is preserved and a
parse error appears in the status bar.

---

### Tree Edit Mode (add / rename / edit value)

| Key         | Action                          |
|-------------|---------------------------------|
| `Enter`     | Confirm the edit                |
| `Esc`       | Cancel                          |
| `Backspace` | Delete last character           |
| Any char    | Append to the input buffer      |

Value input is first parsed as JSON; if it fails, the text is stored as a
plain string.

---

### Search Mode (`/`)

| Key         | Action                                        |
|-------------|-----------------------------------------------|
| Type        | Live-filter tree to matching keys / values    |
| `Enter`     | Jump to first match and return to Normal mode |
| `Ctrl+R`    | Switch to Replace mode                        |
| `Esc`       | Clear filter and return to Normal mode        |

---

### Replace Mode (`Ctrl+R` from Search)

| Key         | Action                                        |
|-------------|-----------------------------------------------|
| Type        | Build replacement string                      |
| `Enter`     | Replace current match and advance             |
| `Ctrl+A`    | Replace all matches at once                   |
| `Esc`       | Return to Search mode                         |

---

### Confirm Mode

| Key       | Action           |
|-----------|------------------|
| `y` / `Y` | Confirm action   |
| Any other | Cancel           |

---

### Save As Mode (`Ctrl+S` with no file open)

| Key         | Action                                              |
|-------------|-----------------------------------------------------|
| Type        | Build filename                                      |
| `Enter`     | Save (format extension auto-appended if omitted)    |
| `Esc`       | Cancel                                              |
| `Backspace` | Delete last character                               |

---

### File Picker (`Ctrl+O`)

A centered overlay for browsing the filesystem. Directories and supported
files (`.json`, `.yaml`, `.toml`, `.xml`) are listed; hidden files are
excluded.

| Key           | Action                                          |
|---------------|-------------------------------------------------|
| `↑` / `k`     | Move selection up                               |
| `↓` / `j`     | Move selection down                             |
| `PgUp`        | Jump up one page                                |
| `PgDn`        | Jump down one page                              |
| `Enter`       | Enter directory or open highlighted file        |
| `Esc`         | Close picker without opening                    |

If there are unsaved changes, a confirmation prompt appears before
discarding them.

---

## Mouse Support

| Action                              | Result                                           |
|-------------------------------------|--------------------------------------------------|
| Click in raw pane                   | Move cursor; tree scrolls to matching node       |
| Double-click word in raw pane       | Select word, enter Insert mode                   |
| Click in tree pane                  | Select node; raw pane cursor follows             |
| Double-click node in tree pane      | Edit leaf / toggle container                     |
| Click `▶` / `▼` indicator           | Toggle collapse without changing selection       |
| Scroll wheel                        | Scrolls the **focused** pane only                |
| Click / drag center scrollbar       | Seeks both panes to the proportional position    |
| Right-click                         | Transfer focus to the clicked pane               |
| Click outside file picker           | Dismiss picker                                   |

---

## Architecture

```
src/
├── main.rs     Entry point, terminal setup, event loop, save/load
├── state.rs    AppState, Mode enum, UndoStack, Focus, FileEntry
├── tree.rs     JsonTree, FlatNode, collapse state, search filter, line sync
├── sync.rs     raw ↔ tree: parse, serialize, line-number lookup
├── format.rs   FileFormat codec: JSON, YAML, TOML, XML
├── events.rs   Input event routing by Mode, mouse handling, bidirectional sync
└── ui.rs       Pure Ratatui render function (no state mutation)
```

### Key design points

- **Single source of truth** — `AppState` is passed by `&mut` to event handlers
  and by `&` to the render function. No other copies of state exist.
- **Bidirectional sync** — clicking in either pane moves the cursor in the
  other. Line numbers are cached in `FlatNode` after every successful parse.
- **Debounced reparse** — the tree updates 250 ms after the last keystroke,
  avoiding a full reparse on every character.
- **Undo/redo** — full string snapshots, 100 levels, O(1) eviction via
  `VecDeque`.
- **Unified scrollbar** — a single `███` thumb in the center strip drives
  both panes proportionally; its position reflects the focused pane.

---

## Dependencies

| Crate          | Purpose                                  |
|----------------|------------------------------------------|
| `ratatui`      | TUI layout and widgets                   |
| `crossterm`    | Terminal backend, events, mouse support  |
| `tui-textarea` | Editable text area widget                |
| `serde_json`   | JSON parsing and serialization           |
| `serde_yaml`   | YAML parsing and serialization           |
| `toml`         | TOML parsing and serialization           |
| `roxmltree`    | XML parsing (read-only)                  |
| `serde`        | Derive macros for format codecs          |
| `arboard`      | Clipboard read/write                     |
| `color-eyre`   | Pretty error reporting                   |
