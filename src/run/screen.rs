//! The live view of a run in a terminal.
//!
//! When the view is live, the steps still to run stay on screen as dim "ghost" lines below
//! the output. Each print erases the ghost, writes its lines, and draws the ghost again, so
//! the ghost always sits under the newest output. A gate's preview is erased once it is
//! approved, so the stage reads as a header followed by what actually ran.
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

/// The share of the terminal's height the ghost may take; the rest is for output.
const GHOST_SHARE: usize = 2;

pub struct Screen {
    live: bool,
    /// Ghost lines, already themed, without trailing newlines.
    ghost: Vec<String>,
    /// How many rows the ghost takes on screen, or `None` while it is not drawn.
    ghost_rows: Option<usize>,
    /// Rows written since [`Screen::mark`], for erasing a gate's preview.
    since_mark: Option<usize>,
    theme: Theme,
}

impl Screen {
    pub fn new(live: bool, theme: Theme) -> Self {
        Self {
            live,
            ghost: Vec::new(),
            ghost_rows: None,
            since_mark: None,
            theme,
        }
    }

    pub fn live(&self) -> bool {
        self.live
    }

    /// Writes whole lines (each ending in a newline) above the ghost.
    pub fn print(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.live {
            print!("{text}");
            flush();
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

    /// Writes text that may end mid-line. Only for a view that is not live.
    pub fn print_raw(&mut self, text: &str) {
        debug_assert!(!self.live, "a live view writes whole lines only");
        print!("{text}");
        flush();
    }

    /// Replaces the ghost lines.
    pub fn set_ghost(&mut self, lines: Vec<String>) {
        if !self.live || lines == self.ghost {
            return;
        }
        self.hide();
        self.ghost = lines;
        self.show();
    }

    /// Erases the ghost from the screen. The next print draws it again.
    pub fn hide(&mut self) {
        if let Some(rows) = self.ghost_rows.take()
            && rows > 0
        {
            print!("\x1b[{rows}A\x1b[J");
            flush();
        }
    }

    /// Erases the ghost and forgets it, as at the end of a stage or a run.
    pub fn clear(&mut self) {
        self.hide();
        self.ghost.clear();
    }

    /// Starts counting rows, so [`Screen::erase_to_mark`] can remove them later.
    pub fn mark(&mut self) {
        self.since_mark = self.live.then_some(0);
    }

    /// Counts a line the terminal showed without [`Screen::print`]: a prompt and the answer
    /// the terminal echoed, ending with the newline of the Enter key.
    pub fn echoed(&mut self, visible: &str) {
        if let Some(count) = &mut self.since_mark {
            *count += rows(&format!("{visible}\n"), width());
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
        if self.ghost_rows.is_some() || self.ghost.is_empty() {
            return;
        }
        let cols = width();
        let budget = (height() / GHOST_SHARE).max(3);
        let mut text = String::new();
        let mut used = 0;
        for (index, line) in self.ghost.iter().enumerate() {
            let line_rows = rows(&format!("{line}\n"), cols);
            // Keep a row for the `… N more` line when anything is left out.
            let left = self.ghost.len() - index - 1;
            let reserve = usize::from(left > 0);
            if used + line_rows + reserve > budget {
                let more = self
                    .theme
                    .ghost(&format!("  … {} more", self.ghost.len() - index));
                used += rows(&format!("{more}\n"), cols);
                text.push_str(&more);
                text.push('\n');
                break;
            }
            used += line_rows;
            text.push_str(line);
            text.push('\n');
        }
        print!("{text}");
        flush();
        self.ghost_rows = Some(used);
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.clear();
    }
}

fn flush() {
    let _ = std::io::stdout().flush();
}

fn width() -> usize {
    terminal_size::terminal_size().map_or(80, |(w, _)| usize::from(w.0).max(1))
}

fn height() -> usize {
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
