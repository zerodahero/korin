//! Text rendering shared by `plan`, gates and the journal.

use std::fmt::Write as _;

use unicode_width::UnicodeWidthStr as _;

use crate::ast::{CaptureMode, CmpOp, GateMode, SayKind};
use crate::ir::{Expr, IPart, Interpolated, Program, Step, StepKind, Sym, SymKind};

/// Opens a value korin put into a command, in a rendered line for the terminal. The theme turns
/// each marked value into color, and every line loses its marks before it is shown.
pub const VALUE_OPEN: char = '\u{e000}';
/// Closes a value opened by [`VALUE_OPEN`].
pub const VALUE_CLOSE: char = '\u{e001}';

fn is_mark(c: char) -> bool {
    c == VALUE_OPEN || c == VALUE_CLOSE
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
    let whole: String = segments.iter().map(|(text, _)| *text).collect();
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
    for (text, marked) in segments {
        let marked = *marked && !text.is_empty();
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
    let one = words.join(" ");
    let end = words
        .iter()
        .position(|w| strip_marks(w) == "--")
        .unwrap_or(words.len());
    let flags = words[..end].iter().filter(|w| is_flag(w)).count();
    if indent + strip_marks(&one).width() <= width && flags < BREAK_FLAGS {
        return vec![one];
    }
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
        return vec![one];
    }
    let pad = " ".repeat(indent + 2);
    let mut lines = vec![words[..head].join(" ")];
    lines.extend(groups.into_iter().map(|g| format!("{pad}{g}")));
    let last = lines.len() - 1;
    for line in &mut lines[..last] {
        line.push_str(" \\");
    }
    lines
}

/// The shown words of a command, one per argument (a known list gives one per item).
fn words(program: &Program, argv: &[Interpolated], values: &dyn Values) -> Vec<String> {
    let mut out = Vec::new();
    for t in argv {
        if let [
            IPart::Sym {
                sym, splat: true, ..
            },
        ] = t.parts.as_slice()
            && let Some(items) = values.shown_arg(program, *sym)
        {
            out.extend(items.iter().map(|i| quote_segments(&[(i, true)])));
            continue;
        }
        out.push(argument(program, t, values));
    }
    out
}

fn argument(program: &Program, t: &Interpolated, values: &dyn Values) -> String {
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
    quote_segments(&borrowed)
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
                    interpolated(program, value, values, false)
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
        }
        StepKind::Confirm(text) => {
            out.push(format!(
                "{pad}{id}  confirm \"{}\"",
                interpolated(program, text, values, false)
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
            interpolated(program, prompt, values, false)
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
                interpolated(program, text, values, false)
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
    let total = program.stages.len();
    for stage in &program.stages {
        out.push(String::new());
        let gated = program.gates == GateMode::All || stage.mutating;
        let blockers = program.from_blockers(stage.number);
        let from = match blockers.first() {
            None => "valid --from start".to_string(),
            Some(u) => {
                let (line, _) = u.loc.file.line_col(u.loc.span.start);
                format!(
                    "not a --from start: uses `{}` (line {line})",
                    program.sym(u.sym).name
                )
            }
        };
        out.push(stage_rule(stage.number, total, &stage.title, width));
        out.push(format!(
            "  {}, {}; {from}",
            if stage.mutating {
                "mutating"
            } else {
                "read-only"
            },
            if gated { "gate" } else { "no gate" },
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
