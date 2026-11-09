//! Text rendering shared by `plan`, gates and the journal.

use std::fmt::Write as _;

use crate::ast::{CaptureMode, CmpOp, GateMode, SayKind};
use crate::ir::{Expr, IPart, Interpolated, Program, Step, StepKind, Sym, SymKind};

/// Quotes one argument so it reads unambiguously, the way a shell would need it. Control
/// characters are escaped, so a value cannot rewrite the line it is shown on.
pub fn quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if plain {
        return arg.to_string();
    }
    if arg.chars().any(char::is_control) {
        let mut out = String::from("$'");
        for c in arg.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\'' => out.push_str("\\'"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => {
                    let _ = write!(out, "\\x{:02x}", u32::from(c));
                }
                c => out.push(c),
            }
        }
        out.push('\'');
        return out;
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
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
            c if c.is_control() => {
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

/// A command as the operator sees it: each argument quoted once it is fully known, so a value
/// with spaces reads as one argument and a control character cannot hide.
pub fn argv(program: &Program, argv: &[Interpolated], values: &dyn Values) -> String {
    argv.iter()
        .map(|t| argument(program, t, values))
        .collect::<Vec<_>>()
        .join(" ")
}

fn argument(program: &Program, t: &Interpolated, values: &dyn Values) -> String {
    if let [
        IPart::Sym {
            sym, splat: true, ..
        },
    ] = t.parts.as_slice()
        && let Some(items) = values.shown_arg(program, *sym)
    {
        return items.iter().map(|i| quote(i)).collect::<Vec<_>>().join(" ");
    }
    let mut whole = String::new();
    for part in &t.parts {
        match part {
            IPart::Lit(text) => whole.push_str(text),
            IPart::Sym { sym, .. } => match values.shown_arg(program, *sym) {
                Some(pieces) => whole.push_str(&pieces.join(" ")),
                // Not known yet: show the placeholder unquoted, so it reads as one.
                None => return interpolated(program, t, values, false),
            },
        }
    }
    quote(&whole)
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

/// The lines of one step and its body, indented by `depth`.
pub fn step_lines(
    program: &Program,
    step: &Step,
    depth: usize,
    values: &dyn Values,
    out: &mut Vec<String>,
) {
    let pad = "  ".repeat(depth);
    let id = &step.id;
    match &step.kind {
        StepKind::Command(c) => {
            out.push(format!(
                "{pad}{id}  {} $ {}",
                c.kind.keyword(),
                argv(program, &c.argv, values)
            ));
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
                step_lines(program, s, depth + 1, values, out);
            }
        }
        StepKind::Each { var, list, body } => {
            out.push(format!(
                "{pad}{id}  each {} in {}",
                program.sym(*var).name,
                expr(program, list, values)
            ));
            for s in body {
                step_lines(program, s, depth + 1, values, out);
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
                step_lines(program, s, depth + 1, values, out);
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
                step_lines(program, s, depth + 1, values, out);
            }
        }
    }
}

/// The whole static plan.
pub fn plan(program: &Program) -> String {
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
        out.push(format!(
            "▸ Stage {}/{total} · {}  [{}, {}; {from}]",
            stage.number,
            stage.title,
            if stage.mutating {
                "mutating"
            } else {
                "read-only"
            },
            if gated { "gate" } else { "no gate" },
        ));
        for step in &stage.steps {
            step_lines(program, step, 1, &Placeholders, &mut out);
        }
    }
    out.join("\n") + "\n"
}
