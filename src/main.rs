//! `roe` — ROE Object Editor, a dual-pane TUI editor for structured data.
//!
//! Entry point: sets up the terminal, runs the event loop, and tears
//! everything down cleanly on exit or panic.
//!
//! Usage:
//!   roe [path/to/file.json|yaml|toml|xml]

mod events;
mod format;
mod state;
mod sync;
mod tree;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr};
use crossterm::{
    event::{self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tui_textarea::TextArea;

use events::{handle_event, reparse_now, refresh_line_numbers, Action};
use format::FileFormat;
use state::AppState;
use sync::{raw_to_tree, ParseResult};

fn main() -> Result<()> {
    // Initialise color-eyre for pretty error reports.
    color_eyre::install()?;

    // Parse CLI arguments.
    let args: Vec<String> = std::env::args().collect();
    let file_path: Option<PathBuf> = args.get(1).map(PathBuf::from);

    // Load initial content.
    let initial_text = match &file_path {
        Some(path) => {
            std::fs::read_to_string(path)
                .wrap_err_with(|| format!("Failed to read '{}'", path.display()))?
        }
        None => String::new(),
    };

    // Build initial application state (detects format from file extension).
    let mut state = AppState::new(file_path, initial_text.clone());

    // Perform initial parse and prettify — only when there is actual content.
    // On success the raw text is replaced with the canonical pretty-printed form
    // so the Object Source pane always shows formatted output on open.
    // On failure the original text is shown as-is so the user can see the error.
    let display_text: String = if state.raw_text.is_empty() {
        String::new()
    } else {
        match raw_to_tree(&state.raw_text, Default::default(), state.format) {
            ParseResult::Ok(tree) => {
                let pretty = state.format.serialize(&tree.root);
                state.raw_text = pretty.clone();
                state.tree = Some(tree);
                refresh_line_numbers(&mut state);
                pretty
            }
            ParseResult::Err(msg) => {
                state.parse_error = Some(msg);
                initial_text.clone()
            }
        }
    };

    // Build the textarea from the (possibly prettified) display text.
    let lines: Vec<String> = display_text.lines().map(String::from).collect();
    let mut textarea: TextArea<'static> = if lines.is_empty() {
        TextArea::default()
    } else {
        TextArea::from(lines)
    };

    // ── terminal setup ────────────────────────────────────────────────────────
    enable_raw_mode().wrap_err("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)
        .wrap_err("Failed to enter alternate screen")?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).wrap_err("Failed to create terminal")?;
    terminal.clear()?;

    // ── main event loop ───────────────────────────────────────────────────────
    let result = run_loop(&mut terminal, &mut state, &mut textarea);

    // ── terminal teardown (always runs, even on error) ────────────────────────
    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
    )
    .ok();
    terminal.show_cursor().ok();

    // Propagate any error from the loop.
    result
}

/// How long after the last keystroke before re-parsing the raw text.
///
/// Debouncing avoids reparsing on every character when editing large files —
/// the tree only updates once typing pauses for this duration.
const PARSE_DEBOUNCE: Duration = Duration::from_millis(250);

/// Main event loop. Returns when the user quits or a fatal error occurs.
fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
) -> Result<()> {
    loop {
        // ── render ────────────────────────────────────────────────────────────
        terminal.draw(|frame| {
            let size = frame.area();
            // Reserve 1 line for status bar and 2 for borders.
            state.tree_pane_height = size.height.saturating_sub(3);

            // Layout: [Fill(1), Length(3), Fill(1)] — 3-col scrollbar strip between panes.
            // raw_width = (W - 3) / 2; scrollbar starts there; tree starts 3 cols later.
            let raw_width = size.width.saturating_sub(3) / 2;
            state.scrollbar_x_start = raw_width;
            state.tree_x_start = raw_width + 3;

            // File picker popup: 70% wide × 70% tall, centred on screen.
            // Layout inside popup: 1-char border on all sides, then 1-row dir header,
            // then the scrollable file list.
            //   list_x = popup_x + 1 (border)
            //   list_y = popup_y + 2 (border + dir header)
            //   list_w = popup_w - 2 (both side borders)
            //   list_h = popup_h - 3 (top border + dir header + bottom border)
            let popup_w = size.width * 70 / 100;
            let popup_h = size.height * 70 / 100;
            let popup_x = size.width.saturating_sub(popup_w) / 2;
            let popup_y = size.height.saturating_sub(popup_h) / 2;
            state.picker_x = popup_x + 1;
            state.picker_y = popup_y + 2;
            state.picker_w = popup_w.saturating_sub(2);
            state.picker_height = popup_h.saturating_sub(3);

            ui::draw(frame, state, textarea);
        })?;

        // ── event polling (non-blocking, 16 ms tick ≈ 60 fps) ────────────────
        if event::poll(Duration::from_millis(16))? {
            let ev = event::read()?;

            // ── process event ─────────────────────────────────────────────────
            let action = handle_event(&ev, state, textarea);

            match action {
                Action::Quit => break,
                Action::Save => {
                    save_file(state, textarea)?;
                }
                Action::OpenFile(path) => {
                    load_file(state, textarea, path)?;
                }
                Action::Continue | Action::Noop => {}
            }
        }

        // ── debounced reparse ─────────────────────────────────────────────────
        // Fires on every tick so the tree updates ~250 ms after typing stops,
        // regardless of whether an event arrived this iteration.
        if state.parse_dirty {
            if state.last_edit.map_or(false, |t| t.elapsed() >= PARSE_DEBOUNCE) {
                reparse_now(state);
            }
        }
    }
    Ok(())
}

/// Read a file from disk and reset the editor to show its contents.
///
/// Detects the file format from the extension, clears the undo stack, resets
/// the tree, and updates both the textarea widget and the `AppState` mirror.
fn load_file(
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
    path: std::path::PathBuf,
) -> Result<()> {
    let text = std::fs::read_to_string(&path)
        .wrap_err_with(|| format!("Failed to read '{}'", path.display()))?;

    // Detect format from the new path before resetting state.
    let fmt = FileFormat::from_path(&path);

    // Reset state.
    state.file_path = Some(path);
    state.format = fmt;
    state.modified = false;
    state.undo_stack = Default::default();
    state.tree = None;
    state.tree_selected = 0;
    state.tree_scroll = 0;
    state.parse_error = None;
    state.mode = state::Mode::Normal;
    state.search_query.clear();
    state.parse_dirty = false;
    state.last_edit = None;
    state.last_click = None;

    // Parse and prettify. On success display the canonical formatted text;
    // on failure keep the raw file content so the user can see the error.
    let display_text = match sync::raw_to_tree(&text, Default::default(), fmt) {
        sync::ParseResult::Ok(tree) => {
            let pretty = fmt.serialize(&tree.root);
            state.raw_text = pretty.clone();
            state.tree = Some(tree);
            refresh_line_numbers(state);
            pretty
        }
        sync::ParseResult::Err(msg) => {
            state.raw_text = text.clone();
            state.parse_error = Some(msg);
            text
        }
    };

    // Rebuild textarea from the display text.
    let lines: Vec<String> = display_text.lines().map(String::from).collect();
    *textarea = if lines.is_empty() {
        TextArea::default()
    } else {
        TextArea::from(lines)
    };

    Ok(())
}

/// Write the raw pane content to disk.
///
/// If `state.file_path` is `None` (new file), defaults to the format's
/// canonical filename (e.g. `output.json`, `output.yaml`).
fn save_file(state: &mut AppState, textarea: &TextArea<'static>) -> Result<()> {
    let raw = textarea.lines().join("\n");

    let path = match &state.file_path {
        Some(p) => p.clone(),
        None => {
            let default_path = PathBuf::from(state.format.default_filename());
            state.file_path = Some(default_path.clone());
            default_path
        }
    };

    std::fs::write(&path, raw.as_bytes())
        .wrap_err_with(|| format!("Failed to write '{}'", path.display()))?;

    state.modified = false;
    state.raw_text = raw;
    Ok(())
}
