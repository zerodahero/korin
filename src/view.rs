//! The compact view of a run: shortened path prefixes and cut values, for the terminal only.
//!
//! The journal never sees this module's output. Every function here takes text that is
//! already redacted, and only removes or replaces text, so it cannot reveal a secret.

use unicode_width::UnicodeWidthStr as _;

use crate::render::{PREFIX_CLOSE, PREFIX_OPEN, VALUE_CLOSE, VALUE_OPEN, strip_marks};

/// How much of each path and value the terminal shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// Paths are shortened and huge values are cut.
    Compact,
    /// Everything shows as it runs.
    Full,
}

impl View {
    #[must_use]
    pub fn toggled(self) -> Self {
        match self {
            View::Compact => View::Full,
            View::Full => View::Compact,
        }
    }

    /// The name of the view the toggle key switches to, for a prompt's hint.
    pub fn other(self) -> &'static str {
        match self {
            View::Compact => "full",
            View::Full => "compact",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Prefix {
    /// The path, without trailing `/` characters.
    path: String,
    /// What the terminal shows instead: `<pwd>`, `~` or `<name>`.
    label: String,
    /// Whether the prefix also replaces the whole word, not only a path that continues.
    whole: bool,
}

/// The path prefixes the compact view shortens, longest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prefixes(Vec<Prefix>);

impl Prefixes {
    /// Adds a prefix. Trailing `/` characters are removed for matching, and the root never
    /// becomes a prefix. Among prefixes of the same length, the one added first wins.
    pub fn push(&mut self, path: &str, label: &str, whole: bool) {
        let path = path.trim_end_matches('/');
        if path.is_empty() || !path.starts_with('/') {
            return;
        }
        let at = self
            .0
            .iter()
            .position(|p| p.path.len() < path.len())
            .unwrap_or(self.0.len());
        self.0.insert(
            at,
            Prefix {
                path: path.to_string(),
                label: label.to_string(),
                whole,
            },
        );
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The prefix that starts `text`, as its label and the bytes it covers. A prefix matches
    /// when a `/` follows it, or, for a prefix that may replace a whole word, when
    /// `word_ends` says the word ends there.
    pub fn at(&self, text: &str, word_ends: impl Fn(&str) -> bool) -> Option<(&str, usize)> {
        self.0.iter().find_map(|p| {
            let rest = text.strip_prefix(p.path.as_str())?;
            (rest.starts_with('/') || (p.whole && word_ends(rest)))
                .then_some((p.label.as_str(), p.path.len()))
        })
    }

    /// Shortens every path in a piece of text, such as a line of output or a message. A
    /// prefix is replaced where a path starts: at the line start, or after whitespace, a
    /// quote, `=`, `:` or `(`. Each replacement is wrapped in prefix marks.
    pub fn shorten_text(&self, text: &str) -> String {
        if self.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut index = 0;
        let mut previous: Option<char> = None;
        while index < text.len() {
            let starts = previous.is_none_or(|c| c.is_whitespace() || "\"'=:(".contains(c));
            if starts
                && let Some((label, len)) = self.at(&text[index..], |rest| {
                    rest.chars()
                        .next()
                        .is_none_or(|c| c.is_whitespace() || "\"'),;:".contains(c))
                })
            {
                out.push(PREFIX_OPEN);
                out.push_str(label);
                out.push(PREFIX_CLOSE);
                index += len;
                previous = Some('/');
                continue;
            }
            let c = text[index..].chars().next().expect("index is in bounds");
            out.push(c);
            previous = Some(c);
            index += c.len_utf8();
        }
        out
    }
}

/// Where a scan of shell-quoted text stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Quote {
    None,
    /// Inside `'…'`.
    Single,
    /// Inside `$'…'`, where a backslash escapes the next character.
    Dollar,
}

/// Reads one shell token from the start of `text` (which must not be empty): a character,
/// an escape pair, or a `$'` opener. Returns the state after it and its length in bytes.
pub(crate) fn quote_step(state: Quote, text: &str) -> (Quote, usize) {
    let mut chars = text.chars();
    let c = chars.next().expect("quote_step needs text");
    let next = chars.next();
    let pair = |n: Option<char>| c.len_utf8() + n.map_or(0, char::len_utf8);
    match (state, c) {
        (Quote::None, '\\') => (Quote::None, pair(next)),
        (Quote::None, '$') if next == Some('\'') => (Quote::Dollar, 2),
        (Quote::None, '\'') => (Quote::Single, 1),
        (Quote::Single | Quote::Dollar, '\'') => (Quote::None, 1),
        (Quote::Dollar, '\\') => (Quote::Dollar, pair(next)),
        (state, c) => (state, c.len_utf8()),
    }
}

/// Scans `text` from `state` and returns the state at its end.
pub(crate) fn quote_scan(mut state: Quote, text: &str) -> Quote {
    let mut at = 0;
    while at < text.len() {
        let (next, len) = quote_step(state, &text[at..]);
        state = next;
        at += len;
    }
    state
}

/// One piece of a shown command, for splitting it into words and cutting them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A value mark: no width.
    Mark,
    /// Something a cut never splits: a prefix, a placeholder, or an escape.
    Atom,
    /// One character, or a shell escape pair outside quotes.
    Char,
    /// A space outside quotes and atoms: it ends a word.
    Space,
}

#[derive(Clone, Copy, Debug)]
struct Token {
    start: usize,
    end: usize,
    kind: Kind,
    /// The quote state after the token.
    after: Quote,
}

/// Splits shown command text into tokens.
fn tokens(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut state = Quote::None;
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let c = rest.chars().next().expect("at is in bounds");
        let span = |close: char| rest.find(close).map(|end| end + close.len_utf8());
        let (kind, len, next) = if c == VALUE_OPEN || c == VALUE_CLOSE {
            (Kind::Mark, c.len_utf8(), state)
        } else if c == PREFIX_OPEN {
            (Kind::Atom, span(PREFIX_CLOSE).unwrap_or(rest.len()), state)
        } else if c == '«'
            && let Some(len) = span('»')
        {
            (Kind::Atom, len, state)
        } else if c == '<'
            && state == Quote::None
            && let Some(len) = span('>')
        {
            (Kind::Atom, len, state)
        } else if state == Quote::Dollar && rest.starts_with("\\x") {
            // `\xHH`: one escaped character, shown as four.
            let len = rest
                .char_indices()
                .nth(4)
                .map_or(rest.len(), |(index, _)| index);
            (Kind::Atom, len, state)
        } else if c == ' ' && state == Quote::None {
            (Kind::Space, 1, state)
        } else {
            let (next, len) = quote_step(state, rest);
            (Kind::Char, len, next)
        };
        out.push(Token {
            start: at,
            end: at + len,
            kind,
            after: next,
        });
        state = next;
        at += len;
    }
    out
}

fn width_of(text: &str) -> usize {
    strip_marks(text).width()
}

/// A count of hidden characters, short: `87`, `1.2k`.
fn count(n: usize) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        #[allow(clippy::cast_precision_loss)]
        let thousands = n as f64 / 1000.0;
        format!("{thousands:.1}k")
    }
}

/// The text a cut adds: `…(+1.2k)`.
pub fn cut_suffix(hidden: usize) -> String {
    format!("…(+{})", count(hidden))
}

/// Cuts one shown word so it takes at most `budget` columns, suffix included. Quotes and
/// value marks stay balanced, and atoms are never split. Returns `None` when a cut would not
/// make the word shorter.
fn cut_word(word: &str, budget: usize) -> Option<String> {
    let toks = tokens(word);
    let total_chars = strip_marks(word).chars().count();
    // Reserve room for the widest suffix this word could need.
    let suffix_width = cut_suffix(total_chars).width();
    let mut kept = 0;
    let mut used = 0;
    let mut state = Quote::None;
    let mut value_open = false;
    for (index, tok) in toks.iter().enumerate() {
        let text = &word[tok.start..tok.end];
        let tok_width = width_of(text);
        let closing = usize::from(tok.after != Quote::None);
        let fits = used + tok_width + closing + suffix_width <= budget;
        // Keep at least one visible piece, so a cut always shows where the word starts.
        let first_visible = used == 0 && tok.kind != Kind::Mark;
        let opening = tok.kind == Kind::Char && state == Quote::None && tok.after != Quote::None;
        if !fits && !first_visible && !opening && tok.kind != Kind::Mark {
            break;
        }
        if tok.kind == Kind::Mark {
            value_open = text.starts_with(VALUE_OPEN);
        }
        used += tok_width;
        state = tok.after;
        kept = index + 1;
    }
    let tail = &word[toks.get(kept).map_or(word.len(), |t| t.start)..];
    let mut hidden = strip_marks(tail).chars().count();
    let mut out = word[..toks.get(kept).map_or(word.len(), |t| t.start)].to_string();
    if value_open {
        out.push(VALUE_CLOSE);
    }
    if state != Quote::None {
        out.push('\'');
        hidden = hidden.saturating_sub(1);
    }
    if hidden == 0 {
        return None;
    }
    out.push_str(&cut_suffix(hidden));
    (width_of(&out) < width_of(word)).then_some(out)
}

/// The words of a command body, as byte ranges, split on spaces outside quotes and atoms.
fn words(body: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for tok in tokens(body) {
        if tok.kind == Kind::Space {
            if let Some(s) = start.take() {
                out.push((s, tok.start));
            }
        } else if start.is_none() {
            start = Some(tok.start);
        }
    }
    if let Some(s) = start {
        out.push((s, body.len()));
    }
    out
}

/// Words narrower than this are never cut: the cut would hide too little to help.
const MIN_CUT: usize = 16;

/// Cuts one command line until it fits in `width` columns, widest word first. `body` is
/// where the command's words start.
fn cut_line(line: &str, body: usize, width: usize) -> String {
    let (main, tail) = match line.strip_suffix(" \\") {
        Some(main) => (main, " \\"),
        None => (line, ""),
    };
    let mut head = main[..body].to_string();
    let mut parts: Vec<String> = words(&main[body..])
        .into_iter()
        .map(|(s, e)| main[body + s..body + e].to_string())
        .collect();
    let mut done = vec![false; parts.len()];
    // Keep whatever separates the words (single spaces) by rebuilding with one space.
    let line_width =
        |head: &str, parts: &[String]| width_of(head) + width_of(&parts.join(" ")) + tail.width();
    while line_width(&head, &parts) > width {
        let Some(index) = (0..parts.len())
            .filter(|i| !done[*i] && width_of(&parts[*i]) >= MIN_CUT)
            .max_by_key(|i| width_of(&parts[*i]))
        else {
            break;
        };
        done[index] = true;
        let over = line_width(&head, &parts) - width;
        let budget = width_of(&parts[index])
            .saturating_sub(over)
            .max(MIN_CUT / 2);
        if let Some(cut) = cut_word(&parts[index], budget) {
            parts[index] = cut;
        }
    }
    head.push_str(&parts.join(" "));
    head.push_str(tail);
    head
}

/// Cuts the command lines of a block so each fits in `width` columns. Only lines that hold a
/// command change: a line with `❯ `, and the lines that continue it after a ` \`. A cut only
/// makes a line shorter, so where the lines break never changes.
pub fn cut_lines(text: &str, width: usize) -> String {
    let mut out = Vec::new();
    let mut continues = false;
    for line in text.split('\n') {
        let body = if continues {
            Some(line.len() - line.trim_start_matches(' ').len())
        } else {
            command_start(line)
        };
        let is_command = body.is_some();
        let shown = match body {
            Some(body) if width_of(line) > width => cut_line(line, body, width),
            _ => line.to_string(),
        };
        continues = is_command && line.ends_with(" \\");
        out.push(shown);
    }
    out.join("\n")
}

/// Where a command's words start in a line such as `  2.1 ❯ git status` or
/// `  2.1  read ❯ git status`: after the `❯ ` that follows a step number.
fn command_start(line: &str) -> Option<usize> {
    let arrow = line.find("❯ ")?;
    let before = line[..arrow].trim_start();
    let number = before.split(' ').next()?;
    let numbered = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit() || c == '.');
    numbered.then_some(arrow + "❯ ".len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Prefixes {
        let mut p = Prefixes::default();
        p.push("/home/ada", "~", true);
        p.push("/home/ada/shop", "<pwd>", true);
        p.push("/var/folders/ab/xyz/T/", "<tmp>", true);
        p.push("/opt/ci/build/", "<build_dir>", false);
        p
    }

    fn plain(text: &str) -> String {
        strip_marks(text)
    }

    #[test]
    fn the_longest_prefix_wins_at_a_path_boundary() {
        let p = table();
        assert_eq!(
            plain(&p.shorten_text("wrote /home/ada/shop/a.txt and /home/ada/b")),
            "wrote <pwd>/a.txt and ~/b"
        );
        // Not at a path boundary: a longer name that only starts the same.
        assert_eq!(
            plain(&p.shorten_text("/home/ada/shopping/x /home/adam")),
            "~/shopping/x /home/adam"
        );
        // Not where a path starts.
        assert_eq!(plain(&p.shorten_text("x/home/ada/y")), "x/home/ada/y");
    }

    #[test]
    fn trailing_slashes_are_ignored_for_matching() {
        let p = table();
        assert_eq!(
            plain(&p.shorten_text("/var/folders/ab/xyz/T/build.log")),
            "<tmp>/build.log"
        );
        assert_eq!(
            plain(&p.shorten_text("/opt/ci/build/app.tar")),
            "<build_dir>/app.tar"
        );
        let mut root = Prefixes::default();
        root.push("///", "<root>", true);
        assert!(root.is_empty());
    }

    #[test]
    fn a_named_prefix_never_replaces_a_whole_word() {
        let p = table();
        assert_eq!(
            plain(&p.shorten_text("cd /opt/ci/build")),
            "cd /opt/ci/build"
        );
        assert_eq!(plain(&p.shorten_text("cd /home/ada/shop")), "cd <pwd>");
        assert_eq!(
            plain(&p.shorten_text("(/home/ada/shop): '/home/ada'")),
            "(<pwd>): '~'"
        );
    }

    #[test]
    fn shortened_text_is_marked_for_color() {
        let p = table();
        assert_eq!(
            p.shorten_text("/home/ada/shop/x"),
            format!("{PREFIX_OPEN}<pwd>{PREFIX_CLOSE}/x")
        );
    }

    fn cut_one(line: &str, width: usize) -> String {
        let cut = cut_lines(line, width);
        assert!(width_of(&cut) <= width || cut == line, "{cut:?}");
        cut
    }

    #[test]
    fn a_huge_value_is_cut_with_balanced_marks() {
        let huge = format!("{VALUE_OPEN}{}{VALUE_CLOSE}", "a".repeat(200));
        let line = format!("  2.1 ❯ echo {huge}");
        let cut = cut_one(&line, 40);
        assert_eq!(cut.matches(VALUE_OPEN).count(), 1, "{cut:?}");
        assert_eq!(cut.matches(VALUE_CLOSE).count(), 1, "{cut:?}");
        assert!(
            plain(&cut).ends_with("…(+") || plain(&cut).contains("…(+"),
            "{cut:?}"
        );
        assert!(plain(&cut).starts_with("  2.1 ❯ echo aaaa"), "{cut:?}");
        assert_eq!(width_of(&cut), 40, "{cut:?}");
    }

    #[test]
    fn a_quoted_value_keeps_its_closing_quote() {
        let huge = format!("'{VALUE_OPEN}{}{VALUE_CLOSE} tail'", "b c ".repeat(50));
        let line = format!("  2.1  read ❯ echo {huge} next");
        let cut = cut_one(&line, 50);
        let shown = plain(&cut);
        assert!(shown.contains("echo 'b c"), "{shown}");
        assert!(shown.contains("'…(+"), "{shown}");
        assert!(shown.ends_with(" next"), "{shown}");
        assert_eq!(
            cut.matches(VALUE_OPEN).count(),
            cut.matches(VALUE_CLOSE).count()
        );
    }

    #[test]
    fn wide_characters_respect_the_column_budget() {
        let line = format!("  2.1 ❯ echo {}", "中".repeat(60));
        let cut = cut_one(&line, 40);
        assert!(width_of(&cut) <= 40, "{cut:?}");
        assert!(plain(&cut).contains("…(+"), "{cut:?}");
    }

    #[test]
    fn a_secret_placeholder_is_never_split() {
        let secret = format!("«secret:{}»", "t".repeat(60));
        let line = format!("  2.1 ❯ curl {secret}");
        assert_eq!(cut_lines(&line, 30), line);
        // With more after it, the cut falls after the placeholder.
        let line = format!("  2.1 ❯ curl {secret}{}", "x".repeat(40));
        let cut = cut_lines(&line, 30);
        assert!(plain(&cut).contains(&secret), "{cut:?}");
        assert!(plain(&cut).contains("…(+40)"), "{cut:?}");
    }

    #[test]
    fn escapes_and_prefixes_are_never_split() {
        let word = format!("$'{}\\x1b{}'", "a".repeat(10), "b".repeat(40));
        let cut = cut_word(&word, 16).unwrap();
        assert!(!cut.contains("\\x1'") && !cut.contains("\\x'"), "{cut}");
        let word = format!("{PREFIX_OPEN}<build_dir>{PREFIX_CLOSE}/{}", "c".repeat(40));
        let cut = cut_word(&word, 12).unwrap();
        assert!(
            cut.starts_with(&format!("{PREFIX_OPEN}<build_dir>{PREFIX_CLOSE}")),
            "{cut}"
        );
    }

    #[test]
    fn words_split_on_spaces_outside_quotes() {
        let body = r"git commit -m 'a b' 'it'\''s' $'x y' <snap ← 2.1> «secret:p q»";
        let split: Vec<&str> = words(body).into_iter().map(|(s, e)| &body[s..e]).collect();
        assert_eq!(
            split,
            [
                "git",
                "commit",
                "-m",
                "'a b'",
                r"'it'\''s'",
                "$'x y'",
                "<snap ← 2.1>",
                "«secret:p q»"
            ]
        );
    }

    #[test]
    fn only_command_lines_are_cut_and_breaks_hold() {
        let long = "z".repeat(80);
        let text =
            format!("\n  2.1 ❯ aws s3 cp \\\n          {long} \\\n          ./x\n      → name");
        let cut = cut_lines(&text, 40);
        let lines: Vec<&str> = cut.split('\n').collect();
        assert_eq!(lines.len(), 5);
        assert!(lines[2].ends_with(" \\"), "{cut}");
        assert!(plain(lines[2]).contains("…(+"), "{cut}");
        assert_eq!(lines[4], "      → name");
        // A note is not a command, however long.
        let note = format!("  2.2  note \"{long}\"");
        assert_eq!(cut_lines(&note, 40), note);
    }
}
