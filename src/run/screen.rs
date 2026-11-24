//! The live view of a run in a terminal.
//!
//! The run log scrolls into the terminal's own scrollback, like a shell session. Under it
//! sits the plan pane: the current stage with the step running now, which korin erases and
//! draws again around every print, so it always sits under the newest output.
//!
//! Erasing moves the cursor up by a count of terminal rows, so every count must be exact.
//! The view therefore writes whole lines only, and works out how many rows each line wraps
//! onto. It never erases more rows than the terminal shows: rows that scrolled off the top
//! cannot be reached, and moving up past them would overwrite other lines.
//!
//! When the view is not live (no terminal, or no color), every print goes straight to stdout.

use std::io::Write as _;
use std::sync::OnceLock;

use regex::Regex;
use unicode_width::UnicodeWidthStr as _;

use crate::theme::Theme;

pub struct Screen {
    live: bool,
    /// The plan pane's lines, already themed and fitted, without trailing newlines.
    pane: Vec<String>,
    /// The live window's lines: the command running now and the tail of its output. It sits
    /// above the pane, and both are erased and drawn together.
    window: Vec<String>,
    /// When the window was last drawn, so fast output redraws it only now and then.
    window_drawn: Option<std::time::Instant>,
    /// Whether the window changed since it was last drawn.
    window_pending: bool,
    /// How many rows the window and the pane take on screen, or `None` while not drawn.
    pane_rows: Option<usize>,
    /// Rows written since [`Screen::mark`], for erasing a gate's preview.
    since_mark: Option<usize>,
    /// Colors the rule that sets the live region apart from the run log.
    theme: Theme,
    /// How many dialogs are open. While one is, prints go into the pane, not the run log.
    dialogs: usize,
    /// The lines an open dialog printed, its prompts and answers included.
    dialog: Vec<String>,
    /// The pane a dialog took over, drawn again once the dialog closes.
    saved: Vec<String>,
}

impl Screen {
    pub fn new(live: bool, theme: Theme) -> Self {
        Self {
            live,
            theme,
            pane: Vec::new(),
            window: Vec::new(),
            window_drawn: None,
            window_pending: false,
            pane_rows: None,
            since_mark: None,
            dialogs: 0,
            dialog: Vec::new(),
            saved: Vec::new(),
        }
    }

    pub fn live(&self) -> bool {
        self.live
    }

    /// Writes whole lines (each ending in a newline) above the pane.
    pub fn print(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.live {
            print!("{text}");
            flush();
            return;
        }
        if self.dialogs > 0 {
            self.dialog
                .extend(text.trim_end_matches('\n').split('\n').map(str::to_string));
            self.show_dialog();
            return;
        }
        self.hide();
        print!("{text}");
        let added = rows(text, width());
        if let Some(count) = &mut self.since_mark {
            *count += added;
        }
        self.show();
    }

    /// Replaces the plan pane's lines.
    pub fn set_pane(&mut self, lines: Vec<String>) {
        if !self.live {
            return;
        }
        if lines == self.pane {
            // Draw it again if an erase took it off the screen.
            self.show();
            return;
        }
        self.hide();
        self.pane = lines;
        self.show();
    }

    /// Replaces the live window's lines. Redraws at most every 50 ms unless `now`, so fast
    /// output does not flood the terminal; a skipped redraw shows with the next one.
    pub fn set_window(&mut self, lines: Vec<String>, now: bool) {
        if !self.live || lines == self.window {
            return;
        }
        self.window = lines;
        let due = self
            .window_drawn
            .is_none_or(|at| at.elapsed() >= std::time::Duration::from_millis(50));
        if now || due {
            self.window_drawn = Some(std::time::Instant::now());
            self.window_pending = false;
            self.hide();
            self.show();
        } else {
            self.window_pending = true;
        }
    }

    /// Draws the window's newest lines if a redraw was skipped and is now due.
    pub fn flush_window(&mut self) {
        let due = self
            .window_drawn
            .is_none_or(|at| at.elapsed() >= std::time::Duration::from_millis(50));
        let stale = self.pane_rows.is_some() && self.window_pending;
        if self.live && due && stale {
            self.window_drawn = Some(std::time::Instant::now());
            self.window_pending = false;
            self.hide();
            self.show();
        }
    }

    /// Erases the pane from the screen. The next print draws it again.
    pub fn hide(&mut self) {
        if let Some(rows) = self.pane_rows.take()
            && rows > 0
        {
            print!("\x1b[{rows}A\x1b[J");
            flush();
        }
    }

    /// Erases the pane and the window and forgets both, as at the end of a stage or a run.
    pub fn clear_all(&mut self) {
        self.hide();
        self.pane.clear();
        self.window.clear();
    }

    /// Starts counting rows, so [`Screen::erase_to_mark`] can remove them later.
    pub fn mark(&mut self) {
        self.since_mark = self.live.then_some(0);
    }

    /// Opens a dialog, such as the values view: until [`Screen::end_dialog`], prints go into
    /// the plan pane in place of what it showed, so the run log stays a log of the run.
    /// Dialogs nest; the outermost one keeps the pane it took over.
    pub fn begin_dialog(&mut self) {
        if !self.live {
            return;
        }
        if self.dialogs == 0 {
            self.hide();
            self.saved = std::mem::take(&mut self.pane);
            self.dialog.clear();
        }
        self.dialogs += 1;
    }

    /// Starts an open dialog's lines over, as when the values view lists its rows again.
    pub fn dialog_restart(&mut self) {
        if self.dialogs > 0 {
            self.dialog.clear();
        }
    }

    /// Whether rows are being counted since a [`Screen::mark`], to be erased: a gate or a
    /// pause is open.
    pub fn marking(&self) -> bool {
        self.since_mark.is_some()
    }

    /// Whether a dialog is still open, as when one dialog opened another.
    pub fn dialog_open(&self) -> bool {
        self.dialogs > 0
    }

    /// Closes a dialog. Once the outermost one closes, the pane it took over comes back.
    pub fn end_dialog(&mut self) {
        if !self.live || self.dialogs == 0 {
            return;
        }
        self.dialogs -= 1;
        if self.dialogs == 0 {
            self.hide();
            self.dialog.clear();
            self.pane = std::mem::take(&mut self.saved);
            self.show();
        }
    }

    /// Draws an open dialog's lines in the pane: the newest ones, as many as fit with a row
    /// left for its next prompt.
    fn show_dialog(&mut self) {
        let cols = width();
        let budget = height().saturating_sub(4);
        let mut used = 0;
        let mut start = self.dialog.len();
        while start > 0 {
            let r = rows(&format!("{}\n", self.dialog[start - 1]), cols);
            if used + r > budget {
                break;
            }
            used += r;
            start -= 1;
        }
        let lines = self.dialog[start..].to_vec();
        self.hide();
        self.pane = lines;
        self.show();
    }

    /// Counts a prompt line the terminal showed under the pane, with the answer it echoed,
    /// so the next erase removes it with the pane. In a dialog, the line joins the dialog's
    /// lines, so it stays in view. With no pane on screen it counts toward [`Screen::mark`].
    pub fn prompted(&mut self, visible: &str) {
        if !self.live {
            return;
        }
        if self.dialogs > 0 {
            self.dialog.push(visible.to_string());
        }
        let added = rows(&format!("{visible}\n"), width());
        match &mut self.pane_rows {
            Some(drawn) => *drawn += added,
            None => {
                if let Some(count) = &mut self.since_mark {
                    *count += added;
                }
            }
        }
    }

    /// Erases everything written since [`Screen::mark`], if all of it is still on screen.
    pub fn erase_to_mark(&mut self) {
        self.hide();
        let Some(count) = self.since_mark.take() else {
            return;
        };
        if count > 0 && count < height() {
            print!("\x1b[{count}A\x1b[J");
            flush();
        }
    }

    fn show(&mut self) {
        if self.pane_rows.is_some() || (self.pane.is_empty() && self.window.is_empty()) {
            return;
        }
        let cols = width();
        // The runner fits the pane to a third of the terminal; this only guards the rule
        // that the window and the pane are never taller than the screen. The window gives
        // up rows first, its oldest lines first.
        let budget = height().saturating_sub(2);
        let row_count = |line: &String| rows(&format!("{line}\n"), cols);
        let pane_rows: usize = self.pane.iter().map(row_count).sum();
        let mut window_room = budget.saturating_sub(pane_rows + 1);
        let mut start = self.window.len();
        while start > 0 && row_count(&self.window[start - 1]) <= window_room {
            window_room -= row_count(&self.window[start - 1]);
            start -= 1;
        }
        // A rule across the terminal sets the live region apart from the run log above it.
        let rule = self.theme.separator(cols.saturating_sub(1).max(1));
        let mut text = String::new();
        let mut used = 0;
        for line in std::iter::once(&rule)
            .chain(&self.window[start..])
            .chain(&self.pane)
        {
            let line_rows = row_count(line);
            if used + line_rows > budget {
                break;
            }
            used += line_rows;
            text.push_str(line);
            text.push('\n');
        }
        print!("{text}");
        flush();
        self.pane_rows = Some(used);
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.clear_all();
    }
}

fn flush() {
    let _ = std::io::stdout().flush();
}

/// The terminal's width in columns, or 80 when it cannot be measured.
pub fn width() -> usize {
    terminal_size::terminal_size().map_or(80, |(w, _)| usize::from(w.0).max(1))
}

/// The terminal's height in rows, or 24 when it cannot be measured.
pub fn height() -> usize {
    terminal_size::terminal_size().map_or(24, |(_, h)| usize::from(h.0).max(1))
}

/// How many terminal rows `text` takes, for whole lines `cols` wide. A line exactly as wide
/// as the terminal takes one row: the cursor waits at its end, and the newline moves it on.
pub fn rows(text: &str, cols: usize) -> usize {
    text.split_inclusive('\n')
        .map(|line| {
            let line = line.trim_end_matches('\n');
            // A carriage return goes back to the start of the row, so the widest stretch
            // between returns is what the row holds.
            let width = line.split('\r').map(display_width).max().unwrap_or(0);
            width.div_ceil(cols).max(1)
        })
        .sum()
}

/// The columns a piece of a line takes: escape sequences take none, and a tab moves to the
/// next multiple of eight.
fn display_width(text: &str) -> usize {
    static ESCAPES: OnceLock<Regex> = OnceLock::new();
    let escapes = ESCAPES.get_or_init(|| {
        Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b[()][0-9A-Za-z]|\x1b[=>78]")
            .expect("escape pattern compiles")
    });
    let plain = escapes.replace_all(text, "");
    let mut width = 0;
    for (index, part) in plain.split('\t').enumerate() {
        if index > 0 {
            width = (width / 8 + 1) * 8;
        }
        width += part.width();
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_count_wrapping_tabs_escapes_and_returns() {
        assert_eq!(rows("abc\n", 10), 1);
        assert_eq!(rows("\n", 10), 1);
        assert_eq!(rows("0123456789\n", 10), 1);
        assert_eq!(rows("0123456789x\n", 10), 2);
        assert_eq!(rows("a\nb\n", 10), 2);
        assert_eq!(rows("\x1b[1;38;2;1;2;3mabc\x1b[0m\n", 3), 1);
        assert_eq!(rows("\tx\n", 8), 2);
        assert_eq!(rows("progress 10%\rprogress 100%\n", 80), 1);
        assert_eq!(rows("中文\n", 3), 2);
    }
}
