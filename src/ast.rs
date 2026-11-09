//! The syntax tree for `.korin` files.
//!
//! Every node that can be wrong carries a [`Span`] into its source file, so diagnostics can
//! point at the exact text.

use std::fmt;

/// A byte range in one source file.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn len(self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    #[must_use]
    pub fn join(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

impl From<Span> for miette::SourceSpan {
    fn from(span: Span) -> Self {
        (span.start, span.len()).into()
    }
}

/// A value with the span it came from.
#[derive(Clone, PartialEq, Eq)]
pub struct Spanned<T> {
    pub value: T,
    pub span: Span,
}

impl<T> Spanned<T> {
    pub fn new(value: T, span: Span) -> Self {
        Self { value, span }
    }
}

impl<T: fmt::Debug> fmt::Debug for Spanned<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} @ {:?}", self.value, self.span)
    }
}

pub type Ident = Spanned<String>;

/// A wizard file: header, imports, params and stages.
#[derive(Debug, Clone)]
pub struct Wizard {
    pub header: Header,
    pub uses: Vec<Use>,
    pub params: Vec<Param>,
    pub stages: Vec<Stage>,
}

/// A module file: imports and template definitions only.
#[derive(Debug, Clone)]
pub struct Module {
    pub uses: Vec<Use>,
    pub defines: Vec<Define>,
}

#[derive(Debug, Clone)]
pub struct Header {
    pub version: Spanned<u32>,
    pub title: Spanned<String>,
    pub gates: GateMode,
    pub unattended: Option<Span>,
    pub allow: Option<Vec<Spanned<String>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GateMode {
    #[default]
    All,
    Mutating,
}

#[derive(Debug, Clone)]
pub struct Use {
    pub target: UseTarget,
    pub alias: Option<Ident>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum UseTarget {
    /// `use aws`: a module built into korin.
    Builtin(Ident),
    /// `use "./x.korin"`: a file, relative to the importing file.
    Path(Spanned<String>),
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: Ident,
    pub kind: ParamKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ParamKind {
    Required,
    Secret,
    Default(Interpolated),
}

#[derive(Debug, Clone)]
pub struct Stage {
    pub title: Spanned<String>,
    pub steps: Vec<Step>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Define {
    pub name: Ident,
    pub params: Vec<Ident>,
    pub exports: Vec<Ident>,
    pub body: Vec<Step>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Step {
    Command(Command),
    Confirm {
        text: Interpolated,
        span: Span,
    },
    Ask {
        name: Ident,
        secret: bool,
        prompt: Interpolated,
        span: Span,
    },
    Assert {
        cond: Expr,
        message: Interpolated,
        span: Span,
    },
    When {
        cond: Expr,
        body: Vec<Step>,
        span: Span,
    },
    Each {
        var: Ident,
        list: Expr,
        body: Vec<Step>,
        span: Span,
    },
    Poll(Poll),
    Say {
        kind: SayKind,
        text: Interpolated,
        span: Span,
    },
    Call(Call),
}

impl Step {
    pub fn span(&self) -> Span {
        match self {
            Step::Command(c) => c.span,
            Step::Poll(p) => p.span,
            Step::Call(c) => c.span,
            Step::Confirm { span, .. }
            | Step::Ask { span, .. }
            | Step::Assert { span, .. }
            | Step::When { span, .. }
            | Step::Each { span, .. }
            | Step::Say { span, .. } => *span,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SayKind {
    Note,
    Warn,
    Record,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Read,
    Mutate,
    ScriptRead,
    ScriptMutate,
    Interactive,
}

impl CommandKind {
    /// Whether a stage containing this command needs a gate under `gates mutating`.
    pub fn is_mutating(self) -> bool {
        !matches!(self, CommandKind::Read | CommandKind::ScriptRead)
    }

    pub fn keyword(self) -> &'static str {
        match self {
            CommandKind::Read => "read",
            CommandKind::Mutate => "mutate",
            CommandKind::ScriptRead => "script read",
            CommandKind::ScriptMutate => "script mutate",
            CommandKind::Interactive => "interactive",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Command {
    pub kind: CommandKind,
    pub argv: Vec<Interpolated>,
    pub captures: Vec<Capture>,
    pub expects: Vec<Expect>,
    pub env: Vec<(Ident, Interpolated)>,
    pub timeout: Option<Spanned<Duration>>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Capture {
    pub name: Ident,
    pub secret: bool,
    pub optional: bool,
    pub file: Option<Interpolated>,
    pub mode: Spanned<CaptureMode>,
    pub on: Option<Ident>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureMode {
    Jq(String),
    JqList(String),
    JqJson(String),
    Text,
    Lines,
    Regex(String),
}

#[derive(Debug, Clone)]
pub struct Expect {
    pub name: Ident,
    pub matches: Vec<Spanned<Match>>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Match {
    Exit(i32),
    ExitNonZero,
    Signal(String),
    Stdout(String),
    Stderr(String),
    Missing(Interpolated),
}

#[derive(Debug, Clone)]
pub struct Poll {
    pub every: Spanned<Duration>,
    pub limit: Spanned<Duration>,
    pub body: Vec<Step>,
    pub until: Expr,
    pub fail: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Call {
    pub module: Ident,
    pub template: Ident,
    pub args: Vec<Interpolated>,
    pub alias: Option<Ident>,
    pub span: Span,
}

/// A duration in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Duration(pub u64);

/// A word or string with `{name}` interpolations.
#[derive(Debug, Clone)]
pub struct Interpolated {
    pub parts: Vec<Part>,
    pub span: Span,
}

impl Interpolated {
    /// The literal text, if the template has no interpolations.
    pub fn as_literal(&self) -> Option<String> {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Lit(text) => out.push_str(text),
                Part::Ref(_) => return None,
            }
        }
        Some(out)
    }

    pub fn refs(&self) -> impl Iterator<Item = &Ref> {
        self.parts.iter().filter_map(|part| match part {
            Part::Ref(r) => Some(r),
            Part::Lit(_) => None,
        })
    }
}

#[derive(Debug, Clone)]
pub enum Part {
    Lit(String),
    Ref(Ref),
}

/// A reference to a name: `cluster`, `src.status`, `korin.out`, or `sgs...` when splatted.
#[derive(Debug, Clone)]
pub struct Ref {
    pub path: Vec<String>,
    pub splat: bool,
    pub span: Span,
}

impl Ref {
    pub fn dotted(&self) -> String {
        self.path.join(".")
    }
}

#[derive(Debug, Clone)]
pub enum Expr {
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Not(Box<Expr>, Span),
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
    In(Box<Expr>, Box<Expr>),
    Regex(Box<Expr>, Spanned<String>),
    Exists(Ref),
    Ref(Ref),
    Str(Interpolated),
    Num(Spanned<String>),
    List(Vec<Expr>, Span),
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Or(a, b) | Expr::And(a, b) | Expr::Cmp(_, a, b) | Expr::In(a, b) => {
                a.span().join(b.span())
            }
            Expr::Not(e, span) => span.join(e.span()),
            Expr::Regex(e, re) => e.span().join(re.span),
            Expr::Exists(r) | Expr::Ref(r) => r.span,
            Expr::Str(t) => t.span,
            Expr::Num(n) => n.span,
            Expr::List(_, span) => *span,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}
