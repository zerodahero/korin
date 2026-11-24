//! The checked, expanded program that `plan` renders and `run` executes.
//!
//! Every name is resolved to a [`Sym`], every template call is expanded in place, and every
//! node keeps a [`Loc`] into the file it came from.

use std::path::PathBuf;
use std::sync::Arc;

use crate::ast::{CaptureMode, CmpOp, CommandKind, GateMode, Mode, SayKind, Span};
use crate::diag::SourceFile;

/// A resolved name.
pub type Sym = usize;

/// Where a node came from.
#[derive(Debug, Clone)]
pub struct Loc {
    pub file: Arc<SourceFile>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    Scalar,
    List,
    Json,
    Bool,
}

impl Ty {
    pub fn name(self) -> &'static str {
        match self {
            Ty::Scalar => "a scalar",
            Ty::List => "a list",
            Ty::Json => "JSON",
            Ty::Bool => "an `expect`",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymKind {
    Param,
    Ask,
    Capture,
    Expect,
    EachVar,
    /// `{korin.out}`: the per-invocation scratch directory.
    KorinOut,
}

#[derive(Debug, Clone)]
pub struct SymInfo {
    /// The name as the author wrote it, for example `src.status` or `value`.
    pub name: String,
    pub kind: SymKind,
    pub ty: Ty,
    pub secret: bool,
    pub optional: bool,
    /// The stage that defines it: `Some(n)` for stage n (1-based), `Some(0)` for the
    /// preflight, and `None` for params and `korin.out`.
    pub stage: Option<usize>,
    /// The step that produces it, for plan placeholders such as `<x ← 2.1>`.
    pub producer: Option<String>,
    /// For a capture: the expects of its step, which must all be false for it to exist.
    pub step_expects: Vec<Sym>,
    /// For a capture marked `on o`.
    pub on: Option<Sym>,
    /// For a capture: whether its step also has a secret capture, so it may be withheld.
    pub from_secret_step: bool,
    pub loc: Loc,
}

#[derive(Debug, Clone)]
pub struct Interpolated {
    pub parts: Vec<IPart>,
    pub loc: Loc,
}

impl Interpolated {
    pub fn literal(&self) -> Option<String> {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                IPart::Lit(text) => out.push_str(text),
                IPart::Sym { .. } => return None,
            }
        }
        Some(out)
    }

    pub fn syms(&self) -> impl Iterator<Item = Sym> + '_ {
        self.parts.iter().filter_map(|part| match part {
            IPart::Sym { sym, .. } => Some(*sym),
            IPart::Lit(_) => None,
        })
    }
}

#[derive(Debug, Clone)]
pub enum IPart {
    Lit(String),
    Sym { sym: Sym, splat: bool, loc: Loc },
}

#[derive(Debug, Clone)]
pub enum Expr {
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
    In(Box<Expr>, Box<Expr>),
    Regex(Box<Expr>, String),
    Exists(Sym),
    Sym(Sym),
    Str(Interpolated),
    Num(String),
    List(Vec<Expr>),
}

#[derive(Debug, Clone)]
pub struct Step {
    /// The static path, such as `3.2` or `3.2.1` inside a template or block.
    pub id: String,
    pub loc: Loc,
    pub kind: StepKind,
}

#[derive(Debug, Clone)]
pub enum StepKind {
    Command(Command),
    Confirm(Interpolated),
    Ask {
        sym: Sym,
        secret: bool,
        prompt: Interpolated,
    },
    Assert {
        cond: Expr,
        message: Interpolated,
    },
    When {
        cond: Expr,
        body: Vec<Step>,
    },
    Each {
        var: Sym,
        list: Expr,
        body: Vec<Step>,
    },
    Poll {
        every: u64,
        limit: u64,
        body: Vec<Step>,
        until: Expr,
        fail: Option<Expr>,
    },
    Say {
        kind: SayKind,
        text: Interpolated,
    },
    /// An expanded template call. `alias` names it in invocation IDs.
    Call {
        label: String,
        alias: Option<String>,
        body: Vec<Step>,
    },
}

#[derive(Debug, Clone)]
pub enum Exe {
    /// Looked up on `PATH` before stage 1.
    Name(String),
    /// Already resolved against the directory of the file that contains the step.
    Path(PathBuf),
}

impl Exe {
    pub fn display(&self) -> String {
        match self {
            Exe::Name(name) => name.clone(),
            Exe::Path(path) => path.display().to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Command {
    pub kind: CommandKind,
    pub exe: Exe,
    /// The full argv as written, including argv[0].
    pub argv: Vec<Interpolated>,
    pub captures: Vec<Capture>,
    pub expects: Vec<Expect>,
    pub env: Vec<(String, Interpolated)>,
    pub timeout: Option<u64>,
    /// What the output is, for the `o` picker's opener.
    pub output: Option<crate::ast::OutputKind>,
    /// True when the step has any `capture secret`, so its payload is staged.
    pub secret_producing: bool,
}

#[derive(Debug, Clone)]
pub struct Capture {
    pub sym: Sym,
    pub mode: CaptureMode,
    pub file: Option<Interpolated>,
    pub on: Option<Sym>,
    pub optional: bool,
    pub secret: bool,
}

#[derive(Debug, Clone)]
pub struct Expect {
    pub sym: Sym,
    pub matches: Vec<Match>,
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
pub struct Param {
    pub sym: Sym,
    pub required: bool,
    pub secret: bool,
    pub default: Option<Interpolated>,
}

#[derive(Debug, Clone)]
pub struct Stage {
    /// The stage's number (1-based), or 0 for the preflight.
    pub number: usize,
    pub title: String,
    /// What its `skip` line says, with `skip also` titles resolved to stage numbers.
    pub skip: SkipRule,
    pub steps: Vec<Step>,
    pub mutating: bool,
    pub loc: Loc,
}

/// What a stage's `skip` line says. See [`crate::ast::SkipRule`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SkipRule {
    /// No `skip` line: the stage can be skipped, and takes only its dependents with it.
    #[default]
    Free,
    Never,
    Ends,
    /// The stage numbers that skipping this stage also skips.
    Also(Vec<usize>),
}

impl Stage {
    pub fn is_preflight(&self) -> bool {
        self.number == 0
    }

    /// How a message names it: `stage 2`, or `the preflight`.
    pub fn name(&self) -> String {
        if self.is_preflight() {
            "the preflight".to_string()
        } else {
            format!("stage {}", self.number)
        }
    }
}

/// A use of a runtime value, recorded for the `--from` check.
#[derive(Debug, Clone)]
pub struct Reference {
    /// The stage that reads the value, or 0 for the preflight.
    pub stage: usize,
    /// Whether the read works without the value: an `exists` test, or a read inside
    /// `when exists`. A skip that leaves the value unset does not stop such a stage.
    pub guarded: bool,
    pub sym: Sym,
    pub loc: Loc,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub file: Arc<SourceFile>,
    pub title: String,
    pub gates: GateMode,
    pub mode: Mode,
    pub unattended: bool,
    pub params: Vec<Param>,
    /// The read-only block before stage 1, numbered 0.
    pub preflight: Option<Stage>,
    pub stages: Vec<Stage>,
    pub syms: Vec<SymInfo>,
    pub references: Vec<Reference>,
    pub korin_out: Sym,
}

impl Program {
    pub fn sym(&self, sym: Sym) -> &SymInfo {
        &self.syms[sym]
    }

    /// Why the run cannot start at `stage` (1-based): the uses of values from skipped stages.
    /// The preflight always runs, so its values never block a start.
    pub fn from_blockers(&self, stage: usize) -> Vec<&Reference> {
        self.references
            .iter()
            .filter(|u| {
                let info = &self.syms[u.sym];
                u.stage >= stage && info.stage.is_some_and(|s| s > 0 && s < stage)
            })
            .collect()
    }

    /// The stage numbered `number`: 0 for the preflight.
    pub fn stage(&self, number: usize) -> &Stage {
        if number == 0 {
            self.preflight.as_ref().expect("stage 0 is the preflight")
        } else {
            &self.stages[number - 1]
        }
    }

    /// The preflight, if any, then every stage, in run order.
    pub fn all_stages(&self) -> impl Iterator<Item = &Stage> {
        self.preflight.iter().chain(&self.stages)
    }

    /// Every executable the program may run.
    pub fn executables(&self) -> Vec<(&Exe, &Loc)> {
        fn walk<'a>(steps: &'a [Step], out: &mut Vec<(&'a Exe, &'a Loc)>) {
            for step in steps {
                match &step.kind {
                    StepKind::Command(c) => out.push((&c.exe, &step.loc)),
                    StepKind::When { body, .. }
                    | StepKind::Each { body, .. }
                    | StepKind::Poll { body, .. }
                    | StepKind::Call { body, .. } => walk(body, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        for stage in self.all_stages() {
            walk(&stage.steps, &mut out);
        }
        out
    }
}
