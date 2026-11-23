//! Running a compiled program.
//!
//! The runner walks the stages in order. Each stage is shown first and, when it has a gate,
//! approved by the human before any of its commands run.

pub mod capture;
pub mod journal;
pub mod process;
mod screen;

use screen::Screen;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use serde_json::json;

use crate::ast::{CmpOp, CommandKind, GateMode, Mode, SayKind, Span};
use crate::diag::Diag;
use crate::ir::{
    self, Command, Expr, IPart, Interpolated, Match, Program, SkipRule, Step, StepKind, Sym,
    SymInfo, SymKind, Ty,
};
use crate::redact::{Redactor, StreamRedactor};
use crate::render::{self, Values as _};
use crate::skip::{self, Why};
use crate::theme::Theme;
use crate::view::{Prefixes, View};
use capture::{Data, Extract, Fingerprint, extract};
use journal::Journal;
use process::{Cancel, Signals, Spec, Status, Suspended};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// The default timeout for `read`, `mutate` and `script` steps.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_mins(15);

/// Resolves `--from` to a 1-based stage number.
///
/// # Errors
///
/// Returns a diagnostic if no stage matches.
pub fn resolve_from(program: &Program, from: &str) -> Result<usize, Box<Diag>> {
    resolve_stage(program, from, "from")
}

/// Resolves a stage given by title or number, as `--from` and `--skip` take it.
fn resolve_stage(program: &Program, name: &str, flag: &'static str) -> Result<usize, Box<Diag>> {
    name.parse::<usize>()
        .ok()
        .filter(|n| (1..=program.stages.len()).contains(n))
        .or_else(|| {
            program
                .stages
                .iter()
                .find(|s| s.title == name)
                .map(|s| s.number)
        })
        .ok_or_else(|| {
            let titles: Vec<String> = program
                .stages
                .iter()
                .map(|s| format!("{} \"{}\"", s.number, s.title))
                .collect();
            Box::new(
                Diag::error(
                    &program.file,
                    flag,
                    Span::default(),
                    format!("no stage matches `{name}`"),
                )
                .help(format!("stages are: {}", titles.join(", "))),
            )
        })
}

/// What a run that starts at `stage` will ask for: each value a later stage reads that a
/// stage before it makes. A run with a terminal asks for them by hand; without one, they
/// come from `--set`, `--param` or `KORIN_SECRET_<NAME>`.
pub fn from_warnings(program: &Program, stage: usize) -> Vec<Diag> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for r in program.from_blockers(stage) {
        if r.guarded || seen.contains(&r.sym) {
            continue;
        }
        seen.push(r.sym);
        let info = program.sym(r.sym);
        let number = info.stage.unwrap_or(0);
        let how = match info.kind {
            SymKind::Ask if info.secret => format!("set {}", secret_env(&info.name)),
            SymKind::Ask => format!("pass --param {}=...", info.name),
            _ if info.secret => format!("set {}", secret_env(&info.name)),
            _ => format!("pass --set {}=...", info.name),
        };
        out.push(
            Diag::warning(
                &r.loc.file,
                "from",
                r.loc.span,
                format!(
                    "starting at stage {stage} asks for `{}`, made in skipped {} \"{}\"",
                    info.name,
                    program.stage(number).name(),
                    program.stage(number).title
                ),
            )
            .label("used here")
            .help(format!(
                "a run in a terminal asks for it; without one, {how}, or start at stage {number}"
            )),
        );
    }
    out
}

#[allow(clippy::struct_excessive_bools)] // independent command-line switches
pub struct Options {
    pub params: Vec<(String, String)>,
    pub yes: bool,
    pub from: Option<String>,
    /// `--skip`: stages, by title or number, not to run.
    pub skip: Vec<String>,
    /// `--skip-preflight`.
    pub skip_preflight: bool,
    /// `--set name=value`: values for captures and expects of skipped stages.
    pub set: Vec<(String, String)>,
    pub grace: Duration,
    /// Gates and `confirm` read a whole line, ending with Enter, instead of a single key.
    pub require_enter: bool,
    /// The view a run in a terminal starts in. Without a terminal, the view is always full.
    pub view: View,
    /// Whether the compact view starts with its legend of path prefixes shown.
    pub legend: bool,
    /// A script with at most this many lines shows in full in a preview; 0 never shows it.
    pub script_lines: usize,
    /// The pager command for `o`, from the config file; else `$PAGER`, else `less`.
    pub pager: Option<String>,
    /// Where run directories go, normally `.korin/runs`.
    pub runs_dir: PathBuf,
    /// The directory commands run in.
    pub cwd: PathBuf,
    /// Colors for the terminal; plain when stdout is not one.
    pub theme: Theme,
    /// The command line, shown in the run's intro.
    pub invocation: String,
}

/// How a run ended. The exit code follows from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Completed,
    Failed,
    Cancelled,
    /// The operator skipped a `skip ends` stage: the run stopped early, by choice.
    Ended,
}

impl Ending {
    pub fn exit_code(self) -> u8 {
        match self {
            Ending::Completed | Ending::Ended => 0,
            Ending::Failed => 1,
            Ending::Cancelled => 130,
        }
    }
}

#[derive(Debug)]
struct Stop {
    ending: Ending,
    message: String,
    details: Vec<String>,
    /// Set when the enclosing poll's deadline ended the step; the poll reports it as its own
    /// timeout, with the last values.
    poll_deadline: bool,
    /// Set when the operator skipped the rest of the stage at a `confirm`; the stage ends
    /// there, and the run goes on.
    skip_rest: bool,
}

impl Stop {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            ending: Ending::Failed,
            message: message.into(),
            details: Vec::new(),
            poll_deadline: false,
            skip_rest: false,
        }
    }

    fn poll_deadline() -> Self {
        Self {
            poll_deadline: true,
            ..Self::failed("the poll deadline passed")
        }
    }

    fn cancelled(message: impl Into<String>) -> Self {
        Self {
            ending: Ending::Cancelled,
            message: message.into(),
            details: Vec::new(),
            poll_deadline: false,
            skip_rest: false,
        }
    }

    fn ended(message: impl Into<String>) -> Self {
        Self {
            ending: Ending::Ended,
            ..Self::cancelled(message)
        }
    }

    fn skip_rest() -> Self {
        Self {
            skip_rest: true,
            ..Self::failed("the rest of the stage was skipped")
        }
    }

    fn detail(mut self, line: impl Into<String>) -> Self {
        self.details.push(line.into());
        self
    }
}

/// A script's bytes as read before its stage's first prompt, and their hash.
struct Pin {
    bytes: Vec<u8>,
    hash: Vec<u8>,
}

/// A key that answers a prompt. See [`Runner::prompt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Yes,
    No,
    Continue,
    /// Switch between the compact and the full view, then ask again.
    ToggleView,
    /// Show or hide the legend, then ask again.
    ToggleLegend,
    /// Open the values view, then ask again.
    Values,
    /// Skip the stage, or the rest of it at a `confirm`.
    Skip,
    /// Open a script in the pager, then ask again.
    Open,
}

impl Key {
    /// What the prompt line shows once the key answers it.
    fn echo(self) -> &'static str {
        match self {
            Key::Yes => "y",
            Key::No => "n",
            Key::Continue => "",
            Key::ToggleView => "f",
            Key::ToggleLegend => "l",
            Key::Values => "v",
            Key::Skip => "s",
            Key::Open => "o",
        }
    }
}

/// The keys a prompt allows, beyond its own answers.
#[derive(Debug, Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)] // one switch per key
struct Keys {
    /// `s`: skip the stage, or the rest of it.
    skip: bool,
    /// `f`: switch between the compact and the full view.
    view: bool,
    /// `l`: show or hide the legend. Offered only in the compact view, which has one.
    legend: bool,
    /// `v`: open the values view.
    values: bool,
    /// `o`: open a script in the pager. Set by [`Runner::ask`] when the steps to come hold
    /// a script.
    open: bool,
}

impl Keys {
    /// The hint after the question, such as `y/N · f full`, or after a pause.
    fn hint(self, style: &Style<'_, '_>, view: View) -> String {
        let mut parts = Vec::new();
        if matches!(style, Style::Question(_)) {
            parts.push("y/N".to_string());
        }
        if self.skip {
            parts.push("s skip".to_string());
        }
        if self.values {
            parts.push("v values".to_string());
        }
        if self.view {
            parts.push(format!("f {}", view.other()));
        }
        if self.legend {
            parts.push("l legend".to_string());
        }
        if self.open {
            parts.push("o open script".to_string());
        }
        if parts.is_empty() {
            "Ctrl-C cancel".to_string()
        } else {
            parts.join(" · ")
        }
    }

    /// The allowed key that one read of the terminal gives, if any.
    fn key_for_bytes(self, bytes: &[u8]) -> Option<Key> {
        match bytes {
            [b'f' | b'F', ..] if self.view => Some(Key::ToggleView),
            [b'l' | b'L', ..] if self.legend => Some(Key::ToggleLegend),
            [b'v' | b'V', ..] if self.values => Some(Key::Values),
            [b's' | b'S', ..] if self.skip => Some(Key::Skip),
            [b'o' | b'O', ..] if self.open => Some(Key::Open),
            _ => None,
        }
    }

    /// The allowed key that a line gives, with `--require-enter` or without a terminal.
    fn key_for_line(self, line: &str) -> Option<Key> {
        match line {
            "f" | "F" if self.view => Some(Key::ToggleView),
            "l" | "L" if self.legend => Some(Key::ToggleLegend),
            "v" | "V" if self.values => Some(Key::Values),
            "s" | "S" if self.skip => Some(Key::Skip),
            "o" | "O" if self.open => Some(Key::Open),
            _ => None,
        }
    }
}

/// What a prompt asks.
enum Style<'a, 'p> {
    /// A y/N question, such as a gate or a `confirm`, built for the current view.
    Question(&'a dyn Fn(&Runner<'p>) -> String),
    /// A pause, with the action that Enter takes, such as "continue".
    Pause(&'a str),
}

#[derive(Debug, Clone)]
struct Value {
    data: Data,
    /// Set when the value came from a step whose payload was withheld. It may hold an unknown
    /// secret, so it never reaches a command and never displays.
    withheld: Option<String>,
}

/// Runs a program and reports how it ended. Everything it prints goes to stdout.
pub fn run(program: &Program, options: Options) -> Ending {
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let theme = options.theme;
    if let Err(stop) = preflight(program, &options, tty) {
        print_stop(theme, &stop, None);
        return stop.ending;
    }
    let mut runner = match Runner::new(program, options, tty) {
        Ok(runner) => runner,
        Err(stop) => {
            print_stop(theme, &stop, None);
            return stop.ending;
        }
    };
    runner.start()
}

/// Prints how a run stopped. The message and details must already be redacted.
fn print_stop(theme: Theme, stop: &Stop, journal: Option<&str>) {
    let mark = if stop.ending == Ending::Cancelled {
        "■"
    } else {
        "✗"
    };
    let mut lines = vec![String::new(), format!("{mark} {}", stop.message)];
    for line in &stop.details {
        lines.push(format!("  {line}"));
    }
    if let Some(dir) = journal {
        lines.push(format!("  journal: {dir}"));
    }
    println!("{}", theme.lines(&lines.join("\n")));
}

/// Checks everything that can stop a run before any command executes.
fn preflight(program: &Program, options: &Options, tty: bool) -> Result<(), Stop> {
    if tty && options.yes {
        return Err(Stop::failed("`--yes` is for runs without a terminal")
            .detail("with a terminal, read and approve each gate yourself"));
    }
    if !tty && (!program.unattended || !options.yes) {
        let why = if program.unattended {
            "pass `--yes` to approve every gate"
        } else {
            "this wizard does not declare `unattended allowed`"
        };
        return Err(Stop::failed("refusing to run without a terminal").detail(why));
    }
    Ok(())
}

struct Runner<'p> {
    p: &'p Program,
    opts: Options,
    tty: bool,
    values: HashMap<Sym, Value>,
    redactor: Redactor,
    journal: Journal,
    signals: Signals,
    exes: HashMap<String, PathBuf>,
    provided: HashMap<String, String>,
    seq: u32,
    scratch_root: PathBuf,
    /// The stage to start at (1-based), resolved once from `--from`.
    from: usize,
    /// For each enclosing block, innermost last: the steps after the one running now. A
    /// `confirm` shows all of them, so the human sees everything left in the stage.
    rests: Vec<&'p [Step]>,
    /// The terminal view: with a live view, steps still to run stay below the output.
    screen: RefCell<Screen>,
    /// The footer's text while a stage runs: the stage, then the step running now.
    footer: Option<(String, String)>,
    /// How much of each path and value the terminal shows. The toggle key switches it.
    view: Cell<View>,
    /// Values that name a directory, as path prefixes for the compact view: each name's
    /// current value, already redacted, in the order the names were first set.
    named: Vec<(Sym, String)>,
    /// Whether the compact view shows its legend: `<name> = <path>` lines. `l` switches it.
    legend: Cell<bool>,
    /// While a preview draws, the prefix labels it shows, for the legend under it.
    legend_used: RefCell<Option<Vec<String>>>,
    /// Labels whose path was just set or changed, shown once after the step that set them.
    legend_pending: Vec<String>,
    /// The secret variables a `$` line has already explained, each named once per run.
    announced_vars: HashSet<String>,
    /// The stage running now, or about to run; 0 before stage 1.
    stage_now: usize,
    /// Whether the current stage's steps have started, past its gate or pause.
    in_stage: bool,
    /// Whether the operator has set or cleared any value by hand.
    hand_set: bool,
    /// The names the current stage has set or cleared so far, for the values view.
    touched: std::collections::HashSet<Sym>,
    /// Every stage that will not run, with why: from the command line, the operator, and
    /// their dependents.
    skipped: BTreeMap<usize, Why>,
    /// Each script of the current stage, read once when the stage first shows a prompt: its
    /// bytes and their hash. Previews show these bytes, and a script whose file no longer
    /// matches its pin does not run.
    pins: HashMap<PathBuf, Pin>,
    /// Set while a preview draws, so only previews show scripts.
    previewing: Cell<bool>,
    /// The scripts `o` can open at the prompt showing now.
    prompt_scripts: RefCell<Vec<PathBuf>>,
    /// Values given for skipped stages on the command line or in the environment, stored
    /// once the skips are settled, with who gave them.
    given: Vec<(Sym, Data, &'static str)>,
    /// The command line's skips, when they leave values unset that a later stage reads: the
    /// operator settles them after the preflight.
    start_decided: Option<BTreeMap<usize, Why>>,
    /// `$PWD`, when it names the cwd by another path, such as through a symlink. Paths the
    /// operator types often start with it, so it shortens to `<pwd>` too.
    pwd_alias: Option<String>,
}

impl<'p> Runner<'p> {
    fn new(p: &'p Program, opts: Options, tty: bool) -> Result<Self, Stop> {
        let mut provided = HashMap::new();
        let known: Vec<(String, bool)> = known_inputs(p);
        for (name, value) in &opts.params {
            match known.iter().find(|(n, _)| n == name) {
                None => return Err(Stop::failed(format!("unknown param `{name}`"))),
                Some((_, true)) => {
                    return Err(Stop::failed(format!(
                        "secret `{name}` cannot be passed on the command line"
                    ))
                    .detail(format!(
                        "set the environment variable {} instead",
                        secret_env(name)
                    )));
                }
                Some((_, false)) => {
                    provided.insert(name.clone(), value.clone());
                }
            }
        }
        for (name, secret) in &known {
            if *secret && let Ok(value) = std::env::var(secret_env(name)) {
                provided.insert(name.clone(), value);
            }
        }
        if !tty {
            for (name, secret) in &known {
                let is_required_param = p.params.iter().any(|param| {
                    p.sym(param.sym).name == *name && (param.required || param.secret)
                });
                let is_ask = !p.params.iter().any(|param| p.sym(param.sym).name == *name);
                if (is_required_param || is_ask) && !provided.contains_key(name) {
                    let how = if *secret {
                        format!("set {}", secret_env(name))
                    } else {
                        format!("pass --param {name}=...")
                    };
                    return Err(Stop::failed(format!(
                        "`{name}` has no value, and there is no terminal to ask"
                    ))
                    .detail(how));
                }
            }
        }

        let mut exes = HashMap::new();
        for (exe, loc) in p.executables() {
            let key = exe.display();
            if exes.contains_key(&key) {
                continue;
            }
            let resolved = match exe {
                ir::Exe::Name(name) => find_on_path(name),
                ir::Exe::Path(path) => is_executable(path).then(|| path.clone()),
            };
            let Some(resolved) = resolved else {
                let (line, _) = loc.file.line_col(loc.span.start);
                return Err(Stop::failed(format!("cannot find executable `{key}`"))
                    .detail(format!("used at {}:{line}", loc.file.name))
                    .detail("nothing ran; install it or fix the path"));
            };
            exes.insert(key, resolved);
        }

        let from = match &opts.from {
            Some(from) => resolve_from(p, from).map_err(|d| Stop::failed(d.message.clone()))?,
            None => 1,
        };

        // Every secret known before stage 1 joins the redactor now, so no output can show it
        // and a too-short value stops the run before any command.
        let redactor = Redactor::new();
        for (name, secret) in &known {
            if *secret && let Some(value) = provided.get(name) {
                redactor.add_value(name, value).map_err(Stop::failed)?;
            }
        }
        // Everything `--from`, `--skip`, `--skip-preflight` and `--set` decide is checked now,
        // before anything runs, even the preflight.
        let start = skip_start(p, &opts, from, &provided, &redactor, tty)?;
        let slug = slug(&p.title);
        let journal = Journal::create(&opts.runs_dir, &slug, redactor.clone())
            .map_err(|e| Stop::failed(format!("cannot create the run directory: {e}")))?;
        let signals = Signals::install()
            .map_err(|e| Stop::failed(format!("cannot install signal handlers: {e}")))?;
        sweep_scratch();
        let scratch_root = std::env::temp_dir().join(format!(
            "korin-{}-{}",
            std::process::id(),
            journal
                .dir()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
        // Created fresh with owner-only permissions; an existing path (or a symlink planted in
        // a shared temp directory) is an error, never reused.
        create_private_dir(&scratch_root).map_err(|e| {
            Stop::failed(format!(
                "cannot create the scratch directory {}: {e}",
                scratch_root.display()
            ))
        })?;
        // The live view erases and redraws lines, so it needs a terminal it can measure. Ghost
        // lines are told apart from real output only by color, so it also needs color.
        let theme = opts.theme;
        let view = opts.view;
        let legend = opts.legend;
        let live = tty && theme.enabled() && terminal_size::terminal_size().is_some();
        let cwd_for_alias = opts.cwd.clone();
        Ok(Self {
            p,
            opts,
            tty,
            values: HashMap::new(),
            redactor,
            journal,
            signals,
            exes,
            provided,
            seq: 0,
            scratch_root,
            from,
            rests: Vec::new(),
            screen: RefCell::new(Screen::new(live, theme)),
            footer: None,
            view: Cell::new(if tty { view } else { View::Full }),
            named: Vec::new(),
            legend: Cell::new(legend),
            legend_used: RefCell::new(None),
            legend_pending: Vec::new(),
            announced_vars: HashSet::new(),
            stage_now: 0,
            in_stage: false,
            hand_set: false,
            touched: std::collections::HashSet::new(),
            pins: HashMap::new(),
            previewing: Cell::new(false),
            prompt_scripts: RefCell::new(Vec::new()),
            skipped: start.skipped,
            given: start.given,
            start_decided: start.decided,
            pwd_alias: pwd_alias(&cwd_for_alias),
        })
    }

    fn start(&mut self) -> Ending {
        let from = self.from;
        self.journal.event(
            "run_start",
            json!({"wizard": self.p.file.name, "title": self.p.title, "from": from}),
        );
        self.journal.report(&format!("# {}\n", self.p.title));
        self.intro();
        let p = self.p;
        let result = self.params().and_then(|()| {
            self.flush_legend();
            // The preflight runs first, even with `--from`.
            if let Some(preflight) = &p.preflight {
                self.run_or_skip(preflight)?;
            }
            self.begin_skips()?;
            for stage in &p.stages {
                self.run_or_skip(stage)?;
            }
            Ok(())
        });
        let _ = std::fs::remove_dir_all(&self.scratch_root);
        if let Some((count, first)) = self.journal.failures() {
            self.say(&format!(
                "\n⚠ the journal is incomplete: {count} write(s) failed, first: {first}"
            ));
        }
        match result {
            Ok(()) => {
                self.journal
                    .event("run_end", json!({"ending": "completed"}));
                self.journal.report("\n**Completed.**");
                self.say(&format!("\n✓ {} completed", self.p.title));
                self.say(&format!("  journal: {}", self.journal_shown()));
                Ending::Completed
            }
            Err(stop) if stop.ending == Ending::Ended => {
                self.journal.event(
                    "run_end",
                    json!({"ending": "ended", "message": stop.message}),
                );
                self.journal
                    .report(&format!("\n**Ended:** {}", stop.message));
                self.screen.borrow_mut().clear_all();
                self.say(&format!(
                    "\n✓ {} ended: {}",
                    self.p.title,
                    self.redactor.redact(&stop.message)
                ));
                self.say(&format!("  journal: {}", self.journal_shown()));
                Ending::Ended
            }
            Err(stop) => {
                let ending = match stop.ending {
                    Ending::Cancelled => "cancelled",
                    _ => "failed",
                };
                self.journal.event(
                    "stop",
                    json!({"ending": ending, "message": stop.message, "details": stop.details}),
                );
                self.journal
                    .report(&format!("\n**Stopped:** {}", stop.message));
                for line in &stop.details {
                    self.journal.report(&format!("    {line}"));
                }
                let redacted = Stop {
                    poll_deadline: false,
                    skip_rest: false,
                    ending: stop.ending,
                    message: self.redactor.redact(&stop.message),
                    details: stop
                        .details
                        .iter()
                        .map(|d| self.redactor.redact(d))
                        .collect(),
                };
                self.screen.borrow_mut().clear_all();
                print_stop(self.opts.theme, &redacted, Some(&self.journal_shown()));
                stop.ending
            }
        }
    }

    /// Prints a line to the terminal: redacted first, then colored.
    ///
    /// A line may mark the values korin put into a command, so the theme can color them.
    /// Redaction runs on the marked line, and the marks must not split a secret: the line is
    /// colored only if removing the marks gives exactly the plain line, redacted. Otherwise it
    /// prints that plain line without value colors.
    fn say(&self, line: &str) {
        let line = self.redact_marked(line);
        self.note_prefixes(&line);
        let shown = self.opts.theme.lines(&line);
        self.screen.borrow_mut().print(&format!("{shown}\n"));
    }

    /// Prints a block that holds commands, laid out to read well (`broken`), or on one line
    /// each (`one`) if the broken form would redact differently. Redaction must never depend
    /// on where a command breaks.
    fn say_block(&self, one: &str, broken: &str) {
        let one = self.redact_marked(one);
        let broken = self.redact_marked(broken);
        let chosen =
            if render::strip_marks(&render::join_breaks(&broken)) == render::strip_marks(&one) {
                broken
            } else {
                one
            };
        // Cuts are lossy, so they come after the check above, on the form it chose.
        let chosen = match self.view.get() {
            View::Compact => crate::view::cut_lines(&chosen, self.width()),
            View::Full => chosen,
        };
        self.note_prefixes(&chosen);
        let shown = self.opts.theme.lines(&chosen);
        self.screen.borrow_mut().print(&format!("{shown}\n"));
    }

    /// While a preview draws, remembers each prefix label the text shows.
    fn note_prefixes(&self, text: &str) {
        let mut used = self.legend_used.borrow_mut();
        let Some(used) = used.as_mut() else {
            return;
        };
        let mut rest = text;
        while let Some(start) = rest.find(render::PREFIX_OPEN) {
            let after = &rest[start + render::PREFIX_OPEN.len_utf8()..];
            let Some(end) = after.find(render::PREFIX_CLOSE) else {
                break;
            };
            let label = &after[..end];
            if !used.iter().any(|l| l == label) {
                used.push(label.to_string());
            }
            rest = &after[end + render::PREFIX_CLOSE.len_utf8()..];
        }
    }

    /// Prints a preview's steps, then the legend for the prefixes they show.
    fn say_preview(&self, steps: &[&Step]) {
        *self.legend_used.borrow_mut() = Some(Vec::new());
        self.previewing.set(true);
        self.say_steps(steps);
        self.previewing.set(false);
        let used = self.legend_used.borrow_mut().take().unwrap_or_default();
        self.say_legend(&used);
    }

    /// Whether legend lines show: in the compact view, unless `l` turned them off.
    fn legend_on(&self) -> bool {
        self.legend.get() && self.view.get() == View::Compact
    }

    /// The path a prefix label stands for, as the legend shows it. `~` has no entry.
    fn legend_path(&self, label: &str) -> Option<String> {
        let path = match label {
            "<pwd>" => self.opts.cwd.to_string_lossy().into_owned(),
            "<tmp>" => std::env::var("TMPDIR")
                .ok()
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "/tmp".into())
                .trim_end_matches('/')
                .to_string(),
            _ => {
                let name = label.strip_prefix('<')?.strip_suffix('>')?;
                self.named
                    .iter()
                    .find(|(sym, _)| self.p.sym(*sym).name == name)?
                    .1
                    .clone()
            }
        };
        Some(render::escape_controls(&home_relative(Path::new(&path))))
    }

    /// Prints the legend for some prefix labels, if legend lines show.
    fn say_legend(&self, labels: &[String]) {
        if !self.legend_on() {
            return;
        }
        let entries: Vec<(String, String)> = labels
            .iter()
            .filter_map(|label| Some((label.clone(), self.legend_path(label)?)))
            .collect();
        if entries.is_empty() {
            return;
        }
        let line = self.opts.theme.legend(&entries, None);
        self.screen.borrow_mut().print(&format!("{line}\n"));
    }

    /// Prints the legend for names whose path a step just set, once.
    fn flush_legend(&mut self) {
        let pending = std::mem::take(&mut self.legend_pending);
        self.say_legend(&pending);
    }

    /// Prints steps, with their commands laid out to read well.
    fn say_steps(&self, steps: &[&Step]) {
        let mut one = Vec::new();
        let mut broken = Vec::new();
        for step in steps {
            render::step_lines(self.p, step, 1, self, None, &mut one);
            render::step_lines(self.p, step, 1, self, Some(self.width()), &mut broken);
        }
        if !one.is_empty() {
            self.say_block(&one.join("\n"), &broken.join("\n"));
        }
    }

    /// Sets or removes a value, and keeps the compact view's named prefixes in step with it:
    /// a name's prefix always follows its current value, and goes away with it.
    fn set_value(&mut self, sym: Sym, value: Option<Value>) {
        if self.in_stage {
            self.touched.insert(sym);
        }
        let prefix = value.as_ref().and_then(|v| self.named_prefix(sym, v));
        match value {
            Some(value) => {
                self.values.insert(sym, value);
            }
            None => {
                self.values.remove(&sym);
            }
        }
        let label = format!("<{}>", self.p.sym(sym).name);
        match (self.named.iter().position(|(s, _)| *s == sym), prefix) {
            (Some(index), Some(path)) => {
                if self.named[index].1 != path {
                    self.legend_pending.push(label);
                }
                self.named[index].1 = path;
            }
            (Some(index), None) => {
                self.named.remove(index);
            }
            (None, Some(path)) => {
                self.legend_pending.push(label);
                self.named.push((sym, path));
            }
            (None, None) => {}
        }
    }

    /// The redacted path a value gives as a named prefix, if it qualifies: a param, ask or
    /// capture that is not secret or withheld, holding one absolute path of at least three
    /// components.
    fn named_prefix(&self, sym: Sym, value: &Value) -> Option<String> {
        let info = self.p.sym(sym);
        let kind_ok = matches!(info.kind, SymKind::Param | SymKind::Ask | SymKind::Capture);
        if !kind_ok || info.secret || value.withheld.is_some() {
            return None;
        }
        let Data::Str(text) = &value.data else {
            return None;
        };
        let path = text.trim_end_matches('/');
        let components = path.split('/').filter(|c| !c.is_empty()).count();
        let qualifies = path.starts_with('/') && !path.contains('\n') && components >= 3;
        qualifies.then(|| self.redactor.redact(path))
    }

    /// The prefixes the compact view shortens, or `None` in the full view.
    fn prefixes(&self) -> Option<Prefixes> {
        if self.view.get() == View::Full {
            return None;
        }
        let mut prefixes = Prefixes::default();
        for (sym, path) in &self.named {
            prefixes.push(path, &format!("<{}>", self.p.sym(*sym).name), false);
        }
        prefixes.push(&self.opts.cwd.to_string_lossy(), "<pwd>", true);
        if let Some(alias) = &self.pwd_alias {
            prefixes.push(alias, "<pwd>", true);
        }
        let tmp = std::env::var("TMPDIR")
            .ok()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "/tmp".into());
        prefixes.push(&tmp, "<tmp>", true);
        if let Some(home) = std::env::var_os("HOME") {
            prefixes.push(&home.to_string_lossy(), "~", true);
        }
        Some(prefixes)
    }

    /// Text for the terminal, such as a message or a line of output: shortened in the
    /// compact view. The text must already be redacted, and must not go to the journal.
    fn shorten_text(&self, text: &str) -> String {
        match self.prefixes() {
            Some(prefixes) => prefixes.shorten_text(text),
            None => text.to_string(),
        }
    }

    /// The journal's directory as the terminal shows it: relative to the cwd in the compact
    /// view.
    fn journal_shown(&self) -> String {
        let dir = self.journal.dir();
        match (self.view.get(), dir.strip_prefix(&self.opts.cwd)) {
            (View::Compact, Ok(rest)) => format!("./{}", rest.display()),
            _ => dir.display().to_string(),
        }
    }

    /// The width to lay out for: the terminal's in a live view, else a fixed width.
    fn width(&self) -> usize {
        if self.screen.borrow().live() {
            screen::width()
        } else {
            render::DEFAULT_WIDTH
        }
    }

    /// Redacts a line that may mark values, keeping the marks only if they split no secret.
    fn redact_marked(&self, line: &str) -> String {
        let redacted_plain = self.redactor.redact(&render::strip_marks(line));
        let redacted_marked = self.redactor.redact(line);
        if render::strip_marks(&redacted_marked) == redacted_plain {
            redacted_marked
        } else {
            redacted_plain
        }
    }

    /// Every step still to run in the stage: this block's remaining steps, then each
    /// enclosing block's, innermost first.
    fn remaining(&self) -> Vec<&'p Step> {
        self.rests
            .iter()
            .rev()
            .flat_map(|block| block.iter())
            .collect()
    }

    /// With a live view, shows the steps still to run as ghost lines under the output.
    fn update_ghost(&self) {
        if !self.screen.borrow().live() {
            return;
        }
        let theme = self.opts.theme;
        let mut lines = Vec::new();
        // A guide shows no preview, so it shows no steps to come either; the footer stays.
        if self.p.mode != Mode::Guide {
            for step in self.remaining() {
                render::step_lines(self.p, step, 1, self, None, &mut lines);
            }
        }
        let compact = self.view.get() == View::Compact;
        let width = self.width();
        let lines = lines
            .iter()
            .map(|line| {
                let line = self.redact_marked(line);
                if compact {
                    theme.ghost(&crate::view::cut_lines(&line, width))
                } else {
                    theme.ghost(&line)
                }
            })
            .collect();
        let footer = self.footer.as_ref().map(|(stage, step)| {
            let mut footer =
                theme.footer(&self.redactor.redact(stage), &self.redactor.redact(step));
            // Under the footer, every name known so far and the path it stands for.
            if self.legend_on() {
                let entries: Vec<(String, String)> = self
                    .named
                    .iter()
                    .filter_map(|(sym, _)| {
                        let label = format!("<{}>", self.p.sym(*sym).name);
                        Some((label.clone(), self.legend_path(&label)?))
                    })
                    .collect();
                if !entries.is_empty() {
                    footer.push('\n');
                    footer.push_str(&theme.legend(&entries, Some(width)));
                }
            }
            footer
        });
        let mut screen = self.screen.borrow_mut();
        screen.set_ghost(lines);
        screen.set_footer(footer);
    }

    /// The intro, shaped like a shell prompt: the wizard, where it runs, then the command.
    fn intro(&self) {
        let stages = self.p.stages.len();
        self.say(&format!(
            "╭─ korin ─ {} ─ {} ─ {stages} stage{}",
            render::escape_controls(&self.p.title),
            render::escape_controls(&home_relative(&self.opts.cwd)),
            if stages == 1 { "" } else { "s" },
        ));
        self.say(&format!(
            "╰─❯ {}",
            render::escape_controls(&self.opts.invocation)
        ));
        self.say_legend(&["<pwd>".to_string(), "<tmp>".to_string()]);
    }

    // ---- inputs ----

    fn params(&mut self) -> Result<(), Stop> {
        for param in &self.p.params {
            let name = self.p.sym(param.sym).name.clone();
            let value = if let Some(value) = self.provided.get(&name) {
                value.clone()
            } else if let Some(default) = &param.default {
                self.text(default)?
            } else if param.required || param.secret {
                self.prompt_value(&name, param.secret)?
            } else {
                continue;
            };
            if param.secret || self.p.sym(param.sym).secret {
                self.redactor
                    .add_value(&name, &value)
                    .map_err(Stop::failed)?;
            }
            self.set_value(
                param.sym,
                Some(Value {
                    data: Data::Str(value),
                    withheld: None,
                }),
            );
        }
        Ok(())
    }

    fn prompt_value(&self, name: &str, secret: bool) -> Result<String, Stop> {
        if !self.tty {
            return Err(Stop::failed(format!(
                "`{name}` has no value, and there is no terminal to ask"
            )));
        }
        if secret {
            self.screen.borrow_mut().hide();
            let prompt = self.opts.theme.ask(&format!("{name} (hidden):"));
            let value = read_hidden_interruptible(&self.signals, &prompt, self.opts.theme)?;
            self.screen.borrow_mut().echoed(&format!("{prompt}✓"));
            Ok(value.to_string())
        } else {
            self.read_line(&self.opts.theme.ask(&format!("{name}:")))
        }
    }

    /// Reads a line from the terminal. Ctrl-C and `SIGTERM` cancel the run instead of being lost.
    fn read_line(&self, prompt: &str) -> Result<String, Stop> {
        self.screen.borrow_mut().hide();
        let answer = read_line_interruptible(&self.signals, prompt)?;
        self.screen
            .borrow_mut()
            .echoed(&format!("{prompt}{answer}"));
        Ok(answer)
    }

    /// Waits for the operator in a guide: Enter or Space continues, and Ctrl-C cancels. With
    /// `toggle`, `f`, `l` and `v` work too, and wait again. The prompt is erased once
    /// answered, where the view allows.
    fn pause(&mut self, action: &str, toggle: bool) -> Result<(), Stop> {
        self.screen.borrow_mut().mark();
        let keys = Keys {
            view: toggle,
            legend: toggle,
            values: toggle,
            skip: false,
            open: false,
        };
        self.ask(&Style::Pause(action), keys, &|_| {})?;
        self.screen.borrow_mut().erase_to_mark();
        Ok(())
    }

    /// [`Runner::prompt`], with the keys that need to change the run handled here: `v` opens
    /// the values view, then the preview is drawn again and the prompt asks again.
    fn ask(
        &mut self,
        style: &Style<'_, 'p>,
        keys: Keys,
        redraw: &dyn Fn(&Self),
    ) -> Result<Key, Stop> {
        let keys = Keys {
            open: !self.prompt_scripts.borrow().is_empty(),
            ..keys
        };
        loop {
            match self.prompt(style, keys, redraw)? {
                Key::Open => {
                    self.open_script()?;
                    {
                        let mut screen = self.screen.borrow_mut();
                        screen.erase_to_mark();
                        screen.mark();
                    }
                    redraw(self);
                }
                Key::Values => {
                    self.values_view()?;
                    {
                        let mut screen = self.screen.borrow_mut();
                        screen.erase_to_mark();
                        screen.mark();
                    }
                    redraw(self);
                }
                key => return Ok(key),
            }
        }
    }

    // ---- scripts ----

    /// The file of a `script` command, as resolved before stage 1.
    fn script_file(&self, command: &Command) -> Option<PathBuf> {
        if !matches!(
            command.kind,
            CommandKind::ScriptRead | CommandKind::ScriptMutate
        ) {
            return None;
        }
        self.exes.get(&command.exe.display()).cloned()
    }

    /// The script files the steps run, inside blocks and templates too, each once.
    fn scripts_in<'s>(&self, steps: impl IntoIterator<Item = &'s Step>) -> Vec<PathBuf> {
        fn walk<'s>(
            runner: &Runner<'_>,
            steps: impl IntoIterator<Item = &'s Step>,
            out: &mut Vec<PathBuf>,
        ) {
            for step in steps {
                match &step.kind {
                    StepKind::Command(c) => {
                        if let Some(file) = runner.script_file(c)
                            && !out.contains(&file)
                        {
                            out.push(file);
                        }
                    }
                    StepKind::When { body, .. }
                    | StepKind::Each { body, .. }
                    | StepKind::Poll { body, .. }
                    | StepKind::Call { body, .. } => walk(runner, body, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(self, steps, &mut out);
        out
    }

    /// Reads each script of a stage once, before its first prompt. The preview and the
    /// check before each run use these bytes, so a later change to the file cannot slip
    /// past the gate.
    fn pin_scripts(&mut self, stage: &ir::Stage) {
        self.pins.clear();
        for file in self.scripts_in(stage.steps.iter()) {
            if let Ok(bytes) = std::fs::read(&file) {
                let hash = Sha256::digest(&bytes).to_vec();
                self.pins.insert(file, Pin { bytes, hash });
            }
        }
    }

    /// Whether a script's file still holds the bytes pinned for the stage.
    fn pin_holds(&self, file: &Path) -> bool {
        match (self.pins.get(file), std::fs::read(file)) {
            (Some(pin), Ok(bytes)) => Sha256::digest(&bytes).to_vec() == pin.hash,
            _ => false,
        }
    }

    /// Opens one of the prompt's scripts in the pager: a redacted copy of its pinned bytes,
    /// never the file itself. With no pager, prints it instead.
    fn open_script(&mut self) -> Result<(), Stop> {
        let scripts = self.prompt_scripts.borrow().clone();
        let file = if scripts.len() == 1 {
            scripts[0].clone()
        } else {
            self.say("");
            for (index, file) in scripts.iter().enumerate() {
                self.say(&format!(
                    "  {:>4}  {}",
                    index + 1,
                    self.shorten_text(&render::escape_controls(&file.display().to_string()))
                ));
            }
            let answer =
                self.read_line(&self.opts.theme.ask("script to open, Enter to go back:"))?;
            match answer
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|n| scripts.get(n.wrapping_sub(1)))
            {
                Some(file) => file.clone(),
                None => return Ok(()),
            }
        };
        let Some(pin) = self.pins.get(&file) else {
            self.say("  the script could not be read");
            return Ok(());
        };
        let Ok(text) = std::str::from_utf8(&pin.bytes) else {
            self.say("  the script is not text, so it cannot be shown");
            return Ok(());
        };
        let shown = self.redactor.redact(text);
        let name = file
            .file_name()
            .map_or_else(|| "script".into(), |n| n.to_string_lossy().into_owned());
        let copy = self.scratch_root.join(format!("view-{name}"));
        let pager = self.pager_argv();
        let opened = std::fs::write(&copy, shown.as_bytes()).is_ok()
            && pager.as_ref().is_some_and(|(exe, argv)| {
                let mut args: Vec<String> = argv[1..].to_vec();
                args.push(copy.display().to_string());
                self.screen.borrow_mut().hide();
                let spec = Spec {
                    exe,
                    arg0: &argv[0],
                    args: &args,
                    env: &[],
                    cwd: &self.opts.cwd,
                    timeout: None,
                    grace: self.opts.grace,
                };
                let mut resume = || Suspended::Resume;
                process::handoff(&spec, &self.signals, &mut resume).is_ok()
            });
        let _ = std::fs::remove_file(&copy);
        if !opened {
            self.say("");
            for line in shown.lines() {
                self.say(&format!("  │ {}", render::escape_controls(line)));
            }
        }
        Ok(())
    }

    /// The pager to run: the config key `pager`, else `$PAGER`, else `less`, split into
    /// words with no shell, and resolved on `PATH`.
    fn pager_argv(&self) -> Option<(PathBuf, Vec<String>)> {
        let command = self
            .opts
            .pager
            .clone()
            .or_else(|| std::env::var("PAGER").ok().filter(|p| !p.trim().is_empty()))
            .unwrap_or_else(|| "less".into());
        let argv: Vec<String> = command.split_whitespace().map(String::from).collect();
        let first = argv.first()?;
        let exe = if first.contains('/') {
            PathBuf::from(first)
        } else {
            find_on_path(first)?
        };
        Some((exe, argv))
    }

    // ---- skipping ----

    /// Runs a stage, or, if it is skipped, says so. A skipped `skip ends` stage ends the run.
    fn run_or_skip(&mut self, stage: &'p ir::Stage) -> Result<(), Stop> {
        let Some(why) = self.skipped.get(&stage.number).cloned() else {
            return self.stage(stage);
        };
        let id = stage_id(stage);
        let needs: Vec<String> = match &why {
            Why::Reads(syms) => syms.iter().map(|s| self.p.sym(*s).name.clone()).collect(),
            _ => Vec::new(),
        };
        self.journal.event(
            "stage_skip",
            json!({"stage": id, "by": why.by(), "needs": needs}),
        );
        if why == Why::From {
            // Taken as done in an earlier run; the terminal says nothing, as before.
            return Ok(());
        }
        let description = why.describe(self.p);
        self.say(&format!("\n{}", self.rule(stage)));
        self.say(&format!("  ↷ {description}"));
        self.journal.report(&format!(
            "\n## {} · {}\n\n- {description}",
            heading(stage),
            stage.title
        ));
        if why.rules_apply() && stage.skip == SkipRule::Ends {
            return Err(Stop::ended(format!(
                "{} was skipped, and skipping it ends the run",
                stage.name()
            )));
        }
        Ok(())
    }

    /// The header line that starts a stage or the preflight.
    fn rule(&self, stage: &ir::Stage) -> String {
        if stage.is_preflight() {
            render::preflight_rule(&stage.title, self.width())
        } else {
            render::stage_rule(
                stage.number,
                self.p.stages.len(),
                &stage.title,
                self.width(),
            )
        }
    }

    /// After the preflight: settles the skips the command line asked for, then stores the
    /// values given for skipped stages.
    fn begin_skips(&mut self) -> Result<(), Stop> {
        if let Some(decided) = self.start_decided.take() {
            let what = "the stages before the run".to_string();
            if !self.skip_dialog(&decided, &[], None, &what, true)? {
                return Err(Stop::cancelled(
                    "the run did not start: skipped stages leave values unset",
                ));
            }
        }
        for (sym, data, by) in std::mem::take(&mut self.given) {
            let skipped = self
                .p
                .sym(sym)
                .stage
                .is_some_and(|s| self.skipped.contains_key(&s));
            if skipped && !self.values.contains_key(&sym) {
                self.set_by(sym, data, by);
            }
        }
        self.store_skipped_asks(&[]);
        Ok(())
    }

    /// The operator pressed `s` at a stage's gate or pause. Returns whether the stage is
    /// skipped; a skipped `skip ends` stage ends the run.
    fn operator_skip(&mut self, stage: &'p ir::Stage) -> Result<bool, Stop> {
        let id = stage_id(stage);
        if stage.skip == SkipRule::Ends {
            let question = format!("Skipping {} ends the run. End now?", stage.name());
            let prompt = self.opts.theme.question(&question, "y/N");
            if self.read_choice(&prompt, &['y'])? != Some('y') {
                return Ok(false);
            }
            self.journal.event(
                "stage_skip",
                json!({"stage": id, "by": "operator", "needs": []}),
            );
            self.journal.report("- skipped by the operator");
            return Err(Stop::ended(format!(
                "{} was skipped, and skipping it ends the run",
                stage.name()
            )));
        }
        let mut decided = self.skipped.clone();
        decided.insert(stage.number, Why::Operator);
        let what = format!("{} \"{}\"", stage.name(), stage.title);
        if !self.skip_dialog(&decided, &[], Some(stage.number), &what, false)? {
            return Ok(false);
        }
        self.screen.borrow_mut().erase_to_mark();
        self.say("  ↷ skipped");
        self.journal.event(
            "stage_skip",
            json!({"stage": id, "by": "operator", "needs": []}),
        );
        self.journal.report("- skipped by the operator");
        Ok(true)
    }

    /// Shows what a skip takes with it, and lets the operator skip it all, set the missing
    /// values by hand, or go back. Nothing changes until the skip is committed: values set
    /// by hand, and answers given on the command line for skipped asks, wait in a pending
    /// overlay that going back throws away. Returns whether the skip was committed.
    fn skip_dialog(
        &mut self,
        decided: &BTreeMap<usize, Why>,
        partial: &[Sym],
        after: Option<usize>,
        what: &str,
        start: bool,
    ) -> Result<bool, Stop> {
        let p = self.p;
        let mut pending: Vec<(Sym, Data)> = Vec::new();
        let mut first = true;
        let back = if start { "Esc stop" } else { "Esc back" };
        loop {
            let closure = {
                let have = |s: Sym| {
                    pending.iter().any(|(q, _)| *q == s)
                        || self.given.iter().any(|(g, _, _)| *g == s)
                        || self.ask_answered(s)
                };
                skip::closure(p, decided, partial, after, &have)
            };
            let dependents: Vec<(usize, Why)> = closure
                .dependents()
                .map(|(n, why)| (n, why.clone()))
                .collect();
            if dependents.is_empty() {
                if first && pending.is_empty() {
                    self.commit_skip(closure, pending, partial);
                    return Ok(true);
                }
                self.say("");
                self.say(&format!("  Skipping {what} now skips nothing else."));
                let prompt = self
                    .opts
                    .theme
                    .question("Skip?", &format!("s skip · {back}"));
                if self.read_choice(&prompt, &['s'])? == Some('s') {
                    self.commit_skip(closure, pending, partial);
                    return Ok(true);
                }
                return Ok(false);
            }
            self.say("");
            self.say(&format!("  Skipping {what} also skips:"));
            let width = dependents
                .iter()
                .map(|(n, _)| stage_label(p, *n).chars().count())
                .max()
                .unwrap_or(0);
            for (n, why) in &dependents {
                let reason = match why {
                    Why::Reads(syms) => {
                        let names: Vec<String> = syms
                            .iter()
                            .map(|s| {
                                let info = p.sym(*s);
                                match &info.producer {
                                    Some(step) => format!("`{}` ({step})", info.name),
                                    None => format!("`{}`", info.name),
                                }
                            })
                            .collect();
                        format!("reads {}", names.join(", "))
                    }
                    Why::Also(from) => format!("skip also, in {}", p.stage(*from).name()),
                    other => other.describe(p),
                };
                self.say(&format!("    {:<width$}  {reason}", stage_label(p, *n)));
            }
            for n in &closure.blocked {
                self.say(&format!(
                    "  {} is `skip never`: set the values it reads by hand to skip",
                    p.stage(*n).name()
                ));
            }
            let can = closure.blocked.is_empty();
            let (hint, choices): (String, &[char]) = if can {
                (
                    format!("a skip all · v set values by hand · {back}"),
                    &['a', 'v'],
                )
            } else {
                (format!("v set values by hand · {back}"), &['v'])
            };
            let prompt = self.opts.theme.question("Skip?", &hint);
            match self.read_choice(&prompt, choices)? {
                Some('a') => {
                    self.commit_skip(closure, pending, partial);
                    return Ok(true);
                }
                Some('v') => self.repair_view(&closure.missing, &mut pending)?,
                _ => return Ok(false),
            }
            first = false;
        }
    }

    /// Lists the values the dependents of a skip need, and lets the operator set them. The
    /// values only wait in `pending` until the skip is committed.
    fn repair_view(&mut self, missing: &[Sym], pending: &mut Vec<(Sym, Data)>) -> Result<(), Stop> {
        loop {
            self.say("");
            self.say("  values to set by hand");
            let width = missing
                .iter()
                .map(|s| self.p.sym(*s).name.chars().count())
                .max()
                .unwrap_or(0);
            for (index, sym) in missing.iter().enumerate() {
                let info = self.p.sym(*sym);
                let shown = pending
                    .iter()
                    .find(|(q, _)| q == sym)
                    .map_or_else(|| "unset".to_string(), |(_, d)| self.show_data(info, d));
                self.say(&format!(
                    "  {:>4}  {:<width$}  {}  {}",
                    index + 1,
                    info.name,
                    self.shorten_text(&shown),
                    self.value_origin(*sym),
                ));
            }
            let answer =
                self.read_line(&self.opts.theme.ask("number to set, Enter to go back:"))?;
            let answer = answer.trim();
            if answer.is_empty() {
                return Ok(());
            }
            let Some(&sym) = answer
                .parse::<usize>()
                .ok()
                .and_then(|n| missing.get(n.wrapping_sub(1)))
            else {
                self.say(&format!("  no value numbered {answer}"));
                continue;
            };
            let info = self.p.sym(sym).clone();
            let data = self.read_typed(&info)?;
            if info.secret
                && let Data::Str(text) = &data
                && let Err(e) = self.redactor.add_value(&info.name, text)
            {
                self.say(&format!("  not set: {e}"));
                continue;
            }
            pending.retain(|(q, _)| *q != sym);
            pending.push((sym, data));
        }
    }

    /// Commits a skip: the values set by hand, the stages it skips, and the answers given on
    /// the command line for the asks it skips.
    fn commit_skip(&mut self, closure: skip::Closure, pending: Vec<(Sym, Data)>, partial: &[Sym]) {
        for (sym, data) in pending {
            self.set_by(sym, data, "operator");
        }
        for (n, why) in closure.stages {
            self.skipped.entry(n).or_insert(why);
        }
        self.store_skipped_asks(partial);
    }

    /// Whether an `ask` already has its answer from `--param` or `KORIN_SECRET_<NAME>`.
    fn ask_answered(&self, sym: Sym) -> bool {
        let info = self.p.sym(sym);
        info.kind == SymKind::Ask && self.provided.contains_key(&info.name)
    }

    /// Gives each skipped `ask` the answer from the command line, if it has one: the step
    /// will not run to take it.
    fn store_skipped_asks(&mut self, partial: &[Sym]) {
        for sym in 0..self.p.syms.len() {
            let info = self.p.sym(sym);
            let skipped =
                info.stage.is_some_and(|s| self.skipped.contains_key(&s)) || partial.contains(&sym);
            if info.kind != SymKind::Ask || !skipped || self.values.contains_key(&sym) {
                continue;
            }
            if let Some(answer) = self.provided.get(&info.name).cloned() {
                self.set_by(sym, Data::Str(answer), "--param");
            }
        }
    }

    /// Sets a value that did not come from its step, and records who gave it.
    fn set_by(&mut self, sym: Sym, data: Data, by: &str) {
        let name = self.p.sym(sym).name.clone();
        self.journal.event(
            "value_set",
            json!({"name": name, "value": data.to_json(), "by": by}),
        );
        let who = if by == "operator" { "by hand" } else { by };
        self.journal.report(&format!("- `{name}` set {who}"));
        self.set_value(
            sym,
            Some(Value {
                data,
                withheld: None,
            }),
        );
        self.hand_set = true;
    }

    // ---- the values view ----

    /// The values the values view lists: params, asks, captures and expects from stages
    /// before the current one, and, once the current stage runs, the ones it made so far.
    fn value_rows(&self) -> Vec<Sym> {
        (0..self.p.syms.len())
            .filter(|&sym| {
                let info = self.p.sym(sym);
                let listed = matches!(
                    info.kind,
                    SymKind::Param | SymKind::Ask | SymKind::Capture | SymKind::Expect
                );
                let earlier = info.stage.is_none_or(|s| s < self.stage_now);
                let made_here = info.stage == Some(self.stage_now)
                    && self.in_stage
                    && self.touched.contains(&sym);
                listed && (earlier || made_here)
            })
            .collect()
    }

    /// One value as the values view shows it. A secret or withheld value shows only its
    /// placeholder: a withheld value may hold a secret the redactor does not know.
    fn value_text(&self, sym: Sym) -> String {
        let info = self.p.sym(sym);
        let Some(value) = self.values.get(&sym) else {
            return if info.optional { "absent" } else { "unset" }.to_string();
        };
        if info.secret {
            return format!("«secret:{}»", info.name);
        }
        if value.withheld.is_some() {
            return format!("«withheld:{}»", info.name);
        }
        self.show_data(info, &value.data)
    }

    /// A value's data as the values view shows it: a secret as its placeholder only.
    fn show_data(&self, info: &SymInfo, data: &Data) -> String {
        if info.secret {
            return format!("«secret:{}»", info.name);
        }
        let text = match data {
            Data::Str(s) => self.redactor.redact(s),
            Data::List(items) => {
                let items: Vec<String> = items.iter().map(|i| self.redactor.redact(i)).collect();
                serde_json::to_string(&items).unwrap_or_default()
            }
            Data::Bool(b) => if *b { "yes" } else { "no" }.to_string(),
        };
        render::escape_controls(&text)
    }

    /// Where a value comes from, as the values view describes it.
    fn value_origin(&self, sym: Sym) -> String {
        let info = self.p.sym(sym);
        let kind = match info.kind {
            SymKind::Param => "param",
            SymKind::Ask => "ask",
            SymKind::Capture => "capture",
            SymKind::Expect => "expect",
            SymKind::EachVar | SymKind::KorinOut => "",
        };
        let mut parts = vec![match &info.producer {
            Some(step) if info.kind != SymKind::Param => format!("{kind} {step}"),
            _ => kind.to_string(),
        }];
        if info.secret {
            parts.push("secret".into());
        }
        if info.optional {
            parts.push("optional".into());
        }
        if let Some(stage) = info.stage
            && self.skipped.contains_key(&stage)
        {
            parts.push(format!("{} skipped", self.p.stage(stage).name()));
        }
        parts.join(", ")
    }

    /// Lists every value known so far, and lets the operator change one by its number.
    fn values_view(&mut self) -> Result<(), Stop> {
        loop {
            let rows = self.value_rows();
            self.say("");
            if rows.is_empty() {
                self.say("  values: none yet");
                return Ok(());
            }
            self.say("  values");
            let names: Vec<String> = rows.iter().map(|s| self.p.sym(*s).name.clone()).collect();
            let width = names.iter().map(|n| n.chars().count()).max().unwrap_or(0);
            let shown: Vec<String> = rows
                .iter()
                .map(|s| self.shorten_text(&self.value_text(*s)))
                .collect();
            let value_width = shown
                .iter()
                .map(|v| render::strip_marks(v).chars().count())
                .max()
                .unwrap_or(0)
                .min(40);
            for (index, sym) in rows.iter().enumerate() {
                let pad =
                    value_width.saturating_sub(render::strip_marks(&shown[index]).chars().count());
                self.say(&format!(
                    "  {:>4}  {:<width$}  {}{}  {}",
                    index + 1,
                    names[index],
                    shown[index],
                    " ".repeat(pad),
                    self.value_origin(*sym),
                ));
            }
            let answer =
                self.read_line(&self.opts.theme.ask("number to change, Enter to go back:"))?;
            let answer = answer.trim();
            if answer.is_empty() {
                return Ok(());
            }
            match answer
                .parse::<usize>()
                .ok()
                .and_then(|n| rows.get(n.wrapping_sub(1)))
            {
                Some(&sym) => self.edit_value(sym)?,
                None => self.say(&format!("  no value numbered {answer}")),
            }
        }
    }

    /// Reads one choice: a single key in a terminal, or a line otherwise. Enter and Esc give
    /// `None`, as does any key not in `choices`.
    fn read_choice(&self, prompt: &str, choices: &[char]) -> Result<Option<char>, Stop> {
        if self.opts.require_enter || !std::io::stdin().is_terminal() {
            let line = self.read_line(prompt)?;
            let line = line.trim();
            return Ok(choices
                .iter()
                .copied()
                .find(|c| line.len() == 1 && line.starts_with(*c)));
        }
        self.screen.borrow_mut().hide();
        let (choice, echo) = read_key_interruptible(&self.signals, prompt, |bytes| match bytes {
            [b'\r' | b'\n', ..] | [0x1b] => Some((None, "")),
            [b, ..] => choices
                .iter()
                .find(|c| u32::from(**c) == u32::from(*b))
                .map(|c| (Some(*c), choice_echo(*c))),
            [] => None,
        })?;
        self.screen.borrow_mut().echoed(&format!("{prompt}{echo}"));
        Ok(choice)
    }

    /// Changes one value: replace it, clear an optional capture, or keep it.
    fn edit_value(&mut self, sym: Sym) -> Result<(), Stop> {
        let info = self.p.sym(sym).clone();
        let choices: &[char] = if info.optional { &['r', 'c'] } else { &['r'] };
        let hint = if info.optional {
            "r replace · c clear · Enter keep:"
        } else {
            "r replace · Enter keep:"
        };
        let prompt = self.opts.theme.ask(&format!("{}: {hint}", info.name));
        match self.read_choice(&prompt, choices)? {
            Some('r') => {
                let data = self.read_typed(&info)?;
                if info.secret
                    && let Data::Str(text) = &data
                    && let Err(e) = self.redactor.add_value(&info.name, text)
                {
                    self.say(&format!("  not changed: {e}"));
                    return Ok(());
                }
                self.set_by(sym, data, "operator");
            }
            Some('c') => {
                self.journal.event(
                    "value_set",
                    json!({"name": info.name, "value": null, "by": "operator"}),
                );
                self.journal
                    .report(&format!("- `{}` cleared by hand", info.name));
                self.set_value(sym, None);
                self.hand_set = true;
            }
            _ => {}
        }
        Ok(())
    }

    /// Reads a new value of a name's type, asking again until the input fits.
    fn read_typed(&self, info: &SymInfo) -> Result<Data, Stop> {
        let theme = self.opts.theme;
        loop {
            let result = if info.secret {
                let prompt = theme.ask(&format!("{} (hidden):", info.name));
                self.screen.borrow_mut().hide();
                let text = read_hidden_interruptible(&self.signals, &prompt, theme)?;
                self.screen.borrow_mut().echoed(&format!("{prompt}✓"));
                Ok(Data::Str(text.to_string()))
            } else {
                match info.ty {
                    Ty::Bool => {
                        let prompt = theme.ask(&format!("{} [y/n]:", info.name));
                        match self.read_choice(&prompt, &['y', 'n'])? {
                            Some(c) => Ok(Data::Bool(c == 'y')),
                            None => Err("answer y or n".to_string()),
                        }
                    }
                    Ty::List => self.read_list(&info.name)?,
                    Ty::Json => {
                        let line = self.read_line(&theme.ask(&format!("{} (JSON):", info.name)))?;
                        parse_json(&line).map(Data::Str)
                    }
                    Ty::Scalar => Ok(Data::Str(
                        self.read_line(&theme.ask(&format!("{}:", info.name)))?,
                    )),
                }
            };
            match result {
                Ok(data) => return Ok(data),
                Err(reason) => self.say(&format!("  not a value for `{}`: {reason}", info.name)),
            }
        }
    }

    /// Reads a list: one item per line until an empty line, or one JSON array of strings.
    fn read_list(&self, name: &str) -> Result<Result<Data, String>, Stop> {
        let theme = self.opts.theme;
        let first = self.read_line(&theme.ask(&format!(
            "{name}: one item per line, then an empty line (or a JSON array):"
        )))?;
        if first.trim_start().starts_with('[') {
            return Ok(parse_list(&first).map(Data::List));
        }
        let mut items = Vec::new();
        let mut line = first;
        while !line.is_empty() {
            items.push(line);
            line = self.read_line(&theme.ask("  …"))?;
        }
        Ok(Ok(Data::List(items)))
    }

    /// Shows a prompt and waits for one of the keys it allows. A question (a gate or a
    /// `confirm`) answers `Yes` to `y` and `No` to `n`, Enter or Esc. A pause answers
    /// `Continue` to Enter or Space, and ignores keys it does not allow. `f` switches between
    /// the compact and the full view: korin erases what was drawn since the mark, draws it
    /// again with `redraw`, and asks again. Every other allowed key is returned for the
    /// caller to act on.
    ///
    /// In a terminal, one key answers. With `--require-enter`, or without a terminal, korin
    /// reads a line instead: each key is a line, and a pause continues on an empty line.
    fn prompt(
        &self,
        style: &Style<'_, 'p>,
        keys: Keys,
        redraw: &dyn Fn(&Self),
    ) -> Result<Key, Stop> {
        loop {
            // The full view has no legend, so `l` is not offered there.
            let keys = Keys {
                legend: keys.legend && self.view.get() == View::Compact,
                ..keys
            };
            let hint = keys.hint(style, self.view.get());
            let prompt = match style {
                Style::Question(question) => self.opts.theme.question(&question(self), &hint),
                Style::Pause(action) => self.opts.theme.pause(action, &hint),
            };
            let question = matches!(style, Style::Question(_));
            let key = if self.opts.require_enter || !std::io::stdin().is_terminal() {
                let line = self.read_line(&prompt)?;
                let line = line.trim();
                match (keys.key_for_line(line), question) {
                    (Some(key), _) => Some(key),
                    (None, true) if matches!(line, "y" | "Y" | "yes" | "Yes") => Some(Key::Yes),
                    (None, true) => Some(Key::No),
                    (None, false) if line.is_empty() => Some(Key::Continue),
                    (None, false) => None,
                }
            } else {
                self.screen.borrow_mut().hide();
                let (key, echo) = read_key_interruptible(&self.signals, &prompt, |bytes| {
                    if let Some(key) = keys.key_for_bytes(bytes) {
                        return Some((key, key.echo()));
                    }
                    match (bytes, question) {
                        ([b'y' | b'Y', ..], true) => Some((Key::Yes, "y")),
                        ([b'n' | b'N' | b'\r' | b'\n', ..] | [0x1b], true) => Some((Key::No, "n")),
                        ([b'\r' | b'\n' | b' ', ..], false) => Some((Key::Continue, "")),
                        // Anything else, such as an arrow key's escape sequence, is ignored.
                        _ => None,
                    }
                })?;
                self.screen.borrow_mut().echoed(&format!("{prompt}{echo}"));
                Some(key)
            };
            match key {
                Some(toggle @ (Key::ToggleView | Key::ToggleLegend)) => {
                    if toggle == Key::ToggleView {
                        self.view.set(self.view.get().toggled());
                    } else {
                        self.legend.set(!self.legend.get());
                    }
                    {
                        let mut screen = self.screen.borrow_mut();
                        screen.erase_to_mark();
                        screen.mark();
                    }
                    redraw(self);
                }
                Some(key) => return Ok(key),
                None => {}
            }
        }
    }

    // ---- stages ----

    fn stage(&mut self, stage: &'p ir::Stage) -> Result<(), Stop> {
        let total = self.p.stages.len();
        self.stage_now = stage.number;
        self.in_stage = false;
        self.touched.clear();
        self.pin_scripts(stage);
        *self.prompt_scripts.borrow_mut() = self.scripts_in(stage.steps.iter());
        let rule = if stage.is_preflight() {
            render::preflight_rule(&stage.title, self.width())
        } else {
            render::stage_rule(stage.number, total, &stage.title, self.width())
        };
        self.say(&format!("\n{rule}"));
        let id = stage_id(stage);
        let guide = self.p.mode == Mode::Guide;
        // Everything after the header is the preview; a live view erases it once approved.
        // A guide shows no preview: its stage just runs, after a pause.
        self.screen.borrow_mut().mark();
        if !guide && !stage.steps.is_empty() {
            self.say("");
            self.say_preview(&stage.steps.iter().collect::<Vec<_>>());
        }
        self.journal.event(
            "stage_start",
            json!({"stage": id, "title": stage.title, "mutating": stage.mutating}),
        );
        self.journal.report(&if stage.is_preflight() {
            format!("\n## Preflight · {}\n", stage.title)
        } else {
            format!("\n## Stage {} · {}\n", stage.number, stage.title)
        });
        self.check_withheld_boundaries(&stage.steps)?;
        let gated = !guide && (self.p.gates == GateMode::All || stage.mutating);
        if self.opts.yes {
            if gated {
                self.say("  approved by --yes");
                self.journal.event("gate_auto", json!({"stage": id}));
                self.journal.report("- gate approved by `--yes`");
            } else {
                self.journal
                    .event("pause", json!({"stage": id, "auto": true}));
                self.journal.report("- continued by `--yes`");
            }
        } else {
            // A guide shows no preview, so it draws nothing again either.
            let preview = |r: &Self| {
                if !guide && !stage.steps.is_empty() {
                    r.say("");
                    r.say_preview(&stage.steps.iter().collect::<Vec<_>>());
                }
            };
            let keys = Keys {
                view: true,
                legend: true,
                values: true,
                skip: stage.skip != SkipRule::Never,
                open: false,
            };
            let question = |_: &Self| format!("Run {}?", stage.name());
            let style = if gated {
                Style::Question(&question)
            } else {
                Style::Pause("continue")
            };
            loop {
                match self.ask(&style, keys, &preview)? {
                    Key::Skip => {
                        if self.operator_skip(stage)? {
                            return Ok(());
                        }
                        {
                            let mut screen = self.screen.borrow_mut();
                            screen.erase_to_mark();
                            screen.mark();
                        }
                        preview(self);
                    }
                    Key::Yes => {
                        self.journal
                            .event("gate", json!({"stage": id, "approved": true}));
                        self.journal.report("- gate approved");
                        break;
                    }
                    Key::Continue => {
                        self.journal.event("pause", json!({"stage": id}));
                        self.journal.report("- continued");
                        break;
                    }
                    _ => {
                        self.journal
                            .event("gate", json!({"stage": id, "approved": false}));
                        return Err(Stop::failed(format!(
                            "{} was not approved; nothing in it ran",
                            stage.name()
                        )));
                    }
                }
            }
        }
        self.screen.borrow_mut().erase_to_mark();
        self.prompt_scripts.borrow_mut().clear();
        self.in_stage = true;
        let prefix = if stage.is_preflight() {
            "p".to_string()
        } else {
            format!("s{}", stage.number)
        };
        let before = self.seq;
        self.footer = Some((
            if stage.is_preflight() {
                format!("Preflight · {}", render::escape_controls(&stage.title))
            } else {
                format!(
                    "Stage {}/{total} · {}",
                    stage.number,
                    render::escape_controls(&stage.title)
                )
            },
            String::new(),
        ));
        let result = self.steps(&stage.steps, &prefix, "", None);
        self.footer = None;
        self.screen.borrow_mut().clear_all();
        if let Err(stop) = &result
            && stop.skip_rest
        {
            self.say(&format!("  ↷ the rest of {} was skipped", stage.name()));
            return Ok(());
        }
        result?;
        // The stage-end line.
        let commands = self.seq - before;
        let tail = match commands {
            0 => String::new(),
            1 => " · 1 command".into(),
            n => format!(" · {n} commands"),
        };
        let done = if stage.is_preflight() {
            "preflight".to_string()
        } else {
            format!("stage {}", stage.number)
        };
        self.say(&format!("  ✓ {done} done{tail}"));
        Ok(())
    }

    /// Stops before a gate if any command in the stage would receive a value that is already
    /// withheld.
    fn check_withheld_boundaries(&self, steps: &[Step]) -> Result<(), Stop> {
        for step in steps {
            match &step.kind {
                StepKind::Command(c) => {
                    for t in c.argv.iter().chain(c.env.iter().map(|(_, t)| t)) {
                        for sym in t.syms() {
                            if let Some(reason) =
                                self.values.get(&sym).and_then(|v| v.withheld.as_ref())
                            {
                                return Err(self.withheld_stop(sym, reason));
                            }
                        }
                    }
                }
                StepKind::When { body, .. }
                | StepKind::Each { body, .. }
                | StepKind::Poll { body, .. }
                | StepKind::Call { body, .. } => self.check_withheld_boundaries(body)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn withheld_stop(&self, sym: Sym, reason: &str) -> Stop {
        Stop::failed(format!(
            "`{}` is withheld ({reason}), so it cannot be passed to a command",
            self.p.sym(sym).name
        ))
        .detail("nothing in this stage ran")
    }

    fn steps(
        &mut self,
        steps: &'p [Step],
        prefix: &str,
        suffix: &str,
        deadline: Option<Instant>,
    ) -> Result<(), Stop> {
        for (index, step) in steps.iter().enumerate() {
            let path = format!("{prefix}.{}", index + 1);
            let rest = &steps[index + 1..];
            self.rests.push(rest);
            let result = self.step(step, rest, &path, suffix, deadline);
            self.rests.pop();
            self.flush_legend();
            result?;
        }
        Ok(())
    }

    fn step(
        &mut self,
        step: &'p Step,
        rest: &'p [Step],
        path: &str,
        suffix: &str,
        deadline: Option<Instant>,
    ) -> Result<(), Stop> {
        // A Ctrl-C or SIGTERM that arrived between steps stops the run before the next step.
        if self.signals.take_interrupt() {
            return Err(Stop::cancelled("interrupted with Ctrl-C"));
        }
        if self.signals.take_terminate() {
            return Err(Stop::cancelled("korin received SIGTERM or SIGHUP"));
        }
        if let Some((_, current)) = &mut self.footer {
            current.clone_from(&step.id);
        }
        // A `confirm` sets the scripts `o` can open; they belong to its prompt only.
        self.prompt_scripts.borrow_mut().clear();
        self.update_ghost();
        match &step.kind {
            StepKind::Command(command) => self.command(command, step, path, suffix, deadline),
            StepKind::Confirm(text) => {
                let text = self.display_interpolated(text);
                // Everything left in the stage, shown in full as at a gate. A live view
                // erases the list once confirmed, and the ghost takes over again.
                let _ = rest;
                let steps = self.remaining();
                self.screen.borrow_mut().clear();
                self.screen.borrow_mut().mark();
                if !steps.is_empty() {
                    self.say("  next:");
                    self.say_preview(&steps);
                }
                let redraw = |r: &Self| {
                    if !steps.is_empty() {
                        r.say("  next:");
                        r.say_preview(&steps);
                    }
                };
                let keys = Keys {
                    view: true,
                    legend: true,
                    values: true,
                    skip: self.p.stage(self.stage_now).skip != SkipRule::Never,
                    open: false,
                };
                let question = |r: &Self| r.shorten_text(&text);
                *self.prompt_scripts.borrow_mut() = self.scripts_in(steps.iter().copied());
                loop {
                    match self.ask(&Style::Question(&question), keys, &redraw)? {
                        Key::Yes => {
                            self.screen.borrow_mut().erase_to_mark();
                            self.update_ghost();
                            self.journal
                                .event("confirm", json!({"step": step.id, "approved": true}));
                            return Ok(());
                        }
                        Key::Skip => {
                            let partial = skip::made_by(steps.iter().copied());
                            let what =
                                format!("the rest of {}", self.p.stage(self.stage_now).name());
                            let decided = self.skipped.clone();
                            let now = Some(self.stage_now);
                            if self.skip_dialog(&decided, &partial, now, &what, false)? {
                                self.screen.borrow_mut().erase_to_mark();
                                self.journal.event(
                                    "stage_skip",
                                    json!({
                                        "stage": stage_id(self.p.stage(self.stage_now)),
                                        "by": "operator",
                                        "from_step": step.id,
                                        "needs": [],
                                    }),
                                );
                                self.journal.report(&format!(
                                    "- the rest skipped by the operator at step {}",
                                    step.id
                                ));
                                return Err(Stop::skip_rest());
                            }
                            {
                                let mut screen = self.screen.borrow_mut();
                                screen.erase_to_mark();
                                screen.mark();
                            }
                            redraw(self);
                        }
                        _ => {
                            self.journal
                                .event("confirm", json!({"step": step.id, "approved": false}));
                            return Err(Stop::failed(format!(
                                "not confirmed at step {}: {text}",
                                step.id
                            )));
                        }
                    }
                }
            }
            StepKind::Ask {
                sym,
                secret,
                prompt,
            } => {
                let name = self.p.sym(*sym).name.clone();
                let value = if let Some(value) = self.provided.get(&name) {
                    value.clone()
                } else {
                    let prompt = self.display_interpolated(prompt);
                    self.say(&format!("  {}", self.shorten_text(&prompt)));
                    self.prompt_value(&name, *secret)?
                };
                if *secret {
                    self.redactor
                        .add_value(&name, &value)
                        .map_err(Stop::failed)?;
                }
                self.journal.event(
                    "ask",
                    json!({"step": step.id, "name": name, "value": value}),
                );
                self.set_value(
                    *sym,
                    Some(Value {
                        data: Data::Str(value),
                        withheld: None,
                    }),
                );
                Ok(())
            }
            StepKind::Assert { cond, message } => {
                let ok = self.eval_bool(cond)?;
                self.journal
                    .event("assert", json!({"step": step.id, "passed": ok}));
                if ok {
                    Ok(())
                } else {
                    Err(Stop::failed(self.display_interpolated(message))
                        .detail(format!("assert failed at step {}", step.id)))
                }
            }
            StepKind::When { cond, body } => {
                if self.eval_bool(cond)? {
                    self.steps(body, path, suffix, deadline)
                } else {
                    Ok(())
                }
            }
            StepKind::Each { var, list, body } => {
                let (items, withheld) = self.eval_list(list)?;
                for (index, item) in items.into_iter().enumerate() {
                    self.set_value(
                        *var,
                        Some(Value {
                            data: Data::Str(item),
                            withheld: withheld.clone(),
                        }),
                    );
                    self.steps(body, &format!("{path}[{index}]"), suffix, deadline)?;
                }
                Ok(())
            }
            StepKind::Poll {
                every,
                limit,
                body,
                until,
                fail,
            } => self.poll(
                step,
                path,
                *every,
                *limit,
                body,
                until,
                fail.as_ref(),
                deadline,
            ),
            StepKind::Say { kind, text } => {
                // One full, redacted text: the journal keeps it, the terminal shows its view.
                let text = self.display_interpolated(text);
                let keyword = match kind {
                    SayKind::Note => "note",
                    SayKind::Warn => "warn",
                    SayKind::Record => "record",
                };
                self.say(&comment(&format!("{keyword}: {text}")));
                if *kind == SayKind::Record {
                    self.journal.report(&format!("- {text}"));
                }
                self.journal
                    .event("say", json!({"step": step.id, "text": text}));
                Ok(())
            }
            StepKind::Call { alias, body, .. } => {
                let prefix = match alias {
                    Some(alias) => format!("{path}/{alias}"),
                    None => path.to_string(),
                };
                self.steps(body, &prefix, suffix, deadline)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn poll(
        &mut self,
        step: &'p Step,
        path: &str,
        every: u64,
        limit: u64,
        body: &'p [Step],
        until: &Expr,
        fail: Option<&Expr>,
        outer: Option<Instant>,
    ) -> Result<(), Stop> {
        let mut deadline = Instant::now() + Duration::from_secs(limit);
        if let Some(outer) = outer {
            deadline = deadline.min(outer);
        }
        let mut attempts = Vec::new();
        for attempt in 1.. {
            let before = self.seq;
            let ran = self.steps(body, path, &format!("@{attempt}"), Some(deadline));
            attempts.extend((before + 1..=self.seq).map(|n| n.to_string()));
            match ran {
                Ok(()) => {}
                Err(stop) if stop.poll_deadline => {
                    return Err(self.poll_timeout(step, limit, until, body, attempt, &attempts));
                }
                Err(stop) => return Err(stop),
            }
            if let Some(fail) = fail
                && self.eval_bool(fail)?
            {
                return Err(Stop::failed(format!(
                    "poll at step {} hit its failure condition",
                    step.id
                ))
                .detail(format!("fail {}", render::expr(self.p, fail, self))));
            }
            if self.eval_bool(until)? {
                return Ok(());
            }
            let wake = Instant::now() + Duration::from_secs(every);
            if wake >= deadline {
                return Err(self.poll_timeout(step, limit, until, body, attempt, &attempts));
            }
            self.say(&format!("  … waiting {every}s (attempt {attempt})"));
            self.sleep_until(wake)?;
        }
        Ok(())
    }

    fn poll_timeout(
        &self,
        step: &Step,
        limit: u64,
        until: &Expr,
        body: &[Step],
        attempt: usize,
        attempts: &[String],
    ) -> Stop {
        let mut stop = Stop::failed(format!("poll at step {} timed out after {limit}s", step.id))
            .detail(format!("until {}", render::expr(self.p, until, self)))
            .detail(format!(
                "attempts: {attempt} (invocations {})",
                attempts.join(", ")
            ));
        for sym in body_captures(body) {
            let shown = self.shown(self.p, sym).unwrap_or_else(|| "absent".into());
            stop = stop.detail(format!("last {} = {shown}", self.p.sym(sym).name));
        }
        stop
    }

    fn sleep_until(&self, wake: Instant) -> Result<(), Stop> {
        while Instant::now() < wake {
            if self.signals.take_interrupt() {
                return Err(Stop::cancelled("interrupted with Ctrl-C"));
            }
            if self.signals.take_terminate() {
                return Err(Stop::cancelled("korin received SIGTERM or SIGHUP"));
            }
            std::thread::sleep(
                Duration::from_millis(50).min(wake.saturating_duration_since(Instant::now())),
            );
        }
        Ok(())
    }

    // ---- commands ----

    #[allow(clippy::too_many_lines)]
    fn command(
        &mut self,
        command: &Command,
        step: &Step,
        path: &str,
        suffix: &str,
        deadline: Option<Instant>,
    ) -> Result<(), Stop> {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(Stop::poll_deadline());
        }
        for expect in &command.expects {
            self.set_value(expect.sym, None);
        }
        for capture in &command.captures {
            self.set_value(capture.sym, None);
        }
        self.seq += 1;
        let invocation = format!("{:04}-{path}{suffix}", self.seq);
        let scratch = self
            .scratch_root
            .join(invocation.replace(['/', '[', ']', '@'], "_"));
        create_private_dir(&scratch)
            .map_err(|e| Stop::failed(format!("cannot create {}: {e}", scratch.display())))?;
        // Removes the scratch directory on every exit path, including a panic.
        let _guard = ScratchGuard(scratch.clone());
        self.command_in(command, step, &invocation, &scratch, deadline)
    }

    #[allow(clippy::too_many_lines)]
    fn command_in(
        &mut self,
        command: &Command,
        step: &Step,
        invocation: &str,
        scratch: &Path,
        deadline: Option<Instant>,
    ) -> Result<(), Stop> {
        let argv = self.args(&command.argv, scratch)?;
        let mut env = Vec::new();
        for (name, value) in &command.env {
            env.push((name.clone(), self.exec_text(value, scratch)?));
        }
        let exe = self
            .exes
            .get(&command.exe.display())
            .cloned()
            .ok_or_else(|| {
                Stop::failed(format!(
                    "executable `{}` was not resolved",
                    command.exe.display()
                ))
            })?;
        // A script runs only if its file still holds the bytes shown at the gate.
        let mut script_sha256 = None;
        if let Some(file) = self.script_file(command) {
            if !self.pins.contains_key(&file)
                && let Ok(bytes) = std::fs::read(&file)
            {
                let hash = Sha256::digest(&bytes).to_vec();
                self.pins.insert(file.clone(), Pin { bytes, hash });
            }
            if !self.pin_holds(&file) {
                return Err(Stop::failed(format!(
                    "{} changed after the gate; nothing in step {} ran",
                    home_relative(&file),
                    step.id
                ))
                .detail("read the new version, and run the wizard again"));
            }
            script_sha256 = self.pins.get(&file).map(|pin| hex(&pin.hash));
        }
        self.say_command(command, scratch)?;
        // The journal, the suspend prompt and any failure detail show the full command.
        let shown = render::strip_marks(&render::argv(self.p, &command.argv, &Full(self)));
        let mut start = json!({"invocation_id": invocation, "step": step.id, "command_kind": command.kind.keyword(), "argv": shown, "exe": exe.display().to_string()});
        if let Some(hash) = script_sha256 {
            start["script_sha256"] = json!(hash);
        }
        self.journal.event("command_start", start);

        // Fingerprints for file paths outside the scratch directory, so stale files never count.
        let mut fingerprints: HashMap<PathBuf, Fingerprint> = HashMap::new();
        let mut watched: Vec<PathBuf> = Vec::new();
        for capture in &command.captures {
            if let Some(file) = &capture.file {
                watched.push(self.cwd_path(&self.exec_text(file, scratch)?));
            }
        }
        for expect in &command.expects {
            for expect_match in &expect.matches {
                if let Match::Missing(file) = expect_match {
                    watched.push(self.cwd_path(&self.exec_text(file, scratch)?));
                }
            }
        }
        for path in &watched {
            if !crate::compile::normalize(path).starts_with(scratch) {
                fingerprints.insert(path.clone(), Fingerprint::of(path));
            }
        }
        let is_fresh = |path: &Path| match fingerprints.get(path) {
            Some(_) if crate::compile::normalize(path).starts_with(scratch) => path.exists(),
            Some(fp) => fp.fresh(path),
            None => path.exists(),
        };

        let mut env_all = env.clone();
        env_all.push(("KORIN_OUT".into(), scratch.display().to_string()));
        let args: Vec<String> = argv[1..].to_vec();

        let mut limited_by_poll = false;
        let (status, stdout, stderr, cancelled, escaped) = if command.kind
            == CommandKind::Interactive
        {
            // A guide has no gate, so it asks before it gives the terminal away. The command
            // is already on screen above the pause.
            if self.p.mode == Mode::Guide && !self.opts.yes {
                self.pause("open", false)?;
            }
            self.screen.borrow_mut().hide();
            self.journal
                .event("handoff_start", json!({"invocation_id": invocation}));
            let grace = self.opts.grace;
            let signals = &self.signals;
            let suspended = self.opts.theme.ask(&format!(
                "{} is suspended: [r]esume or [k]ill?",
                self.redactor.redact(&shown)
            ));
            let mut on_suspend = || {
                println!();
                match read_line_interruptible(signals, &suspended) {
                    Ok(answer) if !answer.trim().starts_with('k') => Suspended::Resume,
                    _ => Suspended::Kill,
                }
            };
            let spec = Spec {
                exe: &exe,
                arg0: &argv[0],
                args: &args,
                env: &env_all,
                cwd: &self.opts.cwd,
                timeout: None,
                grace,
            };
            let handoff = process::handoff(&spec, signals, &mut on_suspend)
                .map_err(|e| Stop::failed(format!("cannot start `{}`: {e}", argv[0])))?;
            self.journal.event(
                "handoff_end",
                json!({"invocation_id": invocation, "status": status_json(handoff.status), "cancelled": handoff.cancelled.map(Cancel::describe)}),
            );
            (
                handoff.status,
                Vec::new(),
                Vec::new(),
                handoff.cancelled,
                false,
            )
        } else {
            let own = command.timeout.map_or(DEFAULT_TIMEOUT, Duration::from_secs);
            let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            let timeout = left.map_or(own, |left| own.min(left));
            limited_by_poll = left.is_some_and(|left| left < own);
            let spec = Spec {
                exe: &exe,
                arg0: &argv[0],
                args: &args,
                env: &env_all,
                cwd: &self.opts.cwd,
                timeout: Some(timeout),
                grace: self.opts.grace,
            };
            let stream = !command.secret_producing;
            let redactor = self.redactor.clone();
            // One buffer per stream: a secret split across two reads is held until whole.
            let mut buffers = [StreamRedactor::default(), StreamRedactor::default()];
            let mut line_start = true;
            // The run log reads like a shell session: output follows its `$` line as it is.
            let gutter = String::new();
            let screen = &self.screen;
            let live = screen.borrow().live();
            let theme = self.opts.theme;
            // Removes korin's own marks, so output cannot pose as a colored value.
            let view_lines = |text: &str| theme.output(text);
            // A live view writes whole lines only, so it can count the rows it must erase.
            let mut partial = String::new();
            let mut on_output = |is_err: bool, chunk: &[u8]| {
                if stream {
                    let text = buffers[usize::from(is_err)].push(&redactor, chunk);
                    if live {
                        partial.push_str(&text);
                        if let Some(end) = partial.rfind('\n') {
                            let whole: String = partial.drain(..=end).collect();
                            screen
                                .borrow_mut()
                                .print(&indent(&view_lines(&whole), &gutter));
                        }
                    } else {
                        // A chunk may end mid-line, so only lines it holds whole are shortened.
                        let shown = if line_start && text.ends_with('\n') {
                            view_lines(&text)
                        } else {
                            text
                        };
                        screen.borrow_mut().print_raw(&indent_from(
                            &shown,
                            &gutter,
                            &mut line_start,
                        ));
                    }
                }
            };
            let finished = process::execute(&spec, &self.signals, &mut on_output)
                .map_err(|e| Stop::failed(format!("cannot start `{}`: {e}", argv[0])))?;
            if stream {
                if live {
                    for buffer in &mut buffers {
                        partial.push_str(&buffer.finish(&redactor));
                    }
                    if !partial.is_empty() {
                        let shown = view_lines(&partial);
                        self.screen.borrow_mut().print(&indent(&shown, &gutter));
                    }
                } else {
                    let mut screen = self.screen.borrow_mut();
                    for buffer in &mut buffers {
                        screen.print_raw(&indent_from(
                            &buffer.finish(&redactor),
                            &gutter,
                            &mut line_start,
                        ));
                    }
                    if !line_start {
                        screen.print_raw("\n");
                    }
                }
            }
            (
                finished.status,
                finished.stdout,
                finished.stderr,
                finished.cancelled,
                finished.escaped,
            )
        };
        // Output can hold secrets; zero it when it is dropped, on every exit path.
        let stdout = Zeroizing::new(stdout);
        let stderr = Zeroizing::new(stderr);

        if let Some(cancel) = cancelled {
            let published = !command.secret_producing;
            let unresolved: Vec<String> = command
                .captures
                .iter()
                .filter(|c| c.secret)
                .map(|c| self.p.sym(c.sym).name.clone())
                .collect();
            self.write_streams(invocation, &stdout, &stderr, published, &unresolved);
            if escaped {
                self.note(&format!("⚠ step {}: output pipes stayed open after the command was cancelled; a process it started may still be running", step.id));
                self.journal
                    .event("escaped", json!({"invocation_id": invocation}));
            }
            self.journal.event(
                "command_end",
                json!({"invocation_id": invocation, "status": status_json(status), "cancelled": cancel.describe()}),
            );
            let mut stop = Stop::cancelled(format!(
                "step {} was cancelled: {}",
                step.id,
                cancel.describe()
            ));
            if command.kind.is_mutating() {
                stop = stop.detail(
                    "it may be partially applied; re-run the wizard to re-check the current state",
                );
            }
            if cancel == Cancel::Timeout {
                stop.ending = Ending::Failed;
                stop.poll_deadline = limited_by_poll;
            }
            return Err(stop);
        }

        // Expects: first match wins.
        let stdout_text = Zeroizing::new(String::from_utf8_lossy(&stdout).into_owned());
        let stderr_text = Zeroizing::new(String::from_utf8_lossy(&stderr).into_owned());
        let mut matched: Option<Sym> = None;
        for expect in &command.expects {
            self.set_value(
                expect.sym,
                Some(Value {
                    data: Data::Bool(false),
                    withheld: None,
                }),
            );
        }
        for expect in &command.expects {
            let mut all = true;
            for expect_match in &expect.matches {
                let ok = match expect_match {
                    Match::Exit(code) => status == Status::Exited(*code),
                    Match::ExitNonZero => matches!(status, Status::Exited(c) if c != 0),
                    Match::Signal(name) => {
                        matches!(status, Status::Signaled(s) if crate::signals::name(s) == name)
                    }
                    Match::Stdout(re) => {
                        regex::Regex::new(re).is_ok_and(|r| r.is_match(&stdout_text))
                    }
                    Match::Stderr(re) => {
                        regex::Regex::new(re).is_ok_and(|r| r.is_match(&stderr_text))
                    }
                    Match::Missing(file) => {
                        let path = self.cwd_path(&self.exec_text(file, scratch)?);
                        !is_fresh(&path)
                    }
                };
                if !ok {
                    all = false;
                    break;
                }
            }
            if all {
                matched = Some(expect.sym);
                break;
            }
        }
        if let Some(sym) = matched {
            self.set_value(
                sym,
                Some(Value {
                    data: Data::Bool(true),
                    withheld: None,
                }),
            );
        }
        let succeeded = matched.is_some() || status == Status::Exited(0);

        // Captures.
        let mut results: Vec<(&ir::Capture, Extract, Option<CapturedFile>)> = Vec::new();
        if succeeded {
            for capture in &command.captures {
                let applies = match capture.on {
                    Some(on) => matched == Some(on),
                    None => matched.is_none(),
                };
                if !applies {
                    continue;
                }
                let (input, file_bytes) = match &capture.file {
                    None => (Zeroizing::new(stdout.to_vec()), None),
                    Some(file) => {
                        let path = self.cwd_path(&self.exec_text(file, scratch)?);
                        // `check` already holds secret captures to `{korin.out}`; this backs it
                        // up at run time, and also refuses a symlink the command left there.
                        if capture.secret && !private_file(&path, scratch) {
                            results.push((
                                capture,
                                Extract::Fail(format!(
                                    "{} is not a regular file inside the scratch directory",
                                    path.display()
                                )),
                                None,
                            ));
                            continue;
                        }
                        if !is_fresh(&path) {
                            results.push((
                                capture,
                                Extract::Fail(format!(
                                    "{} is missing, or was not written by this command",
                                    path.display()
                                )),
                                None,
                            ));
                            continue;
                        }
                        match std::fs::read(&path) {
                            Ok(bytes) => {
                                let bytes = Zeroizing::new(bytes);
                                (bytes.clone(), Some((path.clone(), bytes)))
                            }
                            Err(e) => {
                                results.push((
                                    capture,
                                    Extract::Fail(format!("cannot read {}: {e}", path.display())),
                                    None,
                                ));
                                continue;
                            }
                        }
                    }
                };
                results.push((
                    capture,
                    extract(&capture.mode, &input, capture.optional),
                    file_bytes,
                ));
            }
        }

        // Secrets resolve first, so publication can redact them.
        let mut secret_error = None;
        for (capture, result, _) in &mut results {
            if capture.secret
                && let Extract::Value(data) = result
            {
                let name = self.p.sym(capture.sym).name.clone();
                // Each list item gets a shell variable of its own in `$` lines.
                let items: Vec<(Option<usize>, String)> = match data {
                    Data::Str(s) => vec![(None, s.clone())],
                    Data::List(items) => items
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(i, s)| (Some(i), s))
                        .collect(),
                    Data::Bool(_) => Vec::new(),
                };
                for (position, item) in items {
                    if let Err(e) = self.redactor.add_value_at(&name, position, &item) {
                        secret_error = Some(e);
                        *result = Extract::Fail("the secret is too short to redact safely".into());
                    }
                }
            }
        }
        let unresolved: Vec<String> = command
            .captures
            .iter()
            .filter(|c| c.secret)
            .filter(|c| {
                !results
                    .iter()
                    .any(|(r, e, _)| r.sym == c.sym && matches!(e, Extract::Value(_)))
            })
            .map(|c| self.p.sym(c.sym).name.clone())
            .collect();
        let published = !command.secret_producing || unresolved.is_empty();
        let withheld_reason = (!published).then(|| {
            format!(
                "step {}, unresolved secret {}",
                step.id,
                unresolved
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });

        // Publish or withhold the payload.
        let copies: Vec<(String, Zeroizing<Vec<u8>>)> = results
            .iter()
            .filter(|(c, e, _)| !c.secret && matches!(e, Extract::Value(_)))
            .filter_map(|(c, _, file)| {
                file.as_ref()
                    .map(|(_, b)| (self.p.sym(c.sym).name.clone(), b.clone()))
            })
            .collect();
        self.write_streams(invocation, &stdout, &stderr, published, &unresolved);
        for (name, bytes) in &copies {
            let rel = format!("{invocation}/{name}");
            if published {
                self.journal.write_out(&rel, bytes);
            } else {
                self.journal
                    .write_out(&rel, placeholder(bytes.len(), &unresolved).as_bytes());
            }
        }
        if command.secret_producing && published {
            let theme = self.opts.theme;
            let out = theme.output(&self.redactor.redact_bytes(&stdout));
            let err = theme.output(&self.redactor.redact_bytes(&stderr));
            let mut screen = self.screen.borrow_mut();
            screen.print(&indent(&out, ""));
            screen.print(&indent(&err, ""));
        } else if !published {
            self.note(&format!(
                "output withheld: {}",
                placeholder(stdout.len() + stderr.len(), &unresolved)
            ));
        }
        if escaped {
            self.note(&format!("⚠ step {}: output pipes stayed open after the command ended; a process it started may still be running", step.id));
            self.journal
                .event("escaped", json!({"invocation_id": invocation}));
        }
        self.journal.event(
            "command_end",
            json!({"invocation_id": invocation, "status": status_json(status), "stdout_bytes": stdout.len(), "stderr_bytes": stderr.len(), "published": published}),
        );
        if let Some(sym) = matched {
            self.journal.event(
                "expect",
                json!({"invocation_id": invocation, "name": self.p.sym(sym).name}),
            );
        }
        // The exit status, when it is worth a look, and the expect that explains it.
        let code = match status {
            Status::Exited(0) => None,
            Status::Exited(c) => Some(format!("exit {c}")),
            Status::Signaled(s) => Some(format!("signal {}", crate::signals::name(s))),
        };
        let expect = matched.map(|sym| format!("expect {}", self.p.sym(sym).name));
        let status_note: Vec<String> = code.into_iter().chain(expect).collect();
        if !status_note.is_empty() {
            let tail = if succeeded { " · continuing" } else { "" };
            self.note(&format!("{}{tail}", status_note.join(" · ")));
        }

        // Store values and log captures.
        let mut failures = Vec::new();
        for (capture, result, file) in &results {
            let name = self.p.sym(capture.sym).name.clone();
            match result {
                Extract::Value(data) => {
                    if published {
                        self.journal.event("capture", json!({"invocation_id": invocation, "name": name, "value": data.to_json()}));
                        let shown = if capture.secret {
                            format!("«secret:{name}»")
                        } else {
                            self.capture_shown(data)
                        };
                        self.note(&format!("{name} = {shown}"));
                    } else {
                        self.journal.event(
                            "capture",
                            json!({"invocation_id": invocation, "name": name, "withheld": true}),
                        );
                    }
                    self.set_value(
                        capture.sym,
                        Some(Value {
                            data: data.clone(),
                            withheld: if capture.secret {
                                None
                            } else {
                                withheld_reason.clone()
                            },
                        }),
                    );
                }
                Extract::Absent => {
                    self.note(&format!("{name} absent"));
                    self.journal.event(
                        "capture",
                        json!({"invocation_id": invocation, "name": name, "absent": true}),
                    );
                }
                Extract::Fail(message) => {
                    failures.push((name, message.clone(), capture.secret, file.clone()));
                }
            }
        }

        if !succeeded {
            let code = match status {
                Status::Exited(c) => format!("exit {c}"),
                Status::Signaled(s) => format!("signal {}", crate::signals::name(s)),
            };
            let mut stop = Stop::failed(format!("step {} failed with {code}", step.id))
                .detail(format!("$ {shown}"));
            if published {
                stop = excerpts(stop, &self.redactor, &stdout, &stderr);
            } else {
                stop = stop.detail(placeholder(stdout.len() + stderr.len(), &unresolved));
            }
            return Err(stop);
        }
        if let Some(e) = secret_error {
            return Err(Stop::failed(e));
        }
        if let Some((name, message, secret, file)) = failures.into_iter().next() {
            // A failure message can quote the input. When the payload is withheld, the input
            // may hold a secret the redactor has never seen, so only the capture name is shown.
            let message = if published && !secret {
                message
            } else {
                "details withheld, because the step's output may hold an unresolved secret".into()
            };
            let mut stop = Stop::failed(format!(
                "capture `{name}` at step {} failed: {message}",
                step.id
            ));
            match (&file, published && !secret) {
                (Some((path, bytes)), true) => {
                    stop = stop.detail(format!("file: {}", path.display()));
                    stop = excerpts(stop, &self.redactor, bytes, &[]);
                }
                (None, true) => stop = excerpts(stop, &self.redactor, &stdout, &[]),
                (Some((path, bytes)), false) => {
                    stop = stop.detail(format!(
                        "file: {} ({} bytes, not shown)",
                        path.display(),
                        bytes.len()
                    ));
                }
                (None, false) => {
                    stop = stop.detail(format!("input: {} bytes (not shown)", stdout.len()));
                }
            }
            return Err(stop);
        }
        Ok(())
    }

    fn write_streams(
        &self,
        invocation: &str,
        stdout: &[u8],
        stderr: &[u8],
        published: bool,
        unresolved: &[String],
    ) {
        if published {
            self.journal
                .write_out(&format!("{invocation}.stdout"), stdout);
            self.journal
                .write_out(&format!("{invocation}.stderr"), stderr);
        } else {
            self.journal.write_out(
                &format!("{invocation}.stdout"),
                placeholder(stdout.len(), unresolved).as_bytes(),
            );
            self.journal.write_out(
                &format!("{invocation}.stderr"),
                placeholder(stderr.len(), unresolved).as_bytes(),
            );
        }
    }

    fn cwd_path(&self, text: &str) -> PathBuf {
        let path = PathBuf::from(text);
        if path.is_absolute() {
            path
        } else {
            self.opts.cwd.join(path)
        }
    }

    // ---- values ----

    fn value(&self, sym: Sym) -> Result<&Value, Stop> {
        self.values.get(&sym).ok_or_else(|| {
            let stop = Stop::failed(format!("`{}` has no value here", self.p.sym(sym).name));
            if self.hand_set {
                stop.detail("a value set or cleared by hand left it missing")
            } else {
                stop.detail("this is a korin bug if `korin check` passed; please report it")
            }
        })
    }

    /// A command's real words for its `$` line, each split into literal text and values, so
    /// the line can name secrets as variables and still color values. Mirrors [`Self::args`].
    fn shell_words(
        &self,
        command: &Command,
        scratch: &Path,
    ) -> Result<(render::ShellEnv, Vec<render::Segments>), Stop> {
        let segments = |t: &Interpolated| -> Result<render::Segments, Stop> {
            let mut out = Vec::new();
            for part in &t.parts {
                match part {
                    IPart::Lit(text) => out.push((text.clone(), false)),
                    IPart::Sym { sym, .. } if *sym == self.p.korin_out => {
                        out.push((scratch.display().to_string(), true));
                    }
                    IPart::Sym { sym, .. } => {
                        out.push((data_text(&self.value(*sym)?.data), true));
                    }
                }
            }
            Ok(out)
        };
        let mut words = Vec::new();
        for t in &command.argv {
            if let [
                IPart::Sym {
                    sym, splat: true, ..
                },
            ] = t.parts.as_slice()
                && let Data::List(items) = &self.value(*sym)?.data
            {
                words.extend(items.iter().map(|i| vec![(i.clone(), true)]));
            } else {
                words.push(segments(t)?);
            }
        }
        let mut env = Vec::new();
        for (name, value) in &command.env {
            env.push((name.clone(), segments(value)?));
        }
        Ok((env, words))
    }

    /// Prints a command's `$` line, and the notes it needs: each secret variable the first
    /// time a line uses it, and a warning when the line cannot be pasted as it is.
    fn say_command(&mut self, command: &Command, scratch: &Path) -> Result<(), Stop> {
        let (env, words) = self.shell_words(command, scratch)?;
        let width = self.screen.borrow().live().then(|| self.width());
        let scratch_text = scratch.display().to_string();
        let line = render::shell_line(&env, &words, &self.redactor, Some(&scratch_text), width);
        // A safety net: the line names secrets as variables, so redaction changes nothing.
        let text = self.redact_marked(&line.lines.join("\n"));
        let shown = self.opts.theme.lines(&text);
        self.screen.borrow_mut().print(&format!("\n{shown}\n"));
        for found in &line.vars {
            let Some(var) = &found.var else { continue };
            if self.announced_vars.insert(var.clone()) {
                self.journal.event(
                    "secret_var",
                    json!({"name": found.name, "var": var, "value": found.label}),
                );
                self.note(&format!("uses ${{{var}}} ({})", found.label));
            }
        }
        if !line.partial.is_empty() {
            self.note(&format!(
                "not pasteable: holds part of {}",
                line.partial.join(", ")
            ));
        }
        Ok(())
    }

    /// A captured value on one line for a note: redacted, escaped, and cut to fit.
    fn capture_shown(&self, data: &Data) -> String {
        let text = match data {
            Data::Str(s) => s.clone(),
            Data::List(items) => format!("[{}]", items.join(", ")),
            Data::Bool(b) => b.to_string(),
        };
        if text.is_empty() {
            return "\"\"".into();
        }
        let text = render::escape_controls(&self.redactor.redact(&text));
        let room = self.width().saturating_sub(20).max(20);
        if text.chars().count() > room {
            let cut: String = text.chars().take(room).collect();
            format!("{cut}…")
        } else {
            text
        }
    }

    /// Prints a `↳` note under a command.
    fn note(&self, text: &str) {
        self.say(&format!("  ↳ {text}"));
    }

    /// The argv for a command. Withheld values never get here.
    fn args(&self, argv: &[Interpolated], scratch: &Path) -> Result<Vec<String>, Stop> {
        let mut out = Vec::new();
        for t in argv {
            if let [
                IPart::Sym {
                    sym, splat: true, ..
                },
            ] = t.parts.as_slice()
            {
                let value = self.value(*sym)?;
                if let Some(reason) = &value.withheld {
                    return Err(self.withheld_stop(*sym, reason));
                }
                match &value.data {
                    Data::List(items) => out.extend(items.iter().cloned()),
                    Data::Str(s) => out.push(s.clone()),
                    Data::Bool(b) => out.push(b.to_string()),
                }
            } else {
                out.push(self.exec_text(t, scratch)?);
            }
        }
        Ok(out)
    }

    fn exec_text(&self, t: &Interpolated, scratch: &Path) -> Result<String, Stop> {
        let mut out = String::new();
        for part in &t.parts {
            match part {
                IPart::Lit(text) => out.push_str(text),
                IPart::Sym { sym, .. } if *sym == self.p.korin_out => {
                    out.push_str(&scratch.display().to_string());
                }
                IPart::Sym { sym, .. } => {
                    let value = self.value(*sym)?;
                    if let Some(reason) = &value.withheld {
                        return Err(self.withheld_stop(*sym, reason));
                    }
                    out.push_str(&data_text(&value.data));
                }
            }
        }
        Ok(out)
    }

    /// Renders a template for use inside korin. Withheld values are allowed; nothing prints.
    fn text(&self, t: &Interpolated) -> Result<String, Stop> {
        let mut out = String::new();
        for part in &t.parts {
            match part {
                IPart::Lit(text) => out.push_str(text),
                IPart::Sym { sym, .. } => out.push_str(&data_text(&self.value(*sym)?.data)),
            }
        }
        Ok(out)
    }

    fn display_interpolated(&self, t: &Interpolated) -> String {
        self.redactor
            .redact(&render::interpolated(self.p, t, self, false))
    }

    fn eval(&self, e: &Expr) -> Result<Data, Stop> {
        Ok(match e {
            Expr::Or(a, b) => Data::Bool(self.eval_bool(a)? || self.eval_bool(b)?),
            Expr::And(a, b) => Data::Bool(self.eval_bool(a)? && self.eval_bool(b)?),
            Expr::Not(inner) => Data::Bool(!self.eval_bool(inner)?),
            Expr::Cmp(op, a, b) => {
                let shown = render::expr(self.p, e, self);
                let (a, b) = (self.eval_str(a)?, self.eval_str(b)?);
                let ordering = || -> Result<std::cmp::Ordering, Stop> {
                    let parse = |s: &str| {
                        s.trim().parse::<f64>().map_err(|_| {
                            Stop::failed(format!(
                                "`{shown}` compares a value that is not a number with `<` or `>`"
                            ))
                        })
                    };
                    let (x, y) = (parse(&a)?, parse(&b)?);
                    x.partial_cmp(&y)
                        .ok_or_else(|| Stop::failed("cannot compare these numbers"))
                };
                Data::Bool(match op {
                    CmpOp::Eq => a == b,
                    CmpOp::Ne => a != b,
                    CmpOp::Lt => ordering()?.is_lt(),
                    CmpOp::Le => ordering()?.is_le(),
                    CmpOp::Gt => ordering()?.is_gt(),
                    CmpOp::Ge => ordering()?.is_ge(),
                })
            }
            Expr::In(a, b) => {
                let needle = self.eval_str(a)?;
                let (items, _) = self.eval_list(b)?;
                Data::Bool(items.contains(&needle))
            }
            Expr::Regex(a, re) => {
                let text = self.eval_str(a)?;
                let re = regex::Regex::new(re)
                    .map_err(|e| Stop::failed(format!("invalid regex: {e}")))?;
                Data::Bool(re.is_match(&text))
            }
            Expr::Exists(sym) => Data::Bool(self.values.contains_key(sym)),
            Expr::Sym(sym) => self.value(*sym)?.data.clone(),
            Expr::Str(t) => Data::Str(self.text(t)?),
            Expr::Num(n) => Data::Str(n.clone()),
            Expr::List(items) => Data::List(
                items
                    .iter()
                    .map(|i| self.eval_str(i))
                    .collect::<Result<_, _>>()?,
            ),
        })
    }

    fn eval_bool(&self, e: &Expr) -> Result<bool, Stop> {
        match self.eval(e)? {
            Data::Bool(b) => Ok(b),
            _ => Err(Stop::failed("expected a condition")),
        }
    }

    fn eval_str(&self, e: &Expr) -> Result<String, Stop> {
        Ok(data_text(&self.eval(e)?))
    }

    fn eval_list(&self, e: &Expr) -> Result<(Vec<String>, Option<String>), Stop> {
        let withheld = self.expr_withheld(e);
        match self.eval(e)? {
            Data::List(items) => Ok((items, withheld)),
            other => Ok((vec![data_text(&other)], withheld)),
        }
    }

    /// The withheld reason of any value an expression reads, so a withheld value stays withheld through list literals
    /// and string interpolation.
    fn expr_withheld(&self, e: &Expr) -> Option<String> {
        let from_sym = |sym: &Sym| self.values.get(sym).and_then(|v| v.withheld.clone());
        match e {
            Expr::Sym(sym) | Expr::Exists(sym) => from_sym(sym),
            Expr::Str(t) => t.syms().find_map(|s| from_sym(&s)),
            Expr::List(items) => items.iter().find_map(|i| self.expr_withheld(i)),
            Expr::Or(a, b) | Expr::And(a, b) | Expr::Cmp(_, a, b) | Expr::In(a, b) => {
                self.expr_withheld(a).or_else(|| self.expr_withheld(b))
            }
            Expr::Not(inner) | Expr::Regex(inner, _) => self.expr_withheld(inner),
            Expr::Num(_) => None,
        }
    }
}

/// The runner's values, always in the full view: for the journal, which never shortens.
struct Full<'a, 'p>(&'a Runner<'p>);

impl render::Values for Full<'_, '_> {
    fn shown(&self, program: &Program, sym: Sym) -> Option<String> {
        self.0.shown(program, sym)
    }

    fn shown_arg(&self, program: &Program, sym: Sym) -> Option<Vec<String>> {
        self.0.shown_arg(program, sym)
    }
}

impl render::Values for Runner<'_> {
    fn prefixes(&self) -> Option<Prefixes> {
        Runner::prefixes(self)
    }

    fn script_lines(&self, command: &Command, pad: &str) -> Vec<String> {
        if !self.previewing.get() {
            return Vec::new();
        }
        let Some(file) = self.script_file(command) else {
            return Vec::new();
        };
        let gutter = format!("{pad}      │ ");
        let Some(pin) = self.pins.get(&file) else {
            return vec![format!("{gutter}cannot be read")];
        };
        let mut out = match std::str::from_utf8(&pin.bytes) {
            Err(_) => vec![format!(
                "{gutter}binary · {} KB",
                pin.bytes.len().div_ceil(1024)
            )],
            Ok(text) => {
                let lines: Vec<&str> = text.lines().collect();
                let limit = self.opts.script_lines;
                if limit == 0 || lines.len() > limit {
                    vec![format!("{gutter}{} lines · o to open", lines.len())]
                } else {
                    lines
                        .iter()
                        .map(|line| format!("{gutter}{}", render::escape_controls(line)))
                        .collect()
                }
            }
        };
        if !self.pin_holds(&file) {
            out.push(format!(
                "{gutter}changed on disk since the gate; it will not run"
            ));
        }
        out
    }

    fn shown_arg(&self, program: &Program, sym: Sym) -> Option<Vec<String>> {
        let info = program.sym(sym);
        if info.kind == SymKind::KorinOut {
            return None;
        }
        let value = self.values.get(&sym)?;
        if info.secret {
            return Some(vec![format!("«secret:{}»", info.name)]);
        }
        if value.withheld.is_some() {
            return Some(vec![format!("«withheld:{}»", info.name)]);
        }
        Some(match &value.data {
            Data::Str(s) => vec![self.redactor.redact(s)],
            Data::List(items) => items.iter().map(|i| self.redactor.redact(i)).collect(),
            Data::Bool(b) => vec![b.to_string()],
        })
    }

    fn shown(&self, program: &Program, sym: Sym) -> Option<String> {
        let info = program.sym(sym);
        if info.kind == SymKind::KorinOut {
            return None;
        }
        let value = self.values.get(&sym)?;
        if info.secret {
            return Some(format!("«secret:{}»", info.name));
        }
        if value.withheld.is_some() {
            return Some(format!("«withheld:{}»", info.name));
        }
        Some(render::escape_controls(
            &self.redactor.redact(&match &value.data {
                Data::Str(s) => s.clone(),
                Data::List(items) => items
                    .iter()
                    .map(|i| render::quote(i))
                    .collect::<Vec<_>>()
                    .join(" "),
                Data::Bool(b) => b.to_string(),
            }),
        ))
    }
}

fn data_text(data: &Data) -> String {
    match data {
        Data::Str(s) => s.clone(),
        Data::List(items) => items.join(" "),
        Data::Bool(b) => b.to_string(),
    }
}

fn placeholder(bytes: usize, unresolved: &[String]) -> String {
    format!(
        "withheld: {bytes} bytes; unresolved secrets: {}",
        unresolved.join(", ")
    )
}

fn excerpts(mut stop: Stop, redactor: &Redactor, stdout: &[u8], stderr: &[u8]) -> Stop {
    for (label, bytes) in [("stdout", stdout), ("stderr", stderr)] {
        if bytes.is_empty() {
            continue;
        }
        let text = redactor.redact_bytes(bytes);
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(20);
        stop = stop.detail(format!("{label} (last {} lines):", lines.len() - start));
        for line in &lines[start..] {
            stop = stop.detail(format!("  {line}"));
        }
    }
    stop
}

/// Text as `#` comment lines for the run log, one `# ` per line.
fn comment(text: &str) -> String {
    text.lines()
        .map(|line| format!("# {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Indents streamed output, continuing a line that an earlier piece left open.
fn indent_from(text: &str, gutter: &str, line_start: &mut bool) -> String {
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        if *line_start {
            out.push_str(gutter);
        }
        out.push_str(line);
        *line_start = line.ends_with('\n');
    }
    out
}

fn indent(text: &str, gutter: &str) -> String {
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        out.push_str(gutter);
        out.push_str(line);
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn status_json(status: Status) -> serde_json::Value {
    match status {
        Status::Exited(code) => json!({"exit": code}),
        Status::Signaled(signal) => json!({"signal": crate::signals::name(signal)}),
    }
}

fn body_captures(steps: &[Step]) -> Vec<Sym> {
    let mut out = Vec::new();
    for step in steps {
        match &step.kind {
            StepKind::Command(c) => out.extend(c.captures.iter().map(|c| c.sym)),
            StepKind::When { body, .. }
            | StepKind::Each { body, .. }
            | StepKind::Poll { body, .. }
            | StepKind::Call { body, .. } => {
                out.extend(body_captures(body));
            }
            _ => {}
        }
    }
    out
}

/// Every name a run can take as input: params and `ask` answers, with whether each is secret.
fn known_inputs(p: &Program) -> Vec<(String, bool)> {
    fn asks(steps: &[Step], p: &Program, out: &mut Vec<(String, bool)>) {
        for step in steps {
            match &step.kind {
                StepKind::Ask { sym, secret, .. } => out.push((p.sym(*sym).name.clone(), *secret)),
                StepKind::When { body, .. }
                | StepKind::Each { body, .. }
                | StepKind::Poll { body, .. }
                | StepKind::Call { body, .. } => {
                    asks(body, p, out);
                }
                _ => {}
            }
        }
    }
    let mut out: Vec<(String, bool)> = p
        .params
        .iter()
        .map(|param| (p.sym(param.sym).name.clone(), param.secret))
        .collect();
    for stage in p.all_stages() {
        asks(&stage.steps, p, &mut out);
    }
    out
}

fn secret_env(name: &str) -> String {
    format!(
        "KORIN_SECRET_{}",
        name.to_ascii_uppercase().replace(['.', '$', '-'], "_")
    )
}

fn slug(title: &str) -> String {
    let mut slug: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    slug.trim_matches('-').chars().take(40).collect()
}

/// Finds `name` on `PATH` and returns an absolute path. An empty or relative `PATH` entry is
/// resolved against the cwd, so a step's own `env PATH=...` can never send the spawn to a
/// different binary.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let cwd = std::env::current_dir().ok()?;
    std::env::split_paths(&path)
        .map(|dir| cwd.join(dir).join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Creates one directory with mode 0700. Fails if anything already exists at `dir`, including
/// a symlink, so korin never writes into a directory it did not create.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(dir)
}

/// Whether `path` is a real directory (not a symlink) owned by the current user.
fn owned_dir(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.is_dir() && m.uid() == nix::unistd::getuid().as_raw())
}

/// Removes scratch directories left by korin runs that are no longer alive.
fn sweep_scratch() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name
            .strip_prefix("korin-")
            .and_then(|rest| rest.split('-').next())
            .and_then(|pid| pid.parse::<i32>().ok())
        else {
            continue;
        };
        let alive = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok();
        if !alive && owned_dir(&entry.path()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Reads a line from the terminal while watching for Ctrl-C and `SIGTERM`.
fn read_line_interruptible(signals: &Signals, prompt: &str) -> Result<String, Stop> {
    // Discard anything typed before the prompt appeared, so type-ahead never answers a gate
    // the human has not read.
    if std::io::stdin().is_terminal() {
        let _ = nix::sys::termios::tcflush(std::io::stdin(), nix::sys::termios::FlushArg::TCIFLUSH);
    }
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    loop {
        let stdin = std::io::stdin();
        let [sig_int, sig_term, _] = signals.fds();
        let mut fds = [
            PollFd::new(stdin.as_fd(), PollFlags::POLLIN),
            PollFd::new(sig_int, PollFlags::POLLIN),
            PollFd::new(sig_term, PollFlags::POLLIN),
        ];
        let _ = poll(&mut fds, PollTimeout::from(200u16));
        let readable = fds[0].revents().is_some_and(|r| !r.is_empty());
        if signals.take_interrupt() {
            println!();
            return Err(Stop::cancelled("interrupted with Ctrl-C"));
        }
        if signals.take_terminate() {
            println!();
            return Err(Stop::cancelled("korin received SIGTERM or SIGHUP"));
        }
        if readable {
            let mut line = String::new();
            return match stdin.read_line(&mut line) {
                Ok(0) => Err(Stop::cancelled("the terminal closed")),
                Ok(_) => Ok(line.trim_end_matches(['\n', '\r']).to_string()),
                Err(e) => Err(Stop::failed(format!("cannot read the terminal: {e}"))),
            };
        }
    }
}

/// How long a single-key prompt ignores keys after it appears, so a key pressed just as the
/// prompt is drawn, meant for something else, never answers it.
const ARMING: Duration = Duration::from_millis(250);

/// Puts the terminal's modes back when dropped, on every way out of a prompt.
struct RestoreModes(nix::sys::termios::Termios);

impl Drop for RestoreModes {
    fn drop(&mut self) {
        let _ = nix::sys::termios::tcsetattr(
            std::io::stdin(),
            nix::sys::termios::SetArg::TCSANOW,
            &self.0,
        );
    }
}

/// A stage as the skip dialog lists it: `stage 3 "Archive"`, or `preflight "Title"`.
fn stage_label(p: &Program, number: usize) -> String {
    let stage = p.stage(number);
    if stage.is_preflight() {
        format!("preflight \"{}\"", stage.title)
    } else {
        format!("stage {number} \"{}\"", stage.title)
    }
}

/// How the report names a stage in a heading: `Stage 3`, or `Preflight`.
fn heading(stage: &ir::Stage) -> String {
    if stage.is_preflight() {
        "Preflight".to_string()
    } else {
        format!("Stage {}", stage.number)
    }
}

/// Parses a value given on the command line, by the kind of value it replaces.
fn parse_typed(ty: Ty, text: &str) -> Result<Data, String> {
    match ty {
        Ty::Scalar => Ok(Data::Str(text.to_string())),
        Ty::Json => parse_json(text).map(Data::Str),
        Ty::List => parse_list(text).map(Data::List),
        Ty::Bool => match text {
            "true" => Ok(Data::Bool(true)),
            "false" => Ok(Data::Bool(false)),
            _ => Err(format!("an expect is `true` or `false`, not `{text}`")),
        },
    }
}

/// What the command line's skips settle before anything runs.
struct SkipStart {
    /// The stages that will not run, as far as the command line decides.
    skipped: BTreeMap<usize, Why>,
    /// Values given for skipped stages, with who gave them.
    given: Vec<(Sym, Data, &'static str)>,
    /// Set when a later stage reads a value the skips leave unset: the operator settles it
    /// after the preflight.
    decided: Option<BTreeMap<usize, Why>>,
}

/// The static pass for skips. It checks every `--skip`, `--skip-preflight` and `--set`, reads
/// `KORIN_SECRET_<NAME>` for secret captures of skipped stages, and works out what the skips
/// take with them. Without a terminal, a value a later stage needs and nobody gave stops the
/// run here, before anything runs.
fn skip_start(
    p: &Program,
    opts: &Options,
    from: usize,
    provided: &HashMap<String, String>,
    redactor: &Redactor,
    tty: bool,
) -> Result<SkipStart, Stop> {
    let mut decided = BTreeMap::new();
    for n in 1..from {
        decided.insert(n, Why::From);
    }
    for name in &opts.skip {
        let n = resolve_stage(p, name, "skip").map_err(|d| Stop::failed(d.message.clone()))?;
        let stage = p.stage(n);
        if stage.skip == SkipRule::Never {
            return Err(Stop::failed(format!(
                "stage {n} \"{}\" is `skip never`, so `--skip` cannot skip it",
                stage.title
            )));
        }
        if n >= from {
            decided.insert(n, Why::Cli);
        }
    }
    if opts.skip_preflight {
        let Some(preflight) = &p.preflight else {
            return Err(Stop::failed(
                "`--skip-preflight` was given, but the wizard has no preflight",
            ));
        };
        if preflight.skip == SkipRule::Never {
            return Err(Stop::failed(
                "the preflight is `skip never`, so `--skip-preflight` cannot skip it",
            ));
        }
        decided.insert(0, Why::SkipPreflight);
    }
    let mut given: Vec<(Sym, Data, &'static str)> = Vec::new();
    for (name, text) in &opts.set {
        let found = p
            .syms
            .iter()
            .position(|s| s.name == *name && matches!(s.kind, SymKind::Capture | SymKind::Expect));
        let Some(sym) = found else {
            let mut stop = Stop::failed(format!(
                "`--set {name}`: no capture or expect is named `{name}`"
            ));
            if p.syms
                .iter()
                .any(|s| s.name == *name && s.kind == SymKind::Ask)
            {
                stop = stop.detail(format!("give an `ask` its answer with --param {name}=..."));
            }
            return Err(stop);
        };
        let info = p.sym(sym);
        if info.secret {
            return Err(Stop::failed(format!(
                "secret `{name}` cannot be passed on the command line"
            ))
            .detail(format!(
                "set the environment variable {} instead",
                secret_env(name)
            )));
        }
        let data =
            parse_typed(info.ty, text).map_err(|e| Stop::failed(format!("`--set {name}`: {e}")))?;
        given.push((sym, data, "--set"));
    }
    for (sym, info) in p.syms.iter().enumerate() {
        if info.kind == SymKind::Capture
            && info.secret
            && let Ok(value) = std::env::var(secret_env(&info.name))
        {
            redactor
                .add_value(&info.name, &value)
                .map_err(Stop::failed)?;
            given.push((sym, Data::Str(value), "env"));
        }
    }
    let have = |sym: Sym| {
        let info = p.sym(sym);
        given.iter().any(|(g, _, _)| *g == sym)
            || (info.kind == SymKind::Ask && provided.contains_key(&info.name))
    };
    let closure = skip::closure(p, &decided, &[], None, &have);
    for (sym, _, by) in &given {
        let info = p.sym(*sym);
        let stage = info.stage.unwrap_or(0);
        if *by == "--set" && !closure.stages.contains_key(&stage) {
            return Err(Stop::failed(format!(
                "`--set {}`: {} runs, and makes this value itself",
                info.name,
                p.stage(stage).name()
            ))
            .detail("--set gives only values of stages that are skipped"));
        }
    }
    given.retain(|(sym, _, _)| {
        p.sym(*sym)
            .stage
            .is_some_and(|s| closure.stages.contains_key(&s))
    });
    let readers: Vec<(usize, Vec<Sym>)> = closure
        .readers()
        .map(|(n, syms)| (n, syms.to_vec()))
        .collect();
    if readers.is_empty() {
        return Ok(SkipStart {
            skipped: closure.stages,
            given,
            decided: None,
        });
    }
    if !tty {
        let mut stop = Stop::failed("skipped stages leave values unset that later stages read");
        for (n, syms) in &readers {
            let names: Vec<String> = syms
                .iter()
                .map(|s| format!("`{}`", p.sym(*s).name))
                .collect();
            stop = stop.detail(format!("{} reads {}", stage_label(p, *n), names.join(", ")));
        }
        for sym in &closure.missing {
            let info = p.sym(*sym);
            let how = match info.kind {
                SymKind::Ask if !info.secret => format!("--param {}=...", info.name),
                _ if info.secret => secret_env(&info.name),
                _ => format!("--set {}=...", info.name),
            };
            stop = stop.detail(format!("give `{}` with {how}", info.name));
        }
        let skip: Vec<String> = readers.iter().map(|(n, _)| format!("--skip {n}")).collect();
        stop = stop.detail(format!("or skip those stages too: {}", skip.join(" ")));
        return Err(stop);
    }
    Ok(SkipStart {
        skipped: decided.clone(),
        given,
        decided: Some(decided),
    })
}

/// How the journal names a stage: its number, or `"preflight"`.
fn stage_id(stage: &ir::Stage) -> serde_json::Value {
    if stage.is_preflight() {
        json!("preflight")
    } else {
        json!(stage.number)
    }
}

/// Bytes as lowercase hex, such as a hash for the journal.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// What a choice prompt shows once a key answers it.
fn choice_echo(c: char) -> &'static str {
    match c {
        'r' => "r",
        'c' => "c",
        'a' => "a",
        's' => "s",
        'v' => "v",
        'y' => "y",
        'n' => "n",
        _ => "",
    }
}

/// Checks a line of JSON, and gives it back in its compact form.
fn parse_json(line: &str) -> Result<String, String> {
    serde_json::from_str::<serde_json::Value>(line)
        .map(|v| v.to_string())
        .map_err(|e| format!("not JSON: {e}"))
}

/// Parses a JSON array of strings, as a list value.
fn parse_list(text: &str) -> Result<Vec<String>, String> {
    serde_json::from_str::<Vec<String>>(text)
        .map_err(|e| format!("not a JSON array of strings: {e}"))
}

/// Reads a hidden value from the terminal. Nothing typed is echoed: the prompt shows only a
/// live count of the characters so far, so a person can tell that a paste arrived. Backspace
/// removes the last character, Ctrl-U clears the line, Enter submits, and escape sequences
/// such as arrow keys are ignored. Once submitted, the line shows `✓` in place of the count,
/// so the length never stays in the scrollback. Ctrl-C and `SIGTERM` cancel the run.
fn read_hidden_interruptible(
    signals: &Signals,
    prompt: &str,
    theme: Theme,
) -> Result<Zeroizing<String>, Stop> {
    use nix::sys::termios::{LocalFlags, SetArg, SpecialCharacterIndices, tcgetattr, tcsetattr};
    use std::io::Read as _;

    let stdin = std::io::stdin();
    let saved = tcgetattr(&stdin)
        .map_err(|e| Stop::failed(format!("cannot read the terminal's modes: {e}")))?;
    let mut keys = saved.clone();
    keys.local_flags
        .remove(LocalFlags::ICANON | LocalFlags::ECHO);
    keys.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
    keys.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
    let _ = nix::sys::termios::tcflush(&stdin, nix::sys::termios::FlushArg::TCIFLUSH);
    tcsetattr(&stdin, SetArg::TCSANOW, &keys)
        .map_err(|e| Stop::failed(format!("cannot set the terminal's modes: {e}")))?;
    let mut restore = saved;
    restore.local_flags.remove(LocalFlags::PENDIN);
    let _restore = RestoreModes(restore);

    // Bytes, so a UTF-8 character split across two reads is kept whole.
    let mut value: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::new());
    let draw = |value: &[u8]| {
        let count = String::from_utf8_lossy(value).chars().count();
        print!("\r{prompt}{}\x1b[K", theme.hidden_count(count));
        let _ = std::io::stdout().flush();
    };
    draw(&value);
    let mut escape = false;
    loop {
        let [sig_int, sig_term, _] = signals.fds();
        let mut fds = [
            PollFd::new(stdin.as_fd(), PollFlags::POLLIN),
            PollFd::new(sig_int, PollFlags::POLLIN),
            PollFd::new(sig_term, PollFlags::POLLIN),
        ];
        let _ = poll(&mut fds, PollTimeout::from(50u16));
        let readable = fds[0].revents().is_some_and(|r| !r.is_empty());
        if signals.take_interrupt() {
            println!();
            return Err(Stop::cancelled("interrupted with Ctrl-C"));
        }
        if signals.take_terminate() {
            println!();
            return Err(Stop::cancelled("korin received SIGTERM or SIGHUP"));
        }
        if !readable {
            continue;
        }
        let mut buf = Zeroizing::new([0u8; 256]);
        let read = match stdin.lock().read(&mut buf[..]) {
            Ok(0) => return Err(Stop::cancelled("the terminal closed")),
            Ok(n) => n,
            Err(e) => return Err(Stop::failed(format!("cannot read the terminal: {e}"))),
        };
        for &byte in &buf[..read] {
            if escape {
                // An escape sequence ends with a byte in 0x40..=0x7e, after its `[` or `O`.
                if (0x40..=0x7e).contains(&byte) && byte != b'[' && byte != b'O' {
                    escape = false;
                }
                continue;
            }
            match byte {
                b'\r' | b'\n' => {
                    print!("\r{prompt}{}\x1b[K\r\n", theme.hidden_done());
                    let _ = std::io::stdout().flush();
                    return match String::from_utf8(std::mem::take(&mut *value)) {
                        Ok(text) => Ok(Zeroizing::new(text)),
                        Err(e) => {
                            let mut bytes = e.into_bytes();
                            zeroize::Zeroize::zeroize(&mut bytes);
                            Err(Stop::failed("the hidden value is not valid UTF-8"))
                        }
                    };
                }
                0x1b => escape = true,
                0x7f | 0x08 => {
                    // Drop the last character: its continuation bytes, then its first byte.
                    while value.last().is_some_and(|b| b & 0xc0 == 0x80) {
                        value.pop();
                    }
                    value.pop();
                }
                0x15 => value.clear(),
                b if b < 0x20 => {}
                b => value.push(b),
            }
        }
        draw(&value);
    }
}

/// Reads one key from the terminal that `answer` accepts, and echoes the text it gives.
/// `answer` sees the bytes of one read, and returns `None` to ignore them. Keys typed before
/// the prompt, or within [`ARMING`] after it, are discarded. Ctrl-C and `SIGTERM` still
/// cancel the run: signals stay on.
fn read_key_interruptible<T>(
    signals: &Signals,
    prompt: &str,
    answer: impl Fn(&[u8]) -> Option<(T, &'static str)>,
) -> Result<(T, &'static str), Stop> {
    use nix::sys::termios::{LocalFlags, SetArg, SpecialCharacterIndices, tcgetattr, tcsetattr};
    use std::io::Read as _;

    let stdin = std::io::stdin();
    let saved = tcgetattr(&stdin)
        .map_err(|e| Stop::failed(format!("cannot read the terminal's modes: {e}")))?;
    let mut keys = saved.clone();
    keys.local_flags
        .remove(LocalFlags::ICANON | LocalFlags::ECHO);
    keys.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
    keys.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
    let _ = nix::sys::termios::tcflush(&stdin, nix::sys::termios::FlushArg::TCIFLUSH);
    tcsetattr(&stdin, SetArg::TCSANOW, &keys)
        .map_err(|e| Stop::failed(format!("cannot set the terminal's modes: {e}")))?;
    let mut restore = saved;
    // The kernel's own flag for "reprint pending input"; restoring it would echo stray keys.
    restore.local_flags.remove(LocalFlags::PENDIN);
    let _restore = RestoreModes(restore);
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let armed = Instant::now() + ARMING;
    loop {
        let [sig_int, sig_term, _] = signals.fds();
        let mut fds = [
            PollFd::new(stdin.as_fd(), PollFlags::POLLIN),
            PollFd::new(sig_int, PollFlags::POLLIN),
            PollFd::new(sig_term, PollFlags::POLLIN),
        ];
        let _ = poll(&mut fds, PollTimeout::from(50u16));
        let readable = fds[0].revents().is_some_and(|r| !r.is_empty());
        if signals.take_interrupt() {
            println!();
            return Err(Stop::cancelled("interrupted with Ctrl-C"));
        }
        if signals.take_terminate() {
            println!();
            return Err(Stop::cancelled("korin received SIGTERM or SIGHUP"));
        }
        if !readable {
            continue;
        }
        let mut buf = [0u8; 64];
        let read = match stdin.lock().read(&mut buf) {
            Ok(0) => return Err(Stop::cancelled("the terminal closed")),
            Ok(n) => n,
            Err(e) => return Err(Stop::failed(format!("cannot read the terminal: {e}"))),
        };
        if Instant::now() < armed {
            continue;
        }
        if let Some((value, echo)) = answer(&buf[..read]) {
            println!("{echo}");
            return Ok((value, echo));
        }
    }
}

/// Whether `path` is a regular file (not a symlink) strictly inside `scratch`. The parent is
/// resolved through any symlinks, so a linked directory cannot lead outside the scratch
/// directory either.
fn private_file(path: &Path, scratch: &Path) -> bool {
    let normal = crate::compile::normalize(path);
    let (Some(parent), Some(name)) = (normal.parent(), normal.file_name()) else {
        return false;
    };
    let (Ok(parent), Ok(scratch)) = (parent.canonicalize(), scratch.canonicalize()) else {
        return false;
    };
    parent.starts_with(&scratch)
        && std::fs::symlink_metadata(parent.join(name)).is_ok_and(|m| m.file_type().is_file())
}

/// A file a capture read: its path and its bytes.
type CapturedFile = (PathBuf, Zeroizing<Vec<u8>>);

/// Removes a scratch directory when dropped, so every exit path cleans up, including a panic.
struct ScratchGuard(PathBuf);

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `$PWD`, if it is another path to `cwd`.
fn pwd_alias(cwd: &Path) -> Option<String> {
    let pwd = PathBuf::from(std::env::var_os("PWD")?);
    let same =
        pwd.is_absolute() && pwd != cwd && pwd.canonicalize().ok()? == cwd.canonicalize().ok()?;
    same.then(|| pwd.to_string_lossy().into_owned())
}

/// A path with the home directory shown as `~`, as a shell prompt shows it.
fn home_relative(path: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if path == home => "~".into(),
        Some(home) => match path.strip_prefix(&home) {
            Ok(rest) if !home.as_os_str().is_empty() && home != Path::new("/") => {
                format!("~/{}", rest.display())
            }
            _ => path.display().to_string(),
        },
        None => path.display().to_string(),
    }
}
