//! Text rendering shared by `plan`, gates and the journal.

use std::fmt::Write as _;

use unicode_width::UnicodeWidthStr as _;

use crate::ast::{CaptureMode, CmpOp, GateMode, SayKind};
use crate::ir::{Expr, IPart, Interpolated, Program, Step, StepKind, Sym, SymKind};
use crate::view::Prefixes;

/// Opens a value korin put into a command, in a rendered line for the terminal. The theme turns
/// each marked value into color, and every line loses its marks before it is shown.
pub const VALUE_OPEN: char = '\u{e000}';
/// Closes a value opened by [`VALUE_OPEN`].
pub const VALUE_CLOSE: char = '\u{e001}';
/// Opens a shortened path prefix, such as `<pwd>`, in a rendered line for the terminal.
pub const PREFIX_OPEN: char = '\u{e002}';
/// Closes a prefix opened by [`PREFIX_OPEN`].
pub const PREFIX_CLOSE: char = '\u{e003}';

fn is_mark(c: char) -> bool {
    matches!(c, VALUE_OPEN | VALUE_CLOSE | PREFIX_OPEN | PREFIX_CLOSE)
}

/// Removes value marks, for anything that is not the terminal, such as the journal.
pub fn strip_marks(text: &str) -> String {
    text.chars().filter(|c| !is_mark(*c)).collect()
}

/// Quotes one argument so it reads unambiguously, the way a shell would need it. Control
/// characters are escaped, so a value cannot rewrite the line it is shown on.
pub fn quote(arg: &str) -> String {
    quote_segments(&[(arg, false)])
}

/// Quotes one argument built from pieces, marking the pieces that are values. The quoting
/// depends on the whole argument, so the marks never change how it is quoted.
fn quote_segments(segments: &[(&str, bool)]) -> String {
    let pieces: Vec<Piece<'_>> = segments
        .iter()
        .map(|(text, marked)| {
            if *marked {
                Piece::Value(text)
            } else {
                Piece::Lit(text)
            }
        })
        .collect();
    quote_pieces(&pieces)
}

/// A piece of one shown argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Piece<'a> {
    Lit(&'a str),
    /// A value korin put into the command: marked, so the theme can color it.
    Value(&'a str),
    /// A shortened path prefix: `label` is shown instead of `path`.
    Prefix {
        label: &'a str,
        path: &'a str,
    },
}

impl Piece<'_> {
    /// The text the command really receives.
    fn real(&self) -> &str {
        match self {
            Piece::Lit(text) | Piece::Value(text) => text,
            Piece::Prefix { path, .. } => path,
        }
    }
}

/// Quotes one argument built from pieces. Quoting is decided by the real argument, so a
/// shortened prefix never changes how the argument is quoted; its label is shown as is.
fn quote_pieces(pieces: &[Piece<'_>]) -> String {
    let whole: String = pieces.iter().map(Piece::real).collect();
    let plain = !whole.is_empty()
        && whole
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    let escaped = !plain && whole.chars().any(|c| c.is_control() || is_mark(c));
    let mut out = String::new();
    if escaped {
        out.push_str("$'");
    } else if !plain {
        out.push('\'');
    }
    for piece in pieces {
        let (text, marked) = match piece {
            Piece::Prefix { label, .. } => {
                out.push(PREFIX_OPEN);
                out.push_str(&escape_controls(label));
                out.push(PREFIX_CLOSE);
                continue;
            }
            Piece::Lit(text) => (*text, false),
            Piece::Value(text) => (*text, true),
        };
        let marked = marked && !text.is_empty();
        if marked {
            out.push(VALUE_OPEN);
        }
        for c in text.chars() {
            if escaped {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\'' => out.push_str("\\'"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c.is_control() || is_mark(c) => {
                        let _ = write!(out, "\\x{:02x}", u32::from(c));
                    }
                    c => out.push(c),
                }
            } else if !plain && c == '\'' {
                out.push_str(r"'\''");
            } else {
                out.push(c);
            }
        }
        if marked {
            out.push(VALUE_CLOSE);
        }
    }
    if !plain {
        out.push('\'');
    }
    out
}

/// Shortens the path that starts an argument, or that follows the `=` of `--flag=`. A named
/// prefix applies only when a `/` follows it; the pieces are split where the prefix ends.
fn shorten_pieces<'a>(
    prefixes: &'a Prefixes,
    segments: &[(&'a str, bool)],
    whole: &'a str,
) -> Vec<Piece<'a>> {
    let as_pieces = || {
        segments
            .iter()
            .map(|(t, m)| if *m { Piece::Value(t) } else { Piece::Lit(t) })
            .collect::<Vec<_>>()
    };
    debug_assert_eq!(
        whole,
        segments.iter().map(|(t, _)| *t).collect::<String>(),
        "whole is the segments joined"
    );
    let start = if whole.starts_with('-') {
        whole.find('=').map_or(0, |i| i + 1)
    } else {
        0
    };
    let Some((label, len)) = prefixes.at(&whole[start..], str::is_empty) else {
        return as_pieces();
    };
    let (from, to) = (start, start + len);
    let path = &whole[from..to];
    let mut out = Vec::new();
    let mut offset = 0;
    let mut placed = false;
    for (text, marked) in segments {
        let (s, e) = (offset, offset + text.len());
        offset = e;
        let piece = |t: &'a str| {
            if *marked {
                Piece::Value(t)
            } else {
                Piece::Lit(t)
            }
        };
        if e <= from || s >= to {
            if s >= to && !placed {
                out.push(Piece::Prefix { label, path });
                placed = true;
            }
            out.push(piece(text));
            continue;
        }
        if s < from {
            out.push(piece(&text[..from - s]));
        }
        if !placed {
            out.push(Piece::Prefix { label, path });
            placed = true;
        }
        if e > to {
            out.push(piece(&text[to - s..]));
        }
    }
    if !placed {
        out.push(Piece::Prefix { label, path });
    }
    out
}

/// Escapes control characters in text shown inside a message, so a value cannot add lines or
/// move the cursor.
pub fn escape_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || is_mark(c) => {
                let _ = write!(out, "\\x{:02x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// How a name renders when its value is not shown.
pub trait Values {
    /// The value to show for `sym` inside a message, or `None` to show a placeholder.
    fn shown(&self, program: &Program, sym: Sym) -> Option<String>;

    /// The raw argument pieces `sym` contributes to a command (one per list item), with
    /// secrets and withheld values already masked, or `None` to show a placeholder.
    fn shown_arg(&self, _program: &Program, _sym: Sym) -> Option<Vec<String>> {
        None
    }

    /// The path prefixes to shorten, in the compact view; `None` shows every path in full.
    fn prefixes(&self) -> Option<Prefixes> {
        None
    }

    /// Lines to show under a `script` step, such as the script itself, each starting with
    /// `pad`. Only a run's previews show any.
    fn script_lines(&self, _command: &crate::ir::Command, _pad: &str) -> Vec<String> {
        Vec::new()
    }
}

/// One shown argument: shortened when there are prefixes, then quoted.
fn shown_word(prefixes: Option<&Prefixes>, segments: &[(&str, bool)]) -> String {
    match prefixes {
        Some(prefixes) => {
            let whole: String = segments.iter().map(|(t, _)| *t).collect();
            quote_pieces(&shorten_pieces(prefixes, segments, &whole))
        }
        None => quote_segments(segments),
    }
}

/// Text inside a message, shortened when there are prefixes.
fn shown_text(values: &dyn Values, text: String) -> String {
    match values.prefixes() {
        Some(prefixes) => prefixes.shorten_text(&text),
        None => text,
    }
}

/// Shows every name as a placeholder. Used by `korin plan`.
pub struct Placeholders;

impl Values for Placeholders {
    fn shown(&self, _: &Program, _: Sym) -> Option<String> {
        None
    }
}

/// The placeholder for a name: `<snap_arn ← 3.1>`, `<cluster>`, or `«secret:token»`.
pub fn placeholder(program: &Program, sym: Sym) -> String {
    let info = program.sym(sym);
    if info.secret {
        return format!("«secret:{}»", info.name);
    }
    match (&info.kind, &info.producer) {
        (SymKind::KorinOut, _) => "<korin.out>".into(),
        (_, Some(step)) => format!("<{} ← {step}>", info.name),
        _ => format!("<{}>", info.name),
    }
}

pub fn interpolated(
    program: &Program,
    t: &Interpolated,
    values: &dyn Values,
    quote_parts: bool,
) -> String {
    let mut out = String::new();
    for part in &t.parts {
        match part {
            IPart::Lit(text) => out.push_str(text),
            IPart::Sym { sym, splat, .. } => {
                let text = values
                    .shown(program, *sym)
                    .unwrap_or_else(|| placeholder(program, *sym));
                out.push_str(&text);
                if *splat && values.shown(program, *sym).is_none() {
                    out.push_str("...");
                }
            }
        }
    }
    if quote_parts && t.parts.iter().all(|p| matches!(p, IPart::Lit(_))) {
        return quote(&out);
    }
    out
}

/// The widest a stage rule gets, however wide the terminal is.
pub const RULE_MAX: usize = 80;
/// The width to lay out for when the output is not a terminal.
pub const DEFAULT_WIDTH: usize = 100;
/// A command with this many flags is broken across lines, even when it fits.
const BREAK_FLAGS: usize = 3;

/// The header that starts a stage: `━━ Stage 2/4 · Title ━━━━`, filled to `width` (at most
/// [`RULE_MAX`]).
pub fn stage_rule(number: usize, total: usize, title: &str, width: usize) -> String {
    let head = format!("━━ Stage {number}/{total} · {} ", escape_controls(title));
    let fill = width.min(RULE_MAX).saturating_sub(head.width()).max(2);
    head + &"━".repeat(fill)
}

/// The header that starts the preflight: `━━ Preflight · Title ━━━━`.
pub fn preflight_rule(title: &str, width: usize) -> String {
    let head = format!("━━ Preflight · {} ", escape_controls(title));
    let fill = width.min(RULE_MAX).saturating_sub(head.width()).max(2);
    head + &"━".repeat(fill)
}

/// A command as the operator sees it, on one line: each argument quoted once it is fully
/// known, so a value with spaces reads as one argument and a control character cannot hide.
pub fn argv(program: &Program, argv: &[Interpolated], values: &dyn Values) -> String {
    words(program, argv, values).join(" ")
}

/// A command laid out to read well. It stays on one line when it fits in `width` after a
/// prefix `indent` columns wide and has few flags. Otherwise the executable and its leading
/// words take the first line, and each flag (with its value) or later word takes a line of
/// its own. Every line but the last ends in ` \`, and later lines are indented to two
/// columns past the executable, so the lines still paste into a shell as one command.
///
/// Joining the lines with [`join_breaks`] gives exactly [`argv`].
pub fn command_lines(
    program: &Program,
    argv: &[Interpolated],
    values: &dyn Values,
    indent: usize,
    width: usize,
) -> Vec<String> {
    layout(&words(program, argv, values), indent, width)
}

/// Undoes the breaks [`command_lines`] adds: each ` \`, newline and indent becomes a space.
pub fn join_breaks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut lines = text.split('\n').peekable();
    while let Some(line) = lines.next() {
        match line.strip_suffix(" \\") {
            Some(body) if lines.peek().is_some_and(|next| next.starts_with(' ')) => {
                out.push_str(body);
                out.push(' ');
                if let Some(next) = lines.peek_mut() {
                    *next = next.trim_start_matches(' ');
                }
            }
            _ => {
                out.push_str(line);
                if lines.peek().is_some() {
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// Whether a shown word is a flag: it starts with `-` and is more than a lone `-`.
fn is_flag(word: &str) -> bool {
    let plain = strip_marks(word);
    plain.len() > 1 && plain.starts_with('-') && plain != "--"
}

fn layout(words: &[String], indent: usize, width: usize) -> Vec<String> {
    layout_at(words, indent, width, 2)
}

/// Lays out a command whose first word sits `indent` columns in. Its continuation lines
/// go `step` columns past that.
///
/// When the words after a `--` start like a command (not like a flag), they are a nested
/// command, as in `fnox exec -- flux bootstrap github …`. The `--` ends the outer part's
/// last line, and the nested command starts on the next line at the outer continuation
/// indent, laid out by the same rule with its own continuations 4 columns deeper.
fn layout_at(words: &[String], indent: usize, width: usize, step: usize) -> Vec<String> {
    let one = words.join(" ");
    let end = words
        .iter()
        .position(|w| strip_marks(w) == "--")
        .unwrap_or(words.len());
    let flags = words[..end].iter().filter(|w| is_flag(w)).count();
    if indent + strip_marks(&one).width() <= width && flags < BREAK_FLAGS {
        return vec![one];
    }
    let nested = words
        .get(end + 1)
        .is_some_and(|w| !strip_marks(w).starts_with('-'));
    let outer = if nested { &words[..end] } else { words };
    let Some(mut lines) =
        break_words(outer, end, indent, width, step).or(nested.then(|| vec![outer.join(" ")]))
    else {
        return vec![one];
    };
    if !nested {
        return lines;
    }
    // The outer part's last line takes the `--` and a break.
    if let Some(last) = lines.last_mut() {
        last.push_str(" -- \\");
    }
    let inner_indent = indent + step;
    let mut inner = layout_at(&words[end + 1..], inner_indent, width, 4).into_iter();
    if let Some(first) = inner.next() {
        lines.push(format!("{}{first}", " ".repeat(inner_indent)));
    }
    lines.extend(inner);
    lines
}

/// The broken form of a command's words: the executable and its leading words, then each
/// flag (with its value) or later word on a line of its own. `end` is where a `--` is, if
/// any; flags pair with values only before it. `None` when nothing follows the head.
fn break_words(
    words: &[String],
    end: usize,
    indent: usize,
    width: usize,
    step: usize,
) -> Option<Vec<String>> {
    let head = 1 + words
        .iter()
        .skip(1)
        .take_while(|w| !is_flag(w) && strip_marks(w) != "--")
        .count();
    let mut head = head.min(words.len());
    // Leading words that are too wide for one line, such as long positional paths, each take
    // a line of their own. If even the executable alone does not fit, breaking cannot help.
    let full_head = head;
    let head_width = |n: usize| indent + strip_marks(&words[..n].join(" ")).width();
    while head > 1 && head_width(head) > width {
        head -= 1;
    }
    if head_width(head) > width {
        head = full_head;
    }
    let end = end.min(words.len());
    let mut groups: Vec<String> = Vec::new();
    let mut index = head;
    while index < words.len() {
        let word = &words[index];
        let pairs = index < end
            && is_flag(word)
            && !strip_marks(word).contains('=')
            && index + 1 < end
            && !is_flag(&words[index + 1]);
        if pairs {
            groups.push(format!("{word} {}", words[index + 1]));
            index += 2;
        } else {
            groups.push(word.clone());
            index += 1;
        }
    }
    if groups.is_empty() {
        return None;
    }
    let pad = " ".repeat(indent + step);
    let mut lines = vec![words[..head].join(" ")];
    lines.extend(groups.into_iter().map(|g| format!("{pad}{g}")));
    let last = lines.len() - 1;
    for line in &mut lines[..last] {
        line.push_str(" \\");
    }
    Some(lines)
}

/// The shown words of a command, one per argument (a known list gives one per item).
fn words(program: &Program, argv: &[Interpolated], values: &dyn Values) -> Vec<String> {
    let prefixes = values.prefixes();
    let prefixes = prefixes.as_ref();
    let mut out = Vec::new();
    for t in argv {
        if let [
            IPart::Sym {
                sym, splat: true, ..
            },
        ] = t.parts.as_slice()
            && let Some(items) = values.shown_arg(program, *sym)
        {
            out.extend(items.iter().map(|i| shown_word(prefixes, &[(i, true)])));
            continue;
        }
        out.push(argument(program, t, values, prefixes));
    }
    out
}

fn argument(
    program: &Program,
    t: &Interpolated,
    values: &dyn Values,
    prefixes: Option<&Prefixes>,
) -> String {
    let mut segments: Vec<(String, bool)> = Vec::new();
    for part in &t.parts {
        match part {
            IPart::Lit(text) => segments.push((text.clone(), false)),
            IPart::Sym { sym, .. } => match values.shown_arg(program, *sym) {
                Some(pieces) => segments.push((pieces.join(" "), true)),
                // Not known yet: show the placeholder unquoted, so it reads as one.
                None => return interpolated(program, t, values, false),
            },
        }
    }
    let borrowed: Vec<(&str, bool)> = segments.iter().map(|(t, m)| (t.as_str(), *m)).collect();
    shown_word(prefixes, &borrowed)
}

/// The name a `$` line gives the scratch directory's match; it is not a secret.
const KORIN_OUT: &str = "korin.out";

/// A command's own variables, each with its value, leading a `$` line as `NAME=value`.
pub type ShellEnv = Vec<(String, Segments)>;

/// One word of a `$` line as real pieces: the text the command receives, and whether korin
/// put it in (a value), so the theme can color it.
pub type Segments = Vec<(String, bool)>;

/// A command as a shell line for the run log: pasteable, with each exact secret as a braced
/// shell variable.
#[derive(Debug, Default)]
pub struct ShellLine {
    /// The command, laid out like [`command_lines`]; the first line starts with `$ `.
    pub lines: Vec<String>,
    /// The exact secrets the line names as variables, each once, in order.
    pub vars: Vec<crate::redact::SecretMatch>,
    /// Secrets shown only as `«secret:name»`, because the line holds part of one; the line
    /// cannot be pasted as it is.
    pub partial: Vec<String>,
}

/// Builds the `$` line for a command from its real words, before any redaction. `env` holds
/// the command's own variables, which lead the line as `NAME=value`.
///
/// `korin_out` is the step's scratch directory. The command receives it as `KORIN_OUT`, and it
/// is gone once the step ends, so the line shows it as `${KORIN_OUT}`.
pub fn shell_line(
    env: &[(String, Segments)],
    words: &[Segments],
    redactor: &crate::redact::Redactor,
    korin_out: Option<&str>,
    width: Option<usize>,
) -> ShellLine {
    let mut line = ShellLine::default();
    let mut shown = Vec::new();
    for (name, value) in env {
        shown.push(format!(
            "{}={}",
            escape_controls(name),
            shell_word(value, redactor, korin_out, &mut line)
        ));
    }
    for word in words {
        shown.push(shell_word(word, redactor, korin_out, &mut line));
    }
    if shown.is_empty() {
        return line;
    }
    let laid = match width {
        Some(width) => layout(&shown, 2, width),
        None => vec![shown.join(" ")],
    };
    let mut laid = laid.into_iter();
    line.lines
        .push(format!("$ {}", laid.next().unwrap_or_default()));
    line.lines.extend(laid);
    line
}

/// One word of a `$` line. A word with no secret is quoted like any shown argument. A word
/// with a secret is double-quoted: each exact secret becomes `${VAR}`, braced so a letter
/// after it cannot change the name, and `\ " $` and backticks in the rest are escaped.
fn shell_word(
    segments: &Segments,
    redactor: &crate::redact::Redactor,
    korin_out: Option<&str>,
    line: &mut ShellLine,
) -> String {
    let whole: String = segments.iter().map(|(t, _)| t.as_str()).collect();
    let mut found = redactor.matches(&whole);
    if let Some(dir) = korin_out.filter(|d| !d.is_empty()) {
        for (start, _) in whole.match_indices(dir) {
            let end = start + dir.len();
            if !found.iter().any(|m| m.start < end && start < m.end) {
                found.push(crate::redact::SecretMatch {
                    start,
                    end,
                    name: KORIN_OUT.into(),
                    var: Some("KORIN_OUT".into()),
                    label: "the step's scratch directory".into(),
                });
            }
        }
        found.sort_by_key(|m| m.start);
    }
    if found.is_empty() {
        let borrowed: Vec<(&str, bool)> = segments.iter().map(|(t, m)| (t.as_str(), *m)).collect();
        return quote_segments(&borrowed);
    }
    // Which segment each byte belongs to, so values keep their marks.
    let mut marked_at = Vec::with_capacity(whole.len());
    for (text, marked) in segments {
        marked_at.extend(std::iter::repeat_n(*marked, text.len()));
    }
    let mut out = String::from("\"");
    let mut open = false;
    let set_mark = |out: &mut String, on: bool, open: &mut bool| {
        if on != *open {
            out.push(if on { VALUE_OPEN } else { VALUE_CLOSE });
            *open = on;
        }
    };
    let mut next = found.iter().peekable();
    let mut at = 0;
    while at < whole.len() {
        if let Some(m) = next.peek()
            && m.start == at
        {
            set_mark(&mut out, true, &mut open);
            if let Some(var) = &m.var {
                let _ = write!(out, "${{{var}}}");
                if m.name != KORIN_OUT && !line.vars.iter().any(|v| v.var == m.var) {
                    line.vars.push((*m).clone());
                }
            } else {
                let _ = write!(out, "«secret:{}»", m.name);
                if !line.partial.contains(&m.name) {
                    line.partial.push(m.name.clone());
                }
            }
            at = m.end;
            next.next();
            continue;
        }
        let c = whole[at..].chars().next().unwrap_or_default();
        set_mark(
            &mut out,
            marked_at.get(at).copied().unwrap_or(false),
            &mut open,
        );
        match c {
            '\\' | '"' | '$' | '`' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() || is_mark(c) => {
                let _ = write!(out, "\\x{:02x}", u32::from(c));
            }
            c => out.push(c),
        }
        at += c.len_utf8();
    }
    set_mark(&mut out, false, &mut open);
    out.push('"');
    out
}

/// How tightly an expression binds: `or` 1, `and` 2, `not` 3, everything else 4.
fn precedence(e: &Expr) -> u8 {
    match e {
        Expr::Or(..) => 1,
        Expr::And(..) => 2,
        Expr::Not(..) => 3,
        _ => 4,
    }
}

/// Renders `e`, in parentheses when it binds more loosely than its context needs, so the
/// operator reads the same grouping the runner evaluates.
fn grouped(program: &Program, e: &Expr, values: &dyn Values, needs: u8) -> String {
    let text = expr(program, e, values);
    if precedence(e) < needs {
        format!("({text})")
    } else {
        text
    }
}

pub fn expr(program: &Program, e: &Expr, values: &dyn Values) -> String {
    match e {
        Expr::Or(a, b) => format!(
            "{} or {}",
            grouped(program, a, values, 1),
            grouped(program, b, values, 1)
        ),
        Expr::And(a, b) => format!(
            "{} and {}",
            grouped(program, a, values, 2),
            grouped(program, b, values, 2)
        ),
        Expr::Not(inner) => format!("not {}", grouped(program, inner, values, 3)),
        Expr::Cmp(op, a, b) => {
            let op = match op {
                CmpOp::Eq => "==",
                CmpOp::Ne => "!=",
                CmpOp::Lt => "<",
                CmpOp::Le => "<=",
                CmpOp::Gt => ">",
                CmpOp::Ge => ">=",
            };
            format!(
                "{} {op} {}",
                expr(program, a, values),
                expr(program, b, values)
            )
        }
        Expr::In(a, b) => format!(
            "{} in {}",
            expr(program, a, values),
            expr(program, b, values)
        ),
        Expr::Regex(a, re) => format!("{} ~ /{re}/", expr(program, a, values)),
        Expr::Exists(sym) => format!("exists {}", program.sym(*sym).name),
        Expr::Sym(sym) => program.sym(*sym).name.clone(),
        Expr::Str(t) => format!("\"{}\"", interpolated(program, t, values, false)),
        Expr::Num(n) => n.clone(),
        Expr::List(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|i| expr(program, i, values))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The lines of one step and its body, indented by `depth`. With a `width`, long commands
/// break across lines ([`command_lines`]); without one, each command takes one line.
pub fn step_lines(
    program: &Program,
    step: &Step,
    depth: usize,
    values: &dyn Values,
    width: Option<usize>,
    out: &mut Vec<String>,
) {
    let pad = "  ".repeat(depth);
    let id = &step.id;
    match &step.kind {
        StepKind::Command(c) => {
            let prefix = format!("{pad}{id}  {} ❯ ", c.kind.keyword());
            let lines = match width {
                Some(width) => command_lines(program, &c.argv, values, prefix.width(), width),
                None => vec![argv(program, &c.argv, values)],
            };
            let mut lines = lines.into_iter();
            out.push(format!("{prefix}{}", lines.next().unwrap_or_default()));
            out.extend(lines);
            for (name, value) in &c.env {
                out.push(format!(
                    "{pad}      env {name}={}",
                    shown_text(values, interpolated(program, value, values, false))
                ));
            }
            for expect in &c.expects {
                out.push(format!("{pad}      ? {}", program.sym(expect.sym).name));
            }
            let captures: Vec<String> = c
                .captures
                .iter()
                .map(|cap| {
                    let info = program.sym(cap.sym);
                    let mut s = info.name.clone();
                    if cap.optional {
                        s.push('?');
                    }
                    if cap.secret {
                        s.push_str(" (secret)");
                    }
                    if let Some(on) = cap.on {
                        let _ = write!(s, " on {}", program.sym(on).name);
                    }
                    if matches!(cap.mode, CaptureMode::JqList(_) | CaptureMode::Lines) {
                        s.push_str(" [list]");
                    }
                    s
                })
                .collect();
            if !captures.is_empty() {
                out.push(format!("{pad}      → {}", captures.join(", ")));
            }
            out.extend(values.script_lines(c, &pad));
        }
        StepKind::Confirm(text) => {
            out.push(format!(
                "{pad}{id}  confirm \"{}\"",
                shown_text(values, interpolated(program, text, values, false))
            ));
        }
        StepKind::Ask {
            sym,
            secret,
            prompt,
        } => out.push(format!(
            "{pad}{id}  ask{} {} \"{}\"",
            if *secret { " secret" } else { "" },
            program.sym(*sym).name,
            shown_text(values, interpolated(program, prompt, values, false))
        )),
        StepKind::Assert { cond, message } => out.push(format!(
            "{pad}{id}  assert {} else \"{}\"",
            expr(program, cond, values),
            interpolated(program, message, values, false)
        )),
        StepKind::When { cond, body } => {
            out.push(format!("{pad}{id}  when {}", expr(program, cond, values)));
            for s in body {
                step_lines(program, s, depth + 1, values, width, out);
            }
        }
        StepKind::Each { var, list, body } => {
            out.push(format!(
                "{pad}{id}  each {} in {}",
                program.sym(*var).name,
                expr(program, list, values)
            ));
            for s in body {
                step_lines(program, s, depth + 1, values, width, out);
            }
        }
        StepKind::Poll {
            every,
            limit,
            body,
            until,
            fail,
        } => {
            out.push(format!("{pad}{id}  poll every {every}s for {limit}s"));
            for s in body {
                step_lines(program, s, depth + 1, values, width, out);
            }
            out.push(format!("{pad}      until {}", expr(program, until, values)));
            if let Some(fail) = fail {
                out.push(format!("{pad}      fail {}", expr(program, fail, values)));
            }
        }
        StepKind::Say { kind, text } => {
            let keyword = match kind {
                SayKind::Note => "note",
                SayKind::Warn => "warn",
                SayKind::Record => "record",
            };
            out.push(format!(
                "{pad}{id}  {keyword} \"{}\"",
                shown_text(values, interpolated(program, text, values, false))
            ));
        }
        StepKind::Call { label, body, .. } => {
            out.push(format!("{pad}{id}  {label}"));
            for s in body {
                step_lines(program, s, depth + 1, values, width, out);
            }
        }
    }
}

/// The whole static plan, with commands laid out for `width` columns.
pub fn plan(program: &Program, width: usize) -> String {
    let mut out = Vec::new();
    out.push(program.title.clone());
    let guide = program.mode == crate::ast::Mode::Guide;
    if guide {
        out.push("mode: guide".into());
    }
    let params: Vec<String> = program
        .params
        .iter()
        .map(|p| {
            let name = &program.sym(p.sym).name;
            if p.required {
                format!("{name} (required)")
            } else if p.secret {
                format!("{name} (secret)")
            } else if let Some(d) = &p.default {
                format!(
                    "{name} = \"{}\"",
                    interpolated(program, d, &Placeholders, false)
                )
            } else {
                name.clone()
            }
        })
        .collect();
    if !params.is_empty() {
        out.push(format!("params: {}", params.join(", ")));
    }
    if let Some(preflight) = &program.preflight {
        out.push(String::new());
        let gated = program.gates == GateMode::All || preflight.mutating;
        out.push(preflight_rule(&preflight.title, width));
        out.push(format!(
            "  read-only, {}; runs first, even with --from{}",
            if guide {
                "pause"
            } else if gated {
                "gate"
            } else {
                "pause"
            },
            if preflight.retry {
                "; retry on failure"
            } else {
                ""
            },
        ));
        for step in &preflight.steps {
            step_lines(program, step, 1, &Placeholders, Some(width), &mut out);
        }
    }
    let total = program.stages.len();
    for stage in &program.stages {
        out.push(String::new());
        let gated = program.gates == GateMode::All || stage.mutating;
        let mut needs: Vec<String> = Vec::new();
        for u in program.from_blockers(stage.number) {
            let name = format!("`{}`", program.sym(u.sym).name);
            if !u.guarded && !needs.contains(&name) {
                needs.push(name);
            }
        }
        let from = if needs.is_empty() {
            "valid --from start".to_string()
        } else {
            format!("a --from start asks for {}", needs.join(", "))
        };
        let skip = match &stage.skip {
            crate::ir::SkipRule::Free => String::new(),
            crate::ir::SkipRule::Never => "; skip never".to_string(),
            crate::ir::SkipRule::Ends => "; skip ends the run".to_string(),
            crate::ir::SkipRule::Also(targets) => {
                let targets: Vec<String> = targets.iter().map(ToString::to_string).collect();
                format!("; skip also skips stage {}", targets.join(", "))
            }
        };
        out.push(stage_rule(stage.number, total, &stage.title, width));
        let retry = if stage.retry {
            "; retry on failure"
        } else {
            ""
        };
        out.push(format!(
            "  {}, {}; {from}{skip}{retry}",
            if stage.mutating {
                "mutating"
            } else {
                "read-only"
            },
            if guide {
                "pause"
            } else if gated {
                "gate"
            } else {
                "no gate"
            },
        ));
        for step in &stage.steps {
            step_lines(program, step, 1, &Placeholders, Some(width), &mut out);
        }
    }
    out.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::Redactor;

    /// Runs a `$` line in `sh` with `vars` exported and returns the argv it receives, one
    /// argument per line.
    fn through_shell(line: &ShellLine, vars: &[(&str, &str)]) -> Vec<String> {
        let text = strip_marks(&join_breaks(&line.lines.join("\n")));
        let command = text.strip_prefix("$ ").expect("a $ line");
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("set -- {command}; printf '%s\\n' \"$@\""))
            .env_clear()
            .envs(vars.iter().copied())
            .output()
            .expect("sh runs");
        // `set --` takes every word, so the first one is the command itself.
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .skip(1)
            .map(str::to_string)
            .collect()
    }

    fn words(words: &[&[(&str, bool)]]) -> Vec<Segments> {
        words
            .iter()
            .map(|w| w.iter().map(|(t, m)| ((*t).to_string(), *m)).collect())
            .collect()
    }

    #[test]
    fn shell_lines_round_trip_through_sh() {
        let r = Redactor::new();
        r.add("token", "s3cr3t-token").unwrap();
        r.add("db.password", "pa$$ \"word\"").unwrap();
        let vars = [("TOKEN", "s3cr3t-token"), ("DB_PASSWORD", "pa$$ \"word\"")];
        let cases: Vec<(Vec<Segments>, Vec<&str>)> = vec![
            (
                words(&[&[("echo", false)], &[("plain", false)]]),
                vec!["plain"],
            ),
            (
                words(&[&[("echo", false)], &[("with space", true)]]),
                vec!["with space"],
            ),
            (
                words(&[&[("echo", false)], &[("s3cr3t-token", true)]]),
                vec!["s3cr3t-token"],
            ),
            (
                words(&[
                    &[("psql", false)],
                    &[
                        ("postgres://admin:", false),
                        ("pa$$ \"word\"", true),
                        ("@db/app", false),
                    ],
                ]),
                vec!["postgres://admin:pa$$ \"word\"@db/app"],
            ),
            (
                words(&[
                    &[("echo", false)],
                    &[("$HOME-", false), ("s3cr3t-token", true)],
                ]),
                vec!["$HOME-s3cr3t-token"],
            ),
            (
                words(&[
                    &[("echo", false)],
                    &[("s3cr3t-token", true), ("suffix", false)],
                ]),
                vec!["s3cr3t-tokensuffix"],
            ),
        ];
        for (argv, expected) in cases {
            let line = shell_line(&[], &argv, &r, None, None);
            assert_eq!(through_shell(&line, &vars), expected, "{:?}", line.lines);
        }
        let line = shell_line(
            &[],
            &words(&[
                &[("echo", false)],
                &[("s3cr3t-token", true), ("suffix", false)],
            ]),
            &r,
            None,
            None,
        );
        assert!(
            strip_marks(&line.lines[0]).contains("${TOKEN}suffix"),
            "{:?}",
            line.lines
        );
        assert_eq!(line.vars.len(), 1);
    }

    #[test]
    fn shell_lines_name_each_value_its_own_variable() {
        let r = Redactor::new();
        r.add("foo.bar", "first-value").unwrap();
        r.add("foo_bar", "second-value").unwrap();
        r.add_value_at("tokens", Some(0), "item-one-value").unwrap();
        r.add_value_at("tokens", Some(1), "item-two-value").unwrap();
        let argv = words(&[
            &[("echo", false)],
            &[("first-value", true)],
            &[("second-value", true)],
            &[("item-one-value", true)],
            &[("item-two-value", true)],
        ]);
        let line = shell_line(&[], &argv, &r, None, None);
        let vars = [
            ("FOO_BAR", "first-value"),
            ("FOO_BAR_2", "second-value"),
            ("TOKENS_1", "item-one-value"),
            ("TOKENS_2", "item-two-value"),
        ];
        assert_eq!(
            through_shell(&line, &vars),
            [
                "first-value",
                "second-value",
                "item-one-value",
                "item-two-value"
            ]
        );

        // A recaptured value gets a new variable; a line built before keeps the old one.
        let r = Redactor::new();
        r.add_value("token", "old-token-value").unwrap();
        let old = shell_line(
            &[],
            &words(&[&[("echo", false)], &[("old-token-value", true)]]),
            &r,
            None,
            None,
        );
        r.add_value("token", "new-token-value").unwrap();
        let new = shell_line(
            &[],
            &words(&[&[("echo", false)], &[("new-token-value", true)]]),
            &r,
            None,
            None,
        );
        assert!(
            strip_marks(&old.lines[0]).contains("${TOKEN}"),
            "{:?}",
            old.lines
        );
        assert!(
            strip_marks(&new.lines[0]).contains("${TOKEN_2}"),
            "{:?}",
            new.lines
        );
        let vars = [("TOKEN", "old-token-value"), ("TOKEN_2", "new-token-value")];
        assert_eq!(through_shell(&old, &vars), ["old-token-value"]);
        assert_eq!(through_shell(&new, &vars), ["new-token-value"]);
    }

    #[test]
    fn a_derived_secret_form_is_shown_redacted_and_not_pasteable() {
        let r = Redactor::new();
        r.add_value("creds", r#"{"key":"leaf-secret-value"}"#)
            .unwrap();
        let whole = shell_line(
            &[],
            &words(&[
                &[("echo", false)],
                &[(r#"{"key":"leaf-secret-value"}"#, true)],
            ]),
            &r,
            None,
            None,
        );
        assert_eq!(whole.partial, Vec::<String>::new());
        assert!(strip_marks(&whole.lines[0]).contains("${CREDS}"));
        let leaf = shell_line(
            &[],
            &words(&[&[("echo", false)], &[("leaf-secret-value", true)]]),
            &r,
            None,
            None,
        );
        assert_eq!(leaf.partial, ["creds"]);
        assert!(leaf.lines[0].contains("«secret:creds»"));
        assert!(!leaf.lines[0].contains("leaf-secret-value"));
    }

    #[test]
    fn the_scratch_directory_shows_as_korin_out() {
        let r = Redactor::new();
        let dir = "/tmp/korin-123/0001-s1.1";
        let line = shell_line(
            &[],
            &words(&[&[("nvim", false)], &[(dir, true), ("/notes.md", false)]]),
            &r,
            Some(dir),
            None,
        );
        assert_eq!(
            strip_marks(&line.lines[0]),
            "$ nvim \"${KORIN_OUT}/notes.md\""
        );
        assert!(line.vars.is_empty(), "{:?}", line.vars);
        assert_eq!(
            through_shell(&line, &[("KORIN_OUT", dir)]),
            [format!("{dir}/notes.md")]
        );
    }

    #[test]
    fn env_leads_the_shell_line_as_an_assignment() {
        let r = Redactor::new();
        r.add("token", "s3cr3t-token").unwrap();
        let line = shell_line(
            &[("API_TOKEN".into(), vec![("s3cr3t-token".into(), true)])],
            &words(&[
                &[("sh", false)],
                &[("-c", false)],
                &[("echo $API_TOKEN", false)],
            ]),
            &r,
            None,
            None,
        );
        assert_eq!(
            strip_marks(&line.lines[0]),
            "$ API_TOKEN=\"${TOKEN}\" sh -c 'echo $API_TOKEN'"
        );
    }

    #[test]
    fn marks_never_change_how_an_argument_is_quoted() {
        for (pieces, expected) in [
            (vec![("hi ", false), ("susan", true)], "'hi susan'"),
            (vec![("--id=", false), ("abc", true)], "--id=abc"),
            (vec![("a\n", true), ("b", false)], "$'a\\nb'"),
            (vec![("it's", true)], r"'it'\''s'"),
        ] {
            let marked = quote_segments(&pieces);
            assert_eq!(strip_marks(&marked), expected, "{pieces:?}");
            let whole: String = pieces.iter().map(|(t, _)| *t).collect();
            assert_eq!(strip_marks(&marked), quote(&whole), "{pieces:?}");
        }
        assert_eq!(
            quote_segments(&[("hi ", false), ("susan", true)]),
            "'hi \u{e000}susan\u{e001}'"
        );
    }

    #[test]
    fn a_mark_inside_a_value_is_escaped_not_trusted() {
        assert_eq!(quote("x\u{e000}y"), "$'x\\xe000y'");
        assert_eq!(escape_controls("x\u{e001}y"), "x\\xe001y");
    }

    fn laid_out(words: &[&str], indent: usize, width: usize) -> Vec<String> {
        let words: Vec<String> = words.iter().map(|w| (*w).to_string()).collect();
        let lines = layout(&words, indent, width);
        assert_eq!(
            join_breaks(&lines.join("\n")),
            words.join(" "),
            "{lines:#?}"
        );
        lines
    }

    #[test]
    fn a_short_command_stays_on_one_line() {
        assert_eq!(
            laid_out(&["git", "branch", "--show-current"], 8, 100),
            ["git branch --show-current"]
        );
        assert_eq!(laid_out(&["echo", "a", "b", "c"], 8, 10), ["echo a b c"]);
    }

    #[test]
    fn three_flags_break_even_when_the_line_fits() {
        assert_eq!(
            laid_out(
                &[
                    "git",
                    "ls-remote",
                    "--tags",
                    "--refs",
                    "--sort=-version:refname",
                    "origin"
                ],
                8,
                100
            ),
            [
                "git ls-remote \\",
                "          --tags \\",
                "          --refs \\",
                "          --sort=-version:refname \\",
                "          origin",
            ]
        );
    }

    #[test]
    fn a_wide_command_breaks_and_pairs_flags_with_values() {
        assert_eq!(
            laid_out(
                &[
                    "aws",
                    "rds",
                    "describe",
                    "--profile",
                    "prod",
                    "--file",
                    "/a/very/long/path.json"
                ],
                8,
                40
            ),
            [
                "aws rds describe \\",
                "          --profile prod \\",
                "          --file /a/very/long/path.json",
            ]
        );
    }

    #[test]
    fn long_positional_words_break_onto_their_own_lines() {
        assert_eq!(
            laid_out(
                &[
                    "aws",
                    "s3",
                    "cp",
                    "s3://my-bucket/some/really/long/key/name.tar.gz",
                    "./downloads/name.tar.gz"
                ],
                8,
                40
            ),
            [
                "aws s3 cp \\",
                "          s3://my-bucket/some/really/long/key/name.tar.gz \\",
                "          ./downloads/name.tar.gz",
            ]
        );
        // One huge word after the executable still gets a line of its own.
        let huge = "x".repeat(60);
        assert_eq!(
            laid_out(&["echo", &huge], 0, 40),
            ["echo \\".to_string(), format!("  {huge}")]
        );
    }

    #[test]
    fn flags_after_a_double_dash_are_words() {
        assert_eq!(
            laid_out(&["git", "log", "-n", "1", "--", "--weird-file", "x"], 0, 10),
            [
                "git log \\",
                "  -n 1 \\",
                "  -- \\",
                "  --weird-file \\",
                "  x",
            ]
        );
    }

    #[test]
    fn words_after_a_double_dash_lay_out_as_a_nested_command() {
        let words = [
            "fnox",
            "exec",
            "--",
            "flux",
            "bootstrap",
            "github",
            "--kubeconfig",
            "kubeconfig",
            "--owner",
            "zerodahero",
            "--repository",
            "homelab",
            "--branch",
            "main",
            "--path",
            "clusters/g7",
            "--private",
            "--personal",
            "--components",
            "source-controller,kustomize-controller,helm-controller",
        ];
        // As a gate shows it: the step line is indented 2 by its depth.
        let prefix = "  3.2.2  mutate ❯ ";
        let lines = laid_out(&words, prefix.width(), 80);
        let mut shown = vec![format!("{prefix}{}", lines[0])];
        shown.extend(lines[1..].iter().cloned());
        // The example drops the depth's 2 columns from the first line only.
        shown[0] = shown[0][2..].to_string();
        assert_eq!(
            shown,
            [
                "3.2.2  mutate ❯ fnox exec -- \\",
                "                    flux bootstrap github \\",
                "                        --kubeconfig kubeconfig \\",
                "                        --owner zerodahero \\",
                "                        --repository homelab \\",
                "                        --branch main \\",
                "                        --path clusters/g7 \\",
                "                        --private \\",
                "                        --personal \\",
                "                        --components source-controller,kustomize-controller,helm-controller",
            ]
        );
    }

    #[test]
    fn a_nested_command_breaks_by_its_own_rule() {
        let long = "x".repeat(30);
        assert_eq!(
            laid_out(&["sudo", "-u", "app", "--", "ls", "-l", &long], 0, 30),
            [
                "sudo \\".to_string(),
                "  -u app -- \\".into(),
                "  ls \\".into(),
                format!("      -l {long}"),
            ]
        );
    }

    #[test]
    fn a_double_dash_inside_a_nested_command_nests_again() {
        let lines = laid_out(
            &[
                "a", "--", "b", "--x", "1", "--", "c", "--y", "2", "--z", "3", "--w", "4",
            ],
            0,
            20,
        );
        assert_eq!(lines[0], "a -- \\");
        assert!(lines[1].starts_with("  b "), "{lines:#?}");
        assert!(lines.iter().any(|l| l.starts_with("      c")), "{lines:#?}");
    }

    #[test]
    fn a_flag_pairs_with_a_lone_dash_but_not_with_another_flag() {
        assert_eq!(
            laid_out(&["x", "--a", "--b", "-", "--c=1"], 0, 5),
            ["x \\", "  --a \\", "  --b - \\", "  --c=1"]
        );
    }

    #[test]
    fn marked_values_and_placeholders_lay_out_like_words() {
        let flag = "\u{e000}--id\u{e001}";
        let lines = laid_out(&["x", flag, "<snap ← 2.1>", "--y", "'a b'", "--z"], 0, 100);
        assert_eq!(
            lines,
            [
                "x \\".to_string(),
                format!("  {flag} <snap ← 2.1> \\"),
                "  --y 'a b' \\".to_string(),
                "  --z".to_string(),
            ]
        );
    }

    fn prefixes() -> Prefixes {
        let mut p = Prefixes::default();
        p.push("/home/ada/shop", "<pwd>", true);
        p.push("/opt/ci/build/", "<build_dir>", false);
        p
    }

    fn shown(segments: &[(&str, bool)]) -> String {
        shown_word(Some(&prefixes()), segments)
    }

    const P: (char, char) = (PREFIX_OPEN, PREFIX_CLOSE);

    #[test]
    fn a_path_argument_is_shortened_before_it_is_quoted() {
        let (o, c) = P;
        // Quoting follows the real argument: a space quotes it, once, around the label.
        assert_eq!(
            shown(&[("/home/ada/shop/my file.txt", false)]),
            format!("'{o}<pwd>{c}/my file.txt'")
        );
        assert_eq!(
            shown(&[("/home/ada/shop/a.txt", false)]),
            format!("{o}<pwd>{c}/a.txt")
        );
        assert_eq!(
            shown(&[("--out=/home/ada/shop/x", false)]),
            format!("--out={o}<pwd>{c}/x")
        );
        // Only where an argument starts.
        assert_eq!(shown(&[("x/home/ada/shop/a", false)]), "x/home/ada/shop/a");
    }

    #[test]
    fn a_value_that_holds_the_prefix_is_split_where_it_ends() {
        let (o, c) = P;
        assert_eq!(
            shown(&[("/opt/ci/build", true), ("/app.tar", false)]),
            format!("{o}<build_dir>{c}/app.tar")
        );
        assert_eq!(
            shown(&[("/opt/ci/build/app", true), (".tar", false)]),
            format!("{o}<build_dir>{c}{VALUE_OPEN}/app{VALUE_CLOSE}.tar")
        );
        // A named prefix never replaces the whole word: the value is the point.
        assert_eq!(
            shown(&[("/opt/ci/build", true)]),
            format!("{VALUE_OPEN}/opt/ci/build{VALUE_CLOSE}")
        );
        assert_eq!(shown(&[("/home/ada/shop", true)]), format!("{o}<pwd>{c}"));
    }

    #[test]
    fn shortening_can_put_a_broken_command_on_one_line() {
        let long = |name: &str| format!("/home/ada/shop/{name}");
        let full: Vec<String> = ["cp".to_string(), long("source.tar"), long("target.tar")]
            .into_iter()
            .map(|w| quote(&w))
            .collect();
        assert!(layout(&full, 8, 50).len() > 1);
        let compact: Vec<String> = ["cp".to_string(), long("source.tar"), long("target.tar")]
            .iter()
            .map(|w| shown(&[(w, false)]))
            .collect();
        let lines = layout(&compact, 8, 50);
        assert_eq!(lines.len(), 1, "{lines:?}");
        // Each prefix is replaced once.
        assert_eq!(
            strip_marks(&lines[0]),
            "cp <pwd>/source.tar <pwd>/target.tar"
        );
    }

    #[test]
    fn the_stage_rule_fills_to_the_width_with_a_cap() {
        let rule = stage_rule(2, 4, "Greet", 40);
        assert_eq!(rule.width(), 40);
        assert!(rule.starts_with("━━ Stage 2/4 · Greet ━"), "{rule}");
        assert_eq!(stage_rule(1, 1, "x", 200).width(), RULE_MAX);
        assert!(stage_rule(1, 1, &"t".repeat(100), 40).ends_with(" ━━"));
    }

    #[test]
    fn join_breaks_leaves_other_lines_alone() {
        assert_eq!(join_breaks("a \\\n  b\nc \\"), "a b\nc \\");
    }
}
