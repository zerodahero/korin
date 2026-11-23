//! The run log's parts that need no terminal: hold scopes, the fold rules, and spools.
//!
//! A command's output reaches the run log as one block once the command ends. A poll holds
//! the blocks of each try in a scope until it knows the try's fate, so a block is rendered
//! only when it reaches the run log. Folded or full is decided then.
//!
//! The full output of a command lives in its spool: one file per command under the run's
//! own scratch root, holding the redacted output in the order it arrived, each line tagged
//! with its stream. Rendering in full streams the spool, so memory stays bounded however
//! long the output.

use std::fs::File;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};

/// A successful command's output folds when it has more lines than this.
pub const FOLD_AT: usize = 12;
/// How many lines a folded output keeps on show.
pub const HEAD: usize = 4;
/// How many lines of a running command the live window shows.
pub const WINDOW: usize = 8;

/// Blocks held back from the run log, innermost scope last.
#[derive(Debug)]
pub struct Scopes<B> {
    stack: Vec<Vec<B>>,
}

impl<B> Default for Scopes<B> {
    fn default() -> Self {
        Self { stack: Vec::new() }
    }
}

impl<B> Scopes<B> {
    /// Opens a scope: blocks emitted until [`Scopes::take`] collect in it.
    #[allow(dead_code)] // polls hold their tries; see the next commit
    pub fn hold(&mut self) {
        self.stack.push(Vec::new());
    }

    /// Whether any scope is open.
    pub fn holding(&self) -> bool {
        !self.stack.is_empty()
    }

    /// Adds a block to the innermost scope. With no scope open, hands it back to go to the
    /// run log now.
    pub fn emit(&mut self, block: B) -> Option<B> {
        match self.stack.last_mut() {
            Some(scope) => {
                scope.push(block);
                None
            }
            None => Some(block),
        }
    }

    /// Closes the innermost scope and hands back its blocks, in order. The caller emits them
    /// again to commit them, or emits something else in their place.
    #[allow(dead_code)] // polls hold their tries; see the next commit
    pub fn take(&mut self) -> Vec<B> {
        self.stack.pop().unwrap_or_default()
    }
}

/// How a command's output shows in the run log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Every line.
    Full,
    /// The first [`HEAD`] lines, then a note of how many more there are.
    Folded,
}

/// The mode for a command's output: a failed command, or one in a unit that failed or was
/// cancelled, always shows in full; a successful one folds once it is long.
pub fn mode(ok: bool, forced_full: bool, lines: usize) -> Mode {
    if ok && !forced_full && lines > FOLD_AT {
        Mode::Folded
    } else {
        Mode::Full
    }
}

/// Which stream a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Out,
    Err,
}

impl Stream {
    fn tag(self) -> char {
        match self {
            Stream::Out => 'o',
            Stream::Err => 'e',
        }
    }
}

/// One command's full output on disk, redacted, in the order its lines arrived.
#[derive(Debug)]
pub struct Spool {
    path: PathBuf,
    file: Option<File>,
}

impl Spool {
    /// Creates the spool file. It must already be redacted text that goes in.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file cannot be created.
    pub fn create(path: PathBuf) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self {
            path,
            file: Some(file),
        })
    }

    /// Adds whole, redacted lines from one stream. A spool that failed to write stops
    /// taking lines.
    pub fn push(&mut self, stream: Stream, text: &str) {
        let Some(file) = &mut self.file else {
            return;
        };
        let mut out = String::new();
        for line in text.split_inclusive('\n') {
            out.push(stream.tag());
            out.push_str(line.trim_end_matches('\n'));
            out.push('\n');
        }
        if file.write_all(out.as_bytes()).is_err() {
            self.file = None;
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Closes the file; the spool can still be read.
    pub fn close(&mut self) {
        self.file = None;
    }
}

/// Reads a spool back, line by line, calling `each` with the stream and the text.
///
/// # Errors
///
/// Returns an I/O error if the file cannot be read.
pub fn read_spool(path: &Path, mut each: impl FnMut(Stream, &str)) -> std::io::Result<()> {
    let reader = BufReader::new(File::open(path)?);
    for line in reader.lines() {
        let line = line?;
        let mut chars = line.chars();
        let stream = match chars.next() {
            Some('e') => Stream::Err,
            _ => Stream::Out,
        };
        each(stream, chars.as_str());
    }
    Ok(())
}

/// A spool's text with its stream tags removed, for a pager.
#[allow(dead_code)] // the `o` picker opens spools; see the next commit
///
/// # Errors
///
/// Returns an I/O error if the file cannot be read.
pub fn spool_text(path: &Path) -> std::io::Result<String> {
    let mut out = String::new();
    read_spool(path, |_, line| {
        out.push_str(line);
        out.push('\n');
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_releases_its_blocks_in_order() {
        let mut scopes = Scopes::default();
        scopes.hold();
        assert_eq!(scopes.emit("one"), None);
        assert_eq!(scopes.emit("two"), None);
        assert_eq!(scopes.take(), ["one", "two"]);
        assert!(!scopes.holding());
        // With no scope open, a block goes straight on.
        assert_eq!(scopes.emit("three"), Some("three"));
    }

    #[test]
    fn a_nested_scope_commits_into_its_parent() {
        let mut scopes = Scopes::default();
        scopes.hold();
        scopes.emit("outer");
        scopes.hold();
        scopes.emit("inner");
        // Committing the inner scope emits its blocks again: they land in the outer scope,
        // not in the run log.
        for block in scopes.take() {
            assert_eq!(scopes.emit(block), None);
        }
        assert_eq!(scopes.take(), ["outer", "inner"]);
    }

    #[test]
    fn replace_puts_other_blocks_in_place_and_keeps_the_old_ones() {
        let mut scopes = Scopes::default();
        scopes.hold();
        scopes.hold();
        scopes.emit("try 1 output");
        let kept = scopes.take();
        scopes.emit("↻ try 1");
        assert_eq!(kept, ["try 1 output"]);
        assert_eq!(scopes.take(), ["↻ try 1"]);
    }

    #[test]
    fn success_folds_failure_stays_full_and_short_output_never_folds() {
        assert_eq!(mode(true, false, FOLD_AT + 1), Mode::Folded);
        assert_eq!(mode(false, false, FOLD_AT + 1), Mode::Full);
        assert_eq!(mode(true, true, FOLD_AT + 1), Mode::Full);
        assert_eq!(mode(true, false, FOLD_AT), Mode::Full);
    }

    #[test]
    fn a_spool_keeps_arrival_order_and_streams() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("0001.out");
        let mut spool = Spool::create(path.clone()).unwrap();
        spool.push(Stream::Out, "a\nb\n");
        spool.push(Stream::Err, "warn\n");
        spool.push(Stream::Out, "c\n");
        spool.close();
        let mut seen = Vec::new();
        read_spool(&path, |stream, line| seen.push((stream, line.to_string()))).unwrap();
        assert_eq!(
            seen,
            [
                (Stream::Out, "a".to_string()),
                (Stream::Out, "b".into()),
                (Stream::Err, "warn".into()),
                (Stream::Out, "c".into()),
            ]
        );
        assert_eq!(spool_text(&path).unwrap(), "a\nb\nwarn\nc\n");
    }
}
