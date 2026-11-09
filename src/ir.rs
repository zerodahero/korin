//! The checked, expanded program that `plan` renders and `run` executes.
//!
//! Every name is resolved to a [`Sym`], every template call is expanded in place, and every
//! node keeps a [`Loc`] into the file it came from.

use std::path::PathBuf;
use std::sync::Arc;

use crate::ast::{CaptureMode, CmpOp, CommandKind, GateMode, SayKind, Span};
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
    /// The stage that defines it (1-based); 0 for params.
    pub stage: usize,
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
    pub number: usize,
    pub title: String,
    pub steps: Vec<Step>,
    pub mutating: bool,
    pub loc: Loc,
}

/// A use of a runtime value, recorded for the `--from` check.
#[derive(Debug, Clone)]
pub struct Reference {
    pub stage: usize,
    pub sym: Sym,
    pub loc: Loc,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub file: Arc<SourceFile>,
    pub title: String,
    pub gates: GateMode,
    pub unattended: bool,
    pub params: Vec<Param>,
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
    pub fn from_blockers(&self, stage: usize) -> Vec<&Reference> {
        self.references
            .iter()
            .filter(|u| {
                let info = &self.syms[u.sym];
                u.stage >= stage && info.stage > 0 && info.stage < stage
            })
            .collect()
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
        for stage in &self.stages {
            walk(&stage.steps, &mut out);
        }
        out
    }
}
