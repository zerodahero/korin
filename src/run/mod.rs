//! Running a compiled program.
//!
//! The runner walks the stages in order. Each stage is shown first and, when it has a gate,
//! approved by the human before any of its commands run.

pub mod capture;
pub mod journal;
mod log;
mod pane;
pub mod process;
pub use log::Tone;
pub use pane::Mark;
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
use capture::{Data, Extract, Fingerprint, extract_with};
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
    /// The command that opens each kind of item at `o`, by kind name, where one is set.
    pub openers: Vec<(String, String)>,
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

/// One finished piece of the run log, held while a poll decides a try's fate.
#[derive(Debug)]
enum Block {
    /// A command: its `$` line, its output and its notes.
    Command(CommandBlock),
    /// Lines that print as they are, such as a `#` comment or a `↻` line.
    Lines(String),
}

/// A command's part of the run log, built while it runs. It holds data, not a rendering:
/// whether the output folds is decided when the block reaches the run log.
#[derive(Debug, Default)]
struct CommandBlock {
    /// The `$` line and the notes that name its variables, colored.
    head: String,
    /// The first lines of output, for a fold.
    first: Vec<String>,
    /// How many lines of output the command printed.
    lines: usize,
    /// The complete output, when it was published.
    spool: Option<PathBuf>,
    /// The journal's file for the output, as the fold note shows it.
    journal: String,
    /// The `↳` notes under the output.
    notes: Vec<String>,
    /// Whether the command succeeded.
    ok: bool,
    /// The step's id and its `$` line as plain text, for the `o` picker.
    step: String,
    label: String,
    /// Shows in full wherever it lands: it belongs to a poll try that failed or was stopped.
    full: bool,
    /// How the command ended, once it has.
    exit: Option<log::Exit>,
    /// How many rows of `head` are the `$` line, before its notes.
    shell_rows: usize,
    /// What the command's `output` line says its output is.
    output: Option<crate::ast::OutputKind>,
}

/// A poll that runs now, as its pane lines show it.
#[derive(Debug, Clone)]
struct PollTrack {
    started: Instant,
    deadline: Instant,
    attempt: usize,
    /// When the current try started, or `None` while waiting.
    try_started: Option<Instant>,
    /// When the next try starts, while waiting.
    wake: Option<Instant>,
    last: Option<String>,
}

/// A folded output that `o` can open in full.
#[derive(Debug, Clone)]
struct Folded {
    step: String,
    label: String,
    hidden: usize,
    spool: PathBuf,
    /// What the command's `output` line says its output is.
    output: Option<crate::ast::OutputKind>,
}

/// Where a stage stands, as the plan pane shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StageState {
    Running(Instant),
    Done(Duration),
    Skipped,
    Failed,
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
    /// Scroll a prompt's content that is taller than the terminal, then ask again.
    Up,
    Down,
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
            Key::Up => "↑",
            Key::Down => "↓",
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
    /// `↑`/`↓` or `k`/`j`: scroll content taller than the terminal. Set by
    /// [`Runner::prompt`] when the content does not fit.
    scroll: bool,
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
            parts.push("o open".to_string());
        }
        if self.scroll {
            parts.push("↑↓ scroll".to_string());
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
            [0x1b, b'[' | b'O', b'A', ..] | [b'k' | b'K', ..] if self.scroll => Some(Key::Up),
            [0x1b, b'[' | b'O', b'B', ..] | [b'j' | b'J', ..] if self.scroll => Some(Key::Down),
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
            "k" | "K" if self.scroll => Some(Key::Up),
            "j" | "J" if self.scroll => Some(Key::Down),
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
    /// The terminal view: with a live view, the plan pane stays below the run log.
    screen: RefCell<Screen>,
    /// While a stage runs, the id of the step running now (empty before the first).
    current_step: Option<String>,
    /// How each stage that started or was skipped ended, by stage number, for the pane.
    stage_states: HashMap<usize, StageState>,
    /// How many times each `retry` stage was retried, by stage number.
    retries: HashMap<usize, usize>,
    /// How much of each path and value the terminal shows. The toggle key switches it.
    view: Cell<View>,
    /// Values that name a directory, as path prefixes for the compact view: each name's
    /// current value, already redacted, in the order the names were first set.
    named: Vec<(Sym, String)>,
    /// Whether the compact view shows its legend: `<name> = <path>` lines. `l` switches it.
    legend: Cell<bool>,
    /// While a preview draws, the prefix labels it shows, for the legend under it.
    legend_used: RefCell<Option<Vec<String>>>,
    /// The secret variables a `$` line has already explained, each named once per run.
    announced_vars: HashSet<String>,
    /// Lines a dialog leaves for the run log, printed once the dialog closes.
    records: Vec<String>,
    /// Whether the run log is inside a stage, so its lines sit indented under the banner.
    indented: Cell<bool>,
    /// The block of the command running now, while a live view builds it.
    building: Option<CommandBlock>,
    /// How the command that ends now ended, for its exit tag.
    last_exit: Option<log::Exit>,
    /// Blocks a poll holds back until it knows a try's fate.
    scopes: log::Scopes<Block>,
    /// The outputs folded so far, oldest first, for the `o` picker.
    folded: RefCell<Vec<Folded>>,
    /// The polls running now, innermost last, for the pane's poll lines.
    polls: RefCell<Vec<PollTrack>>,
    /// How far a prompt's content is scrolled, in lines, when it is taller than the terminal.
    pane_scroll: Cell<usize>,
    /// While a prompt's content is taller than the terminal, how many of its lines are below
    /// the part on screen; `None` while it fits.
    pane_below: Cell<Option<usize>>,
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
        // The live view erases and redraws lines, so it needs a terminal it can measure. The
        // plan pane is told apart from the run log mostly by color, so it also needs color.
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
            current_step: None,
            stage_states: HashMap::new(),
            retries: HashMap::new(),
            view: Cell::new(if tty { view } else { View::Full }),
            named: Vec::new(),
            legend: Cell::new(legend),
            legend_used: RefCell::new(None),
            announced_vars: HashSet::new(),
            records: Vec::new(),
            indented: Cell::new(false),
            building: None,
            last_exit: None,
            scopes: log::Scopes::default(),
            folded: RefCell::new(Vec::new()),
            polls: RefCell::new(Vec::new()),
            pane_scroll: Cell::new(0),
            pane_below: Cell::new(None),
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
        let run_started = Instant::now();
        let p = self.p;
        let result = self.params().and_then(|()| {
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
        self.screen.borrow_mut().clear_all();
        let ending = match &result {
            Ok(()) => Ending::Completed,
            Err(stop) => stop.ending,
        };
        self.say_summary(ending, run_started.elapsed());
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

    /// The plan pane's last state, kept in the run log as the run's summary: each stage with
    /// how it ended and the time it took. Printed only once a stage has started or been
    /// skipped.
    fn say_summary(&self, ending: Ending, took: Duration) {
        if self.stage_states.is_empty() {
            return;
        }
        let theme = self.opts.theme;
        let word = match ending {
            Ending::Completed => "done",
            Ending::Ended => "ended",
            Ending::Failed => "failed",
            Ending::Cancelled => "cancelled",
        };
        let head = format!(
            "── {word} · {} · exit {} ",
            elapsed(took),
            ending.exit_code()
        );
        let fill = render::RULE_MAX
            .min(self.width())
            .saturating_sub(head.chars().count())
            .max(2);
        let mut lines = vec![
            String::new(),
            theme.faint(&format!("{head}{}", "─".repeat(fill))),
        ];
        let total = self.p.stages.len();
        for stage in self.p.preflight.iter().chain(self.p.stages.iter()) {
            let label = if stage.is_preflight() {
                format!("preflight · {}", render::escape_controls(&stage.title))
            } else {
                format!(
                    "{}/{total} {}",
                    stage.number,
                    render::escape_controls(&stage.title)
                )
            };
            let label = self.redactor.redact(&label);
            let line = match self.stage_states.get(&stage.number) {
                Some(StageState::Done(took)) => {
                    let tail = match self.retries.get(&stage.number) {
                        Some(n) => format!("{} · retried {n}×", elapsed(*took)),
                        None => elapsed(*took),
                    };
                    theme.pane_stage(pane::Mark::Done, &label, &tail)
                }
                Some(StageState::Skipped) => {
                    theme.pane_stage(pane::Mark::Skipped, &label, "skipped")
                }
                Some(StageState::Failed | StageState::Running(_)) => {
                    let tail = if ending == Ending::Cancelled {
                        "stopped"
                    } else {
                        "failed"
                    };
                    theme.pane_stage(pane::Mark::Failed, &label, tail)
                }
                None => theme.pane_stage(pane::Mark::Pending, &label, "not run"),
            };
            lines.push(line);
        }
        self.screen
            .borrow_mut()
            .print(&format!("{}\n", lines.join("\n")));
    }

    /// Prints a line to the terminal: redacted first, then colored.
    ///
    /// A line may mark the values korin put into a command, so the theme can color them.
    /// Redaction runs on the marked line, and the marks must not split a secret: the line is
    /// colored only if removing the marks gives exactly the plain line, redacted. Otherwise it
    /// prints that plain line without value colors.
    fn say(&self, line: &str) {
        let line = self.pad_lines(line);
        let line = self.redact_marked(&line);
        self.note_prefixes(&line);
        let shown = self.opts.theme.lines(&line);
        self.screen.borrow_mut().print(&format!("{shown}\n"));
    }

    /// The indent for the run log's lines: two spaces inside a stage, under its banner.
    fn pad(&self) -> &'static str {
        if self.indented.get() { "  " } else { "" }
    }

    /// Indents every line of `text` that is not blank, inside a stage.
    fn pad_lines(&self, text: &str) -> String {
        let pad = self.pad();
        if pad.is_empty() {
            return text.to_string();
        }
        text.split('\n')
            .map(|line| {
                if line.is_empty() {
                    String::new()
                } else {
                    format!("{pad}{line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Adds lines to the run log, or to the poll try that holds them.
    fn log_line(&mut self, line: &str) {
        if self.scopes.holding() {
            let line = self.redact_marked(line);
            self.scopes.emit(Block::Lines(line));
        } else {
            self.say(line);
        }
    }

    /// A block that holds commands, laid out to read well (`broken`), or on one line each
    /// (`one`) if the broken form would redact differently: redaction must never depend on
    /// where a command breaks. It comes back redacted, cut in the compact view, and colored.
    fn block_text(&self, one: &str, broken: &str) -> String {
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
        self.opts.theme.lines(&chosen)
    }

    /// While a preview draws, remembers each prefix label the text shows.
    fn note_prefixes(&self, text: &str) {
        let mut used = self.legend_used.borrow_mut();
        let Some(used) = used.as_mut() else {
            return;
        };
        for label in legend_labels(text) {
            if !used.contains(&label) {
                used.push(label);
            }
        }
    }

    /// A preview's lines, colored: its steps with their commands laid out to read well,
    /// then the legend for the prefixes they show.
    fn preview_lines(&self, steps: &[&Step]) -> Vec<String> {
        *self.legend_used.borrow_mut() = Some(Vec::new());
        self.previewing.set(true);
        let mut one = Vec::new();
        let mut broken = Vec::new();
        for step in steps {
            render::step_lines(self.p, step, 1, self, None, &mut one);
            render::step_lines(self.p, step, 1, self, Some(self.width()), &mut broken);
        }
        let mut lines = Vec::new();
        if !one.is_empty() {
            let text = self.block_text(&one.join("\n"), &broken.join("\n"));
            lines.extend(text.split('\n').map(str::to_string));
        }
        self.previewing.set(false);
        let used = self.legend_used.borrow_mut().take().unwrap_or_default();
        lines.extend(self.legend_line(&used));
        lines
    }

    /// Whether legend lines show: in the compact view, unless `l` turned them off.
    fn legend_on(&self) -> bool {
        self.legend.get() && self.view.get() == View::Compact
    }

    /// The path a prefix label stands for, as the legend shows it. `~` has no entry.
    fn legend_path(&self, label: &str) -> Option<String> {
        let path = match label {
            // A new scratch directory for each step, under the run's own scratch root.
            KORIN_OUT_LABEL => {
                let root = home_relative(&self.scratch_root);
                return Some(render::escape_controls(&format!("{root}/<step>")));
            }
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

    /// The legend line for some prefix labels, if legend lines show.
    fn legend_line(&self, labels: &[String]) -> Option<String> {
        if !self.legend_on() {
            return None;
        }
        let entries: Vec<(String, String)> = labels
            .iter()
            .filter_map(|label| Some((label.clone(), self.legend_path(label)?)))
            .collect();
        if entries.is_empty() {
            return None;
        }
        Some(self.opts.theme.legend(&entries, None))
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
        match (self.named.iter().position(|(s, _)| *s == sym), prefix) {
            (Some(index), Some(path)) => self.named[index].1 = path,
            (Some(index), None) => {
                self.named.remove(index);
            }
            (None, Some(path)) => self.named.push((sym, path)),
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

    /// With a live view, draws the plan pane under the run log: the stages that ended, the
    /// current stage with the step running now, and the stages to come.
    fn update_pane(&self) {
        if !self.screen.borrow().live() {
            return;
        }
        let plan = self.pane_plan();
        let theme = self.opts.theme;
        let lines = pane::fit(&plan, screen::height(), &|text| theme.faint(text));
        self.screen.borrow_mut().set_pane(lines);
    }

    /// Shows what a prompt asks about, such as a gate's stage. In a live view it takes the
    /// plan pane, scrolled when it is taller than the terminal, and the prompt sits under it.
    /// Otherwise it prints once, before the prompt asks.
    fn show_prompt_pane(&self, lines: Vec<String>) {
        let mut screen = self.screen.borrow_mut();
        if !screen.live() {
            for line in lines {
                screen.print(&format!("{line}\n"));
            }
            return;
        }
        // Rows for the content: the terminal, less the prompt line, its box, and one to spare.
        let room = screen::height().saturating_sub(5).max(3) - 1;
        let cols = screen::width();
        let rows_of = |line: &String| screen::rows(&format!("{line}\n"), cols);
        if lines.iter().map(rows_of).sum::<usize>() <= room {
            self.pane_below.set(None);
            self.pane_scroll.set(0);
            screen.set_pane(lines);
            return;
        }
        let theme = self.opts.theme;
        let offset = self.pane_scroll.get().min(lines.len().saturating_sub(1));
        self.pane_scroll.set(offset);
        let mut out = Vec::new();
        // One row for the line that says what is below.
        let mut used = 1;
        if offset > 0 {
            out.push(theme.faint(&format!("  … {offset} lines above")));
            used += 1;
        }
        let mut shown = 0;
        for line in &lines[offset..] {
            let r = rows_of(line);
            if used + r > room {
                break;
            }
            used += r;
            out.push(line.clone());
            shown += 1;
        }
        let below = lines.len() - offset - shown;
        self.pane_below.set(Some(below));
        out.push(theme.faint(&if below > 0 {
            format!("  … {below} lines below")
        } else {
            "  (end)".to_string()
        }));
        screen.set_pane(out);
    }

    /// A gate's or a pause's content: the stage's rule, then, except in a guide, every step
    /// with its commands as they will run.
    fn gate_lines(&self, stage: &ir::Stage) -> Vec<String> {
        let rule = self.redactor.redact(&self.rule(stage));
        let mut lines = vec![self.opts.theme.lines(&rule)];
        if self.p.mode != Mode::Guide && !stage.steps.is_empty() {
            lines.push(String::new());
            lines.extend(self.preview_lines(&stage.steps.iter().collect::<Vec<_>>()));
        }
        lines
    }

    /// Erases a gate's pane, its prompt, and anything printed since the mark, then prints
    /// the records its dialogs left.
    fn close_gate(&mut self) {
        {
            let mut screen = self.screen.borrow_mut();
            screen.erase_to_mark();
            screen.clear_all();
        }
        self.pane_below.set(None);
        self.pane_scroll.set(0);
        self.flush_records();
    }

    /// The banner that opens a stage in the run log, with how its gate went:
    /// `━━ 2/4 Snapshot ━━━━━━━━ approved ━━`.
    fn stage_banner(&self, stage: &ir::Stage, what: &str) -> String {
        let title = render::escape_controls(&stage.title);
        let head = if stage.is_preflight() {
            format!("━━ preflight · {title} ")
        } else {
            format!("━━ {}/{} {title} ", stage.number, self.p.stages.len())
        };
        let tail = format!(" {what} ━━");
        let used = head.chars().count() + tail.chars().count();
        let fill = render::RULE_MAX
            .min(self.width())
            .saturating_sub(used)
            .max(2);
        format!("{head}{}{tail}", "━".repeat(fill))
    }

    /// The plan pane's content, themed, before it is fitted to the terminal.
    fn pane_plan(&self) -> pane::Plan {
        let theme = self.opts.theme;
        let width = self.width().saturating_sub(1).max(20);
        let total = self.p.stages.len();
        let mut plan = pane::Plan::default();
        let stages = self.p.preflight.iter().chain(self.p.stages.iter());
        for stage in stages {
            let label = if stage.is_preflight() {
                format!("preflight · {}", render::escape_controls(&stage.title))
            } else {
                format!(
                    "{}/{total} {}",
                    stage.number,
                    render::escape_controls(&stage.title)
                )
            };
            let label = self.redactor.redact(&label);
            match self.stage_states.get(&stage.number) {
                Some(StageState::Running(_)) if stage.number == self.stage_now => {
                    plan.current = Some(theme.pane_stage(pane::Mark::Current, &label, ""));
                }
                Some(StageState::Done(took)) => {
                    plan.done
                        .push(theme.pane_stage(pane::Mark::Done, &label, &elapsed(*took)));
                }
                Some(StageState::Skipped) => {
                    plan.done
                        .push(theme.pane_stage(pane::Mark::Skipped, &label, "skipped"));
                }
                Some(StageState::Failed | StageState::Running(_)) => {
                    plan.done
                        .push(theme.pane_stage(pane::Mark::Failed, &label, ""));
                }
                None => plan
                    .later
                    .push(theme.pane_stage(pane::Mark::Pending, &label, "")),
            }
        }
        // The labels the pane's lines show, for its legend.
        let mut used: Vec<String> = Vec::new();
        if plan.current.is_some() {
            let stage = self.p.stage(self.stage_now);
            let current = self.current_step.as_deref().unwrap_or_default();
            let mut lines = Vec::new();
            // A guide shows no preview, so its pane shows no steps to come either.
            if self.p.mode == Mode::Guide {
                if let Some(step) = find_step(&stage.steps, current) {
                    render::step_lines(self.p, step, 2, self, None, &mut lines);
                    lines.truncate(1);
                }
            } else {
                for step in &stage.steps {
                    render::step_lines(self.p, step, 2, self, None, &mut lines);
                }
            }
            let at = lines.iter().position(|l| line_id(l) == Some(current));
            let compact = self.view.get() == View::Compact;
            for (index, line) in lines.iter().enumerate() {
                let line = self.redact_marked(line);
                let line = if compact {
                    crate::view::cut_lines(&line, width)
                } else {
                    line
                };
                for label in legend_labels(&line) {
                    if !used.contains(&label) {
                        used.push(label);
                    }
                }
                let line = pane::clip(&line, width);
                let mark = match at {
                    Some(at) if index == at => pane::Mark::Current,
                    Some(at) if index < at && line_id(&line).is_some() => pane::Mark::Done,
                    _ if line_id(&line).is_some() => pane::Mark::Pending,
                    _ => pane::Mark::None,
                };
                plan.steps.push(theme.pane_step(mark, &line));
            }
            plan.at = at;
            if let Some(track) = self.polls.borrow().last() {
                let now = Instant::now();
                let view = pane::PollView {
                    attempt: track.attempt,
                    elapsed: now.saturating_duration_since(track.started),
                    limit: track.deadline.saturating_duration_since(track.started),
                    phase: match (track.try_started, track.wake) {
                        (Some(at), _) => pane::Phase::Running(now.saturating_duration_since(at)),
                        (None, wake) => pane::Phase::Waiting(
                            wake.map_or(Duration::ZERO, |w| w.saturating_duration_since(now)),
                        ),
                    },
                    last: track.last.clone(),
                };
                let (state, bar) = pane::poll_lines(&view);
                let state = pane::clip(&self.redactor.redact(&state), width.saturating_sub(8));
                plan.status = vec![
                    theme.poll_state(&state),
                    theme.faint(&format!("        {bar}")),
                ];
            }
        }
        if self.legend_on() {
            // Every name known so far, then every other label the pane's lines use.
            let mut labels: Vec<String> = self
                .named
                .iter()
                .map(|(sym, _)| format!("<{}>", self.p.sym(*sym).name))
                .collect();
            for label in used {
                if !labels.contains(&label) {
                    labels.push(label);
                }
            }
            let entries: Vec<(String, String)> = labels
                .into_iter()
                .filter_map(|label| Some((label.clone(), self.legend_path(&label)?)))
                .collect();
            if !entries.is_empty() {
                plan.legend = Some(theme.legend(&entries, Some(width)));
            }
        }
        plan
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
                let secret = param.secret;
                self.in_dialog(|r| {
                    let value = r.prompt_value(&name, secret)?;
                    r.records
                        .push(answer_record(&name, &value, secret, "param"));
                    Ok(value)
                })?
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
            let prompt = self.opts.theme.ask(&format!("{name} (hidden):"));
            let framed = self.frame(&prompt);
            let value = read_hidden_interruptible(&self.signals, &framed, self.opts.theme)?;
            let done = self.opts.theme.hidden_done();
            self.screen.borrow_mut().prompted(
                &format!("{}{done}", framed.shown),
                &format!("{prompt}{done}"),
            );
            Ok(value.to_string())
        } else {
            self.read_line(&self.opts.theme.ask(&format!("{name}:")))
        }
    }

    /// A prompt in a box, in a live view, so a glance finds it under the output above. The
    /// box's top border joins the pane. With no live view, or when keys come as lines, the
    /// prompt is plain.
    fn frame(&self, prompt: &str) -> Framed {
        let live = self.screen.borrow().live();
        if !live
            || self.opts.require_enter
            || !std::io::stdin().is_terminal()
            || prompt.contains('\n')
        {
            return Framed::plain(prompt);
        }
        self.screen.borrow_mut().open_box();
        let theme = self.opts.theme;
        let cols = screen::width().max(4);
        // The box is one column narrower than the terminal, like the rule above the pane.
        let inner = cols - 1;
        let side = theme.box_side();
        // The box sets the prompt apart, so it drops its own indent.
        let shown = format!("{side} {}", prompt.trim_start_matches(' '));
        let used = screen::display_width(&shown);
        // The right side goes in the box's last column, if the prompt leaves room for it and
        // an answer. A wider prompt wraps, and its box has no right side.
        let right = if used + 4 <= inner {
            format!("\x1b7\x1b[{inner}G{side}\x1b8")
        } else {
            String::new()
        };
        // Down a row for the bottom border, then back up to the end of the prompt. Moving
        // relative to the cursor still works when the border scrolls the screen.
        let bottom = format!(
            "\r\n{}\x1b[1A\x1b[{}G",
            theme.box_border(false, inner),
            used % cols + 1
        );
        Framed {
            shown,
            right,
            bottom,
        }
    }

    /// Reads a line from the terminal. Ctrl-C and `SIGTERM` cancel the run instead of being lost.
    fn read_line(&self, prompt: &str) -> Result<String, Stop> {
        let framed = self.frame(prompt);
        let answer = read_line_interruptible(&self.signals, &framed.shown, &framed.after())?;
        self.screen.borrow_mut().prompted(
            &format!("{}{answer}", framed.shown),
            &format!("{prompt}{answer}"),
        );
        Ok(answer)
    }

    /// Reads a number from a list, such as the `o` picker's. In a terminal it reads keys:
    /// digits and Backspace edit the answer, Enter gives it, and Esc goes back at once,
    /// whatever was typed. Going back gives an empty answer.
    fn read_number(&self, what: &str) -> Result<String, Stop> {
        let keys = !self.opts.require_enter && std::io::stdin().is_terminal();
        let back = if keys { "Esc or Enter" } else { "Enter" };
        let prompt = self
            .opts
            .theme
            .ask(&format!("number to {what}, {back} to go back:"));
        if !keys {
            return self.read_line(&prompt);
        }
        let framed = self.frame(&prompt);
        let answer = read_digits_interruptible(&self.signals, &framed.shown, &framed.after())?;
        let typed = answer.as_deref().unwrap_or_default();
        self.screen.borrow_mut().prompted(
            &format!("{}{typed}", framed.shown),
            &format!("{prompt}{typed}"),
        );
        Ok(answer.unwrap_or_default())
    }

    /// Runs a dialog, such as the values view, in the plan pane. Records it leaves, such as a
    /// value set by hand, go into the run log once it closes, however it ends.
    fn in_dialog<T>(
        &mut self,
        dialog: impl FnOnce(&mut Self) -> Result<T, Stop>,
    ) -> Result<T, Stop> {
        self.screen.borrow_mut().begin_dialog();
        let result = dialog(self);
        self.screen.borrow_mut().end_dialog();
        // At a gate or a pause, what was printed since its mark is erased when it closes,
        // so the records wait until then.
        let screen = self.screen.borrow();
        let hold = screen.dialog_open() || (screen.live() && screen.marking());
        drop(screen);
        if !hold {
            self.flush_records();
        }
        result
    }

    /// Prints the records dialogs left, such as values set by hand, into the run log.
    fn flush_records(&mut self) {
        for record in std::mem::take(&mut self.records) {
            self.say(&record);
        }
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
            scroll: false,
        };
        self.ask(&Style::Pause(action), keys, &|_| {})?;
        self.screen.borrow_mut().erase_to_mark();
        self.flush_records();
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
            open: !self.prompt_scripts.borrow().is_empty() || !self.folded.borrow().is_empty(),
            ..keys
        };
        loop {
            match self.prompt(style, keys, redraw)? {
                Key::Open => {
                    self.in_dialog(Self::open_item)?;
                    {
                        let mut screen = self.screen.borrow_mut();
                        screen.erase_to_mark();
                        screen.mark();
                    }
                    redraw(self);
                }
                Key::Values => {
                    self.in_dialog(Self::values_view)?;
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

    /// The `o` picker: the scripts of the steps to come, then the outputs folded so far,
    /// newest first. With only one, it opens at once.
    fn open_item(&mut self) -> Result<(), Stop> {
        let scripts = self.prompt_scripts.borrow().clone();
        let outputs: Vec<Folded> = self.folded.borrow().iter().rev().cloned().collect();
        // Each output's kind, from its `output` line or its stdout.
        let kinds: Vec<log::OpenKind> = outputs
            .iter()
            .map(|output| {
                let stdout = match output.output {
                    Some(_) => String::new(),
                    None => log::spool_stdout(&output.spool).unwrap_or_default(),
                };
                log::output_kind(output.output, &stdout)
            })
            .collect();
        let total = scripts.len() + outputs.len();
        let choice = if total == 1 {
            0
        } else {
            self.say("  open:");
            let width = self.width().saturating_sub(34).max(20);
            for (index, file) in scripts.iter().enumerate() {
                let name = self.shorten_text(&render::escape_controls(&file.display().to_string()));
                let kind = log::script_kind(file).name();
                self.say(&format!("  {:>4}  script  {kind:<8}  {name}", index + 1));
            }
            for (index, output) in outputs.iter().enumerate() {
                let label = pane::clip(&render::escape_controls(&output.label), width);
                self.say(&format!(
                    "  {:>4}  output  {:<8}  {}  {label}  +{} lines",
                    scripts.len() + index + 1,
                    kinds[index].name(),
                    output.step,
                    output.hidden
                ));
            }
            let answer = self.read_number("open")?;
            match answer.trim().parse::<usize>() {
                Ok(n) if (1..=total).contains(&n) => n - 1,
                _ => return Ok(()),
            }
        };
        if choice < scripts.len() {
            self.open_script(&scripts[choice]);
            return Ok(());
        }
        let index = choice - scripts.len();
        let output = &outputs[index];
        let kind = kinds[index];
        // A JSON opener gets stdout alone, so stderr cannot break the format.
        let text = if kind == log::OpenKind::Json {
            log::spool_stdout(&output.spool)
        } else {
            log::spool_text(&output.spool)
        };
        match text {
            Ok(text) => {
                let name = format!("output-{}.{}", output.step, kind.extension());
                self.page(&name, &text, kind);
                Ok(())
            }
            Err(e) => {
                self.say(&format!("  the output could not be read: {e}"));
                Ok(())
            }
        }
    }

    /// Opens a script, as the gate showed it, in its opener.
    fn open_script(&mut self, file: &Path) {
        let Some(pin) = self.pins.get(file) else {
            self.say("  the script could not be read");
            return;
        };
        let Ok(text) = std::str::from_utf8(&pin.bytes) else {
            self.say("  the script is not text, so it cannot be shown");
            return;
        };
        let shown = self.redactor.redact(text);
        // The copy keeps the script's own name, so an editor knows its language.
        let name = file
            .file_name()
            .map_or_else(|| "script".into(), |n| n.to_string_lossy().into_owned());
        self.page(&name, &shown, log::script_kind(file));
    }

    /// Shows redacted text in the opener for its kind, from a copy in the run's scratch root,
    /// or prints it when there is no opener.
    fn page(&mut self, name: &str, shown: &str, kind: log::OpenKind) {
        let name = name.replace('/', "_");
        let copy = self.scratch_root.join(format!("view-{name}"));
        let opener = self.opener_argv(kind);
        let opened = std::fs::write(&copy, shown.as_bytes()).is_ok()
            && opener.as_ref().is_some_and(|(exe, argv)| {
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
    }

    /// The program that opens an item of `kind`: the config's `open` entry for it, else the
    /// config key `pager`, else `$PAGER`, else `less`. It is split into words with no shell,
    /// and resolved on `PATH`.
    fn opener_argv(&self, kind: log::OpenKind) -> Option<(PathBuf, Vec<String>)> {
        let command = self
            .opts
            .openers
            .iter()
            .find(|(k, _)| k == kind.name())
            .map(|(_, command)| command.clone())
            .or_else(|| self.opts.pager.clone())
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
            let result = self.stage(stage);
            // A stage still running here ended early: it failed, or the operator skipped it.
            if let Some(StageState::Running(_)) = self.stage_states.get(&stage.number) {
                let state = if result.is_err() {
                    StageState::Failed
                } else {
                    StageState::Skipped
                };
                self.stage_states.insert(stage.number, state);
            }
            return result;
        };
        self.stage_states.insert(stage.number, StageState::Skipped);
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
        self.say(&format!("\n{}", self.stage_banner(stage, &description)));
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
            let answer = self.read_number("set")?;
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
        let shown = if self.p.sym(sym).secret {
            "••••".to_string()
        } else {
            self.capture_shown(&data)
        };
        self.records.push(format!("→ {name} = {shown}  ({who})"));
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
            // Each pass lists the values afresh, with what changed.
            self.screen.borrow_mut().dialog_restart();
            let rows = self.value_rows();
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
            let answer = self.read_number("change")?;
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
        let framed = self.frame(prompt);
        let after = framed.after();
        let (choice, echo) =
            read_key_interruptible(&self.signals, &framed.shown, &after, |bytes| match bytes {
                [b'\r' | b'\n', ..] | [0x1b] => Some((None, "")),
                [b, ..] => choices
                    .iter()
                    .find(|c| u32::from(**c) == u32::from(*b))
                    .map(|c| (Some(*c), choice_echo(*c))),
                [] => None,
            })?;
        self.screen.borrow_mut().prompted(
            &format!("{}{echo}", framed.shown),
            &format!("{prompt}{echo}"),
        );
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
                self.records
                    .push(format!("→ {} = (cleared)  (by hand)", info.name));
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
                let framed = self.frame(&prompt);
                let text = read_hidden_interruptible(&self.signals, &framed, theme)?;
                let done = theme.hidden_done();
                self.screen.borrow_mut().prompted(
                    &format!("{}{done}", framed.shown),
                    &format!("{prompt}{done}"),
                );
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
                scroll: self.pane_below.get().is_some(),
                ..keys
            };
            let hint = keys.hint(style, self.view.get());
            let prompt = match style {
                Style::Question(question) => self.opts.theme.question(&question(self), &hint),
                Style::Pause(action) => self.opts.theme.pause(action, &hint),
            };
            let question = matches!(style, Style::Question(_));
            let key = if self.opts.require_enter || !std::io::stdin().is_terminal() {
                // The prompt sits under the pane, which holds what it asks about.
                let line = read_line_interruptible(&self.signals, &prompt, "")?;
                self.screen
                    .borrow_mut()
                    .prompted(&format!("{prompt}{line}"), &format!("{prompt}{line}"));
                let line = line.trim();
                match (keys.key_for_line(line), question) {
                    (Some(key), _) => Some(key),
                    (None, true) if matches!(line, "y" | "Y" | "yes" | "Yes") => Some(Key::Yes),
                    (None, true) => Some(Key::No),
                    (None, false) if line.is_empty() => Some(Key::Continue),
                    (None, false) => None,
                }
            } else {
                let framed = self.frame(&prompt);
                let after = framed.after();
                let (key, echo) =
                    read_key_interruptible(&self.signals, &framed.shown, &after, |bytes| {
                        if let Some(key) = keys.key_for_bytes(bytes) {
                            return Some((key, key.echo()));
                        }
                        match (bytes, question) {
                            ([b'y' | b'Y', ..], true) => Some((Key::Yes, "y")),
                            ([b'n' | b'N' | b'\r' | b'\n', ..] | [0x1b], true) => {
                                Some((Key::No, "n"))
                            }
                            ([b'\r' | b'\n' | b' ', ..], false) => Some((Key::Continue, "")),
                            // Anything else, such as an arrow key's escape sequence, is ignored.
                            _ => None,
                        }
                    })?;
                self.screen.borrow_mut().prompted(
                    &format!("{}{echo}", framed.shown),
                    &format!("{prompt}{echo}"),
                );
                Some(key)
            };
            match key {
                Some(toggle @ (Key::ToggleView | Key::ToggleLegend | Key::Up | Key::Down)) => {
                    match toggle {
                        Key::ToggleView => self.view.set(self.view.get().toggled()),
                        Key::ToggleLegend => self.legend.set(!self.legend.get()),
                        // Half a page a key: each new prompt ignores keys for a moment, so
                        // a line a key would be slow.
                        Key::Up => {
                            let step = scroll_step();
                            self.pane_scroll
                                .set(self.pane_scroll.get().saturating_sub(step));
                        }
                        _ => {
                            let below = self.pane_below.get().unwrap_or(0);
                            let step = scroll_step().min(below);
                            self.pane_scroll.set(self.pane_scroll.get() + step);
                        }
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
        self.stage_now = stage.number;
        self.in_stage = false;
        self.touched.clear();
        self.pin_scripts(stage);
        *self.prompt_scripts.borrow_mut() = self.scripts_in(stage.steps.iter());
        let id = stage_id(stage);
        let guide = self.p.mode == Mode::Guide;
        // The gate's content sits in the plan pane, so the run log keeps only one line for it.
        // Anything a dialog prints at the gate is counted from this mark and erased with it.
        self.screen.borrow_mut().mark();
        self.pane_scroll.set(0);
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
        let opened;
        if self.opts.yes {
            if gated {
                opened = "approved by --yes";
                self.journal.event("gate_auto", json!({"stage": id}));
                self.journal.report("- gate approved by `--yes`");
            } else {
                opened = "started";
                self.journal
                    .event("pause", json!({"stage": id, "auto": true}));
                self.journal.report("- continued by `--yes`");
            }
        } else {
            let preview = |r: &Self| r.show_prompt_pane(r.gate_lines(stage));
            preview(self);
            let keys = Keys {
                view: true,
                legend: true,
                values: true,
                skip: stage.skip != SkipRule::Never,
                open: false,
                scroll: false,
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
                        if self.in_dialog(|r| r.operator_skip(stage))? {
                            self.close_gate();
                            self.say(&format!("\n{}", self.stage_banner(stage, "skipped")));
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
                        opened = "approved";
                        break;
                    }
                    Key::Continue => {
                        self.journal.event("pause", json!({"stage": id}));
                        self.journal.report("- continued");
                        opened = "continued";
                        break;
                    }
                    _ => {
                        self.journal
                            .event("gate", json!({"stage": id, "approved": false}));
                        self.close_gate();
                        self.say(&format!("\n{}", self.stage_banner(stage, "not approved")));
                        return Err(Stop::failed(format!(
                            "{} was not approved; nothing in it ran",
                            stage.name()
                        )));
                    }
                }
            }
        }
        self.close_gate();
        self.say(&format!("\n{}", self.stage_banner(stage, opened)));
        self.indented.set(true);
        self.prompt_scripts.borrow_mut().clear();
        self.in_stage = true;
        let prefix = if stage.is_preflight() {
            "p".to_string()
        } else {
            format!("s{}", stage.number)
        };
        let before = self.seq;
        let started = Instant::now();
        self.stage_states
            .insert(stage.number, StageState::Running(started));
        self.current_step = Some(String::new());
        let result = self.stage_steps(stage, &prefix);
        self.current_step = None;
        // A failure prints flush, under the stage, as the run's own ending.
        if result.as_ref().is_err_and(|stop| !stop.skip_rest) {
            self.indented.set(false);
        }
        self.screen.borrow_mut().clear_all();
        // A stage whose rest was skipped still ended well.
        let ended_well = match &result {
            Ok(()) => true,
            Err(stop) => stop.skip_rest,
        };
        if ended_well {
            self.stage_states
                .insert(stage.number, StageState::Done(started.elapsed()));
        }
        if let Err(stop) = &result
            && stop.skip_rest
        {
            self.say(&format!("↷ the rest of {} was skipped", stage.name()));
            self.indented.set(false);
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
        let took = elapsed(started.elapsed());
        self.say(&format!("✓ {done} done{tail} · {took}"));
        self.indented.set(false);
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
        self.steps_from(steps, 0, prefix, suffix, deadline)
            .map_err(|(_, stop)| stop)
    }

    /// Runs `steps` from the one at `start`. A stop comes back with the index of the step
    /// that stopped, so a `retry` stage can run again from it.
    fn steps_from(
        &mut self,
        steps: &'p [Step],
        start: usize,
        prefix: &str,
        suffix: &str,
        deadline: Option<Instant>,
    ) -> Result<(), (usize, Stop)> {
        for (index, step) in steps.iter().enumerate().skip(start) {
            let path = format!("{prefix}.{}", index + 1);
            let rest = &steps[index + 1..];
            self.rests.push(rest);
            let result = self.step(step, rest, &path, suffix, deadline);
            self.rests.pop();
            result.map_err(|stop| (index, stop))?;
        }
        Ok(())
    }

    /// Runs a stage's steps. When a step in a `retry` stage fails, and an operator is there
    /// to answer, the stage asks whether to run again from that step, from its first step,
    /// or to stop. A step inside `when`, `each` or `poll` runs again from the top-level step
    /// that holds it.
    fn stage_steps(&mut self, stage: &'p ir::Stage, prefix: &str) -> Result<(), Stop> {
        let mut start = 0;
        let mut attempt = 1;
        loop {
            let (index, stop) = match self.steps_from(&stage.steps, start, prefix, "", None) {
                Ok(()) => return Ok(()),
                Err(failed) => failed,
            };
            let retryable = stage.retry
                && self.tty
                && !self.opts.yes
                && stop.ending == Ending::Failed
                && !stop.skip_rest;
            if !retryable {
                return Err(stop);
            }
            let failed = stage.steps[index].id.clone();
            let Some(from_failed) = self.ask_retry(stage, &failed, &stop)? else {
                return Err(stop);
            };
            attempt += 1;
            start = if from_failed { index } else { 0 };
            let from = stage.steps[start].id.clone();
            *self.retries.entry(stage.number).or_default() += 1;
            self.journal.event(
                "stage_retry",
                json!({"stage": stage_id(stage), "attempt": attempt, "from": from, "failed": failed}),
            );
            self.journal
                .report(&format!("- failed at {failed}; retried from {from}"));
            self.say(&format!(
                "↻ retry {attempt} of {}, from {from}",
                stage.name()
            ));
            self.update_pane();
        }
    }

    /// Asks, in the pane, whether to retry a failed stage: `Some(true)` from the step that
    /// failed, `Some(false)` from the first step, `None` to stop.
    fn ask_retry(
        &mut self,
        stage: &ir::Stage,
        failed: &str,
        stop: &Stop,
    ) -> Result<Option<bool>, Stop> {
        let theme = self.opts.theme;
        self.screen.borrow_mut().mark();
        let reason = self.redactor.redact(&stop.message);
        self.show_prompt_pane(vec![
            theme.lines(&format!("✗ {} failed at {failed}: {reason}", stage.name())),
        ]);
        let prompt = theme.ask(&format!("r retry from {failed} · R retry stage · q quit"));
        let choice = self.read_choice(&prompt, &['r', 'R', 'q']);
        self.close_gate();
        Ok(match choice? {
            Some('r') => Some(true),
            Some('R') => Some(false),
            _ => None,
        })
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
        if let Some(current) = &mut self.current_step {
            current.clone_from(&step.id);
        }
        // A `confirm` sets the scripts `o` can open; they belong to its prompt only.
        self.prompt_scripts.borrow_mut().clear();
        self.update_pane();
        match &step.kind {
            StepKind::Command(command) => self.command(command, step, path, suffix, deadline),
            StepKind::Confirm(text) => {
                let text = self.display_interpolated(text);
                // Everything left in the stage, shown in full in the pane as at a gate. Once
                // answered, the run log keeps one line and the plan pane takes over again.
                let _ = rest;
                let steps = self.remaining();
                self.screen.borrow_mut().clear_all();
                self.screen.borrow_mut().mark();
                self.pane_scroll.set(0);
                let redraw = |r: &Self| {
                    let mut lines = Vec::new();
                    if !steps.is_empty() {
                        lines.push(r.opts.theme.lines("  next:"));
                        lines.extend(r.preview_lines(&steps));
                    }
                    r.show_prompt_pane(lines);
                };
                redraw(self);
                let keys = Keys {
                    view: true,
                    legend: true,
                    values: true,
                    skip: self.p.stage(self.stage_now).skip != SkipRule::Never,
                    open: false,
                    scroll: false,
                };
                let question = |r: &Self| r.shorten_text(&text);
                *self.prompt_scripts.borrow_mut() = self.scripts_in(steps.iter().copied());
                loop {
                    match self.ask(&Style::Question(&question), keys, &redraw)? {
                        Key::Yes => {
                            self.close_gate();
                            self.say(&format!("✓ confirmed: {text}"));
                            self.update_pane();
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
                            let skipped = self.in_dialog(|r| {
                                r.skip_dialog(&decided, &partial, now, &what, false)
                            })?;
                            if skipped {
                                self.close_gate();
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
                            self.close_gate();
                            self.say(&format!("✗ not confirmed: {text}"));
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
                    let secret = *secret;
                    self.in_dialog(|r| {
                        r.say(&format!("  {}", r.shorten_text(&prompt)));
                        let value = r.prompt_value(&name, secret)?;
                        r.records
                            .push(answer_record(&name, &value, secret, "asked"));
                        Ok(value)
                    })?
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
                // A message from the wizard's author: a bar sets it apart from command output,
                // and a mark says its kind. Every line of it keeps the bar.
                let mark = match kind {
                    SayKind::Note => "▌ ",
                    SayKind::Warn => "▌⚠ ",
                    SayKind::Record => "▌✓ ",
                };
                let shown = text.replace('\n', &format!("\n{mark}"));
                self.log_line(&format!("{mark}{shown}"));
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
    #[allow(clippy::too_many_lines)] // one exit path each: match, fail, error, deadline, stop
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
        let started = Instant::now();
        let mut deadline = started + Duration::from_secs(limit);
        if let Some(outer) = outer {
            deadline = deadline.min(outer);
        }
        // A live view holds each try until it knows its fate; otherwise tries print as they run.
        let live = self.screen.borrow().live();
        self.log_line(&format!(
            "↻ poll every {every}s, up to {}: until {}",
            pane::short_duration(Duration::from_secs(limit)),
            render::expr(self.p, until, self)
        ));
        self.polls.borrow_mut().push(PollTrack {
            started,
            deadline,
            attempt: 0,
            try_started: None,
            wake: None,
            last: None,
        });
        let mut attempts = Vec::new();
        // The last try that did not match, kept to show in full if the poll stops waiting.
        let mut last_try: Vec<Block> = Vec::new();
        let result = 'poll: {
            for attempt in 1.. {
                let try_started = Instant::now();
                if let Some(track) = self.polls.borrow_mut().last_mut() {
                    track.attempt = attempt;
                    track.try_started = Some(try_started);
                    track.wake = None;
                }
                self.update_pane();
                if live {
                    self.scopes.hold();
                } else {
                    let at = pane::short_duration(started.elapsed());
                    self.log_line(&format!("↻ try {attempt} · {at}"));
                }
                let before = self.seq;
                let ran = self.steps(body, path, &format!("@{attempt}"), Some(deadline));
                let invocations: Vec<String> =
                    (before + 1..=self.seq).map(|n| n.to_string()).collect();
                attempts.extend(invocations.iter().cloned());
                let result = self.poll_result(body);
                if let Some(track) = self.polls.borrow_mut().last_mut() {
                    track.last.clone_from(&result);
                }
                // The try's fate: matched, not yet, or stopped with the reason.
                let fate: Result<bool, Stop> = match ran {
                    Err(stop) if stop.poll_deadline => {
                        Err(self.poll_timeout(step, limit, until, body, attempt, &attempts))
                    }
                    Err(stop) => Err(stop),
                    Ok(()) => match fail.map(|f| self.eval_bool(f)).transpose() {
                        Err(stop) => Err(stop),
                        Ok(Some(true)) => Err(Stop::failed(format!(
                            "poll at step {} hit its failure condition",
                            step.id
                        ))
                        .detail(format!(
                            "fail {}",
                            render::expr(self.p, fail.unwrap_or(until), self)
                        ))),
                        Ok(_) => self.eval_bool(until),
                    },
                };
                let outcome = match &fate {
                    Ok(true) => "matched",
                    Ok(false) => "waiting",
                    Err(_) => "stopped",
                };
                self.journal.event(
                    "poll_try",
                    json!({
                        "step": step.id,
                        "try": attempt,
                        "elapsed_ms": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                        "result": outcome,
                        "invocations": invocations,
                    }),
                );
                match fate {
                    Ok(true) => {
                        // The matching try lands under the `↻` lines, folded as usual.
                        if live {
                            for block in self.scopes.take() {
                                self.emit(block, false);
                            }
                        }
                        let took = pane::short_duration(started.elapsed());
                        let tries = if attempt == 1 { "try" } else { "tries" };
                        self.log_line(&format!("↻ matched after {attempt} {tries} · {took}"));
                        break 'poll Ok(());
                    }
                    Ok(false) => {
                        if live {
                            last_try = self.scopes.take();
                            let took = pane::short_duration(started.elapsed());
                            let shown = result.map_or_else(String::new, |r| format!("  {r}"));
                            self.log_line(&format!("↻ try {attempt} · {took}{shown}"));
                        }
                    }
                    Err(stop) => {
                        // The evidence first: the try that stopped, in full.
                        if live {
                            for block in self.scopes.take() {
                                self.emit(block, true);
                            }
                        }
                        self.log_line(&format!("✗ try {attempt} stopped: {}", stop.message));
                        break 'poll Err(stop);
                    }
                }
                let wake = Instant::now() + Duration::from_secs(every);
                let stopped = if wake >= deadline {
                    Err(self.poll_timeout(step, limit, until, body, attempt, &attempts))
                } else {
                    if let Some(track) = self.polls.borrow_mut().last_mut() {
                        track.try_started = None;
                        track.wake = Some(wake);
                    }
                    self.sleep_until(wake)
                };
                if let Err(stop) = stopped {
                    // Stopped while waiting: the last try, which only left a `↻` line, shows
                    // in full.
                    if live && !last_try.is_empty() {
                        self.log_line(&format!("↻ last try (try {attempt}), in full:"));
                        for block in std::mem::take(&mut last_try) {
                            self.emit(block, true);
                        }
                    }
                    self.log_line(&format!("✗ poll stopped: {}", stop.message));
                    break 'poll Err(stop);
                }
            }
            Ok(())
        };
        self.polls.borrow_mut().pop();
        result
    }

    /// A try's short result for its `↻` line: the captures it made, such as
    /// `status = "creating"`.
    fn poll_result(&self, body: &[Step]) -> Option<String> {
        let parts: Vec<String> = body_captures(body)
            .into_iter()
            .map(|sym| {
                let info = self.p.sym(sym);
                let shown = match self.values.get(&sym) {
                    None => "absent".to_string(),
                    Some(_) if info.secret => format!("«secret:{}»", info.name),
                    Some(value) if value.withheld.is_some() => format!("«withheld:{}»", info.name),
                    Some(value) => self.capture_shown(&value.data),
                };
                format!("{} = {shown}", info.name)
            })
            .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    /// Hands a block on: to the enclosing poll try that holds it, or to the run log. `full`
    /// marks a command to show in full wherever it lands.
    fn emit(&mut self, mut block: Block, full: bool) {
        if let Block::Command(command) = &mut block {
            command.full |= full;
        }
        if let Some(block) = self.scopes.emit(block) {
            self.render(block, false);
        }
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
        let mut drawn = Instant::now();
        while Instant::now() < wake {
            // The pane's countdown moves while the poll waits.
            if drawn.elapsed() >= Duration::from_millis(250) {
                self.update_pane();
                drawn = Instant::now();
            }
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
        let result = self.command_in(command, step, &invocation, &scratch, deadline);
        self.finish_command(result.is_ok());
        result
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
        self.say_command(command, &step.id, scratch, invocation)?;
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
                match read_line_interruptible(signals, &suspended, "") {
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
            // The terminal is korin's again: the pane comes back under what the program left.
            self.update_pane();
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
            let screen = &self.screen;
            let live = screen.borrow().live();
            let theme = self.opts.theme;
            // A live view feeds the command's block: whole lines go to its spool, the first
            // few are kept for a fold, and the newest show in the live window.
            let mut spool = if live && stream {
                self.spool_for(invocation)
            } else {
                None
            };
            let head: Vec<String> = self
                .building
                .as_ref()
                .map(|b| {
                    b.head
                        .trim_start_matches('\n')
                        .split('\n')
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if live && !stream {
                let mut lines = head.clone();
                lines.push(theme.faint("  output held until its secret captures resolve"));
                screen.borrow_mut().set_window(lines, true);
            }
            let cols = self.width().saturating_sub(3).max(20);
            let pad = self.pad();
            let mut first: Vec<String> = Vec::new();
            let mut count = 0;
            let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::new();
            let mut partial = [String::new(), String::new()];
            let mut feed = |is_err: bool, whole: &str, now: bool| {
                let kind = if is_err {
                    log::Stream::Err
                } else {
                    log::Stream::Out
                };
                if let Some(spool) = &mut spool {
                    spool.push(kind, whole);
                }
                for line in whole.lines() {
                    count += 1;
                    if first.len() < log::HEAD.max(log::FOLD_AT) {
                        first.push(line.to_string());
                    }
                    if tail.len() == log::WINDOW {
                        tail.pop_front();
                    }
                    tail.push_back(format!("{pad}{}", theme.output(&pane::clip(line, cols))));
                }
                let mut lines = head.clone();
                lines.extend(tail.iter().cloned());
                screen.borrow_mut().set_window(lines, now);
            };
            let mut on_output = |is_err: bool, chunk: &[u8]| {
                if stream {
                    let text = buffers[usize::from(is_err)].push(&redactor, chunk);
                    if live {
                        let buffer = &mut partial[usize::from(is_err)];
                        buffer.push_str(&text);
                        if let Some(end) = buffer.rfind('\n') {
                            let whole: String = buffer.drain(..=end).collect();
                            feed(is_err, &whole, false);
                        }
                    } else {
                        // Whole lines only, per stream, so stdout and stderr never split a
                        // line between them. korin's own marks are removed.
                        let buffer = &mut partial[usize::from(is_err)];
                        buffer.push_str(&text);
                        if let Some(end) = buffer.rfind('\n') {
                            let whole: String = buffer.drain(..=end).collect();
                            screen
                                .borrow_mut()
                                .print(&indent(&theme.output(&whole), pad));
                        }
                    }
                }
            };
            // While a poll runs a try, its pane line counts the try's time.
            let this: &Self = self;
            let mut pane_drawn = Instant::now();
            let mut on_tick = || {
                if live
                    && !this.polls.borrow().is_empty()
                    && pane_drawn.elapsed() >= Duration::from_millis(500)
                {
                    this.update_pane();
                    pane_drawn = Instant::now();
                }
                screen.borrow_mut().flush_window();
            };
            let finished = process::execute(&spec, &self.signals, &mut on_output, &mut on_tick)
                .map_err(|e| Stop::failed(format!("cannot start `{}`: {e}", argv[0])))?;
            if stream {
                if live {
                    for (index, buffer) in buffers.iter_mut().enumerate() {
                        let rest = &mut partial[index];
                        rest.push_str(&buffer.finish(&redactor));
                        if !rest.is_empty() {
                            if !rest.ends_with('\n') {
                                rest.push('\n');
                            }
                            let whole = std::mem::take(rest);
                            feed(index == 1, &whole, true);
                        }
                    }
                } else {
                    let mut screen = self.screen.borrow_mut();
                    for (index, buffer) in buffers.iter_mut().enumerate() {
                        let mut rest = std::mem::take(&mut partial[index]);
                        rest.push_str(&buffer.finish(&redactor));
                        if !rest.is_empty() {
                            screen.print(&indent(&theme.output(&rest), pad));
                        }
                    }
                }
            }
            if let Some(block) = &mut self.building {
                block.first = first;
                block.lines = count;
                block.spool = spool.map(|mut spool| {
                    spool.close();
                    spool.path().to_path_buf()
                });
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
            self.last_exit = Some(if cancel == Cancel::Timeout {
                log::Exit::TimedOut
            } else {
                log::Exit::Cancelled
            });
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
                    Match::Stdout(re) => regex::Regex::new(&self.regex_pattern(re)?)
                        .is_ok_and(|r| r.is_match(&stdout_text)),
                    Match::Stderr(re) => regex::Regex::new(&self.regex_pattern(re)?)
                        .is_ok_and(|r| r.is_match(&stderr_text)),
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
        self.last_exit = Some(match status {
            Status::Exited(0) => log::Exit::Ok,
            Status::Exited(code) if matched.is_some() => log::Exit::Expected(code),
            Status::Exited(code) => log::Exit::Failed(code),
            Status::Signaled(signal) => log::Exit::Signal(crate::signals::name(signal).into()),
        });

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
                let mode = match &capture.regex {
                    Some(pattern) => crate::ast::CaptureMode::Regex(self.regex_pattern(pattern)?),
                    None => capture.mode.clone(),
                };
                let extracted = match self.jq_values(capture, scratch) {
                    Ok(vars) => extract_with(&mode, &input, capture.optional, &vars),
                    Err(message) => Extract::Fail(message),
                };
                results.push((capture, extracted, file_bytes));
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
            let out = self.redactor.redact_bytes(&stdout);
            let err = self.redactor.redact_bytes(&stderr);
            if self.building.is_some() {
                // Its output joins the block only now that every secret in it is known:
                // stdout, then stderr, as they were collected apart.
                let mut spool = self.spool_for(invocation);
                let mut first = Vec::new();
                let mut count = 0;
                for (kind, text) in [(log::Stream::Out, &out), (log::Stream::Err, &err)] {
                    if text.is_empty() {
                        continue;
                    }
                    let whole = if text.ends_with('\n') {
                        text.clone()
                    } else {
                        format!("{text}\n")
                    };
                    if let Some(spool) = &mut spool {
                        spool.push(kind, &whole);
                    }
                    for line in whole.lines() {
                        count += 1;
                        if first.len() < log::FOLD_AT {
                            first.push(line.to_string());
                        }
                    }
                }
                if let Some(block) = &mut self.building {
                    block.first = first;
                    block.lines = count;
                    block.spool = spool.map(|mut spool| {
                        spool.close();
                        spool.path().to_path_buf()
                    });
                }
            } else {
                let theme = self.opts.theme;
                let mut screen = self.screen.borrow_mut();
                let pad = self.pad();
                screen.print(&indent(&theme.output(&out), pad));
                screen.print(&indent(&theme.output(&err), pad));
            }
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
        let status_note = (!status_note.is_empty()).then(|| {
            let tail = if succeeded { " · continuing" } else { "" };
            format!("{}{tail}", status_note.join(" · "))
        });

        // Store values and log captures, all on one `→` line.
        let mut captured: Vec<String> = Vec::new();
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
                        captured.push(format!("{name} = {shown}"));
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
                    captured.push(format!("{name} = (absent)"));
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
        if !captured.is_empty() {
            self.note_line(&format!("  → {}", captured.join("  ·  ")));
        }
        if let Some(status) = status_note {
            self.note(&status);
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

    /// The values a `jq` filter uses as `$name`, each as JSON text: a string as a string, a
    /// list as an array, a `jq json` capture as itself, an expect as a boolean, and a value
    /// that is absent as `null`. A withheld value fails the capture.
    fn jq_values(
        &self,
        capture: &ir::Capture,
        scratch: &Path,
    ) -> Result<Vec<(String, String)>, String> {
        let mut out = Vec::new();
        for (name, template) in &capture.jq_vars {
            let json = match template.parts.as_slice() {
                [IPart::Sym { sym, .. }] if *sym != self.p.korin_out => {
                    match self.values.get(sym) {
                        None => "null".to_string(),
                        Some(value) => {
                            if let Some(reason) = &value.withheld {
                                return Err(format!(
                                    "`{name}` is withheld ({reason}), so the filter cannot use it"
                                ));
                            }
                            match &value.data {
                                // A `jq json` capture holds JSON text.
                                Data::Str(text)
                                    if self.p.sym(*sym).ty == Ty::Json
                                        && serde_json::from_str::<serde_json::Value>(text)
                                            .is_ok() =>
                                {
                                    text.clone()
                                }
                                data => data.to_json().to_string(),
                            }
                        }
                    }
                }
                _ => {
                    let text = self
                        .exec_text(template, scratch)
                        .map_err(|stop| stop.message)?;
                    serde_json::Value::String(text).to_string()
                }
            };
            out.push((name.clone(), json));
        }
        Ok(out)
    }

    /// A regex's pattern, with each `{name}` part's value escaped, so it matches as literal
    /// text.
    fn regex_pattern(&self, t: &Interpolated) -> Result<String, Stop> {
        let mut out = String::new();
        for part in &t.parts {
            match part {
                IPart::Lit(text) => out.push_str(text),
                IPart::Sym { sym, .. } => {
                    out.push_str(&regex::escape(&data_text(&self.value(*sym)?.data)));
                }
            }
        }
        Ok(out)
    }

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
    fn say_command(
        &mut self,
        command: &Command,
        step: &str,
        scratch: &Path,
        invocation: &str,
    ) -> Result<(), Stop> {
        let (env, words) = self.shell_words(command, scratch)?;
        let live = self.screen.borrow().live();
        let width = live.then(|| self.width());
        let scratch_text = scratch.display().to_string();
        let line = render::shell_line(&env, &words, &self.redactor, Some(&scratch_text), width);
        let theme = self.opts.theme;
        // A safety net: the line names secrets as variables, so redaction changes nothing.
        let mut head = vec![theme.lines(&self.redact_marked(&line.lines.join("\n")))];
        for found in &line.vars {
            let Some(var) = &found.var else { continue };
            if self.announced_vars.insert(var.clone()) {
                self.journal.event(
                    "secret_var",
                    json!({"name": found.name, "var": var, "value": found.label}),
                );
                head.push(theme.lines(&format!("  ↳ uses ${{{var}}} ({})", found.label)));
            }
        }
        if !line.partial.is_empty() {
            head.push(theme.lines(&format!(
                "  ↳ not pasteable: holds part of {}",
                line.partial.join(", ")
            )));
        }
        let pad = self.pad();
        let head = head
            .iter()
            .flat_map(|h| h.split('\n'))
            .map(|line| format!("{pad}{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        // A live view builds the command's block while it runs, and shows it in the live
        // window. An interactive program takes the whole terminal, so its line prints now.
        if live && command.kind != CommandKind::Interactive {
            let label = render::strip_marks(&render::join_breaks(&line.lines.join("\n")));
            self.building = Some(CommandBlock {
                head: head.clone(),
                shell_rows: line.lines.len(),
                output: command.output,
                journal: format!("{}/out/{invocation}.stdout", self.journal_rel()),
                step: step.to_string(),
                label,
                ..CommandBlock::default()
            });
            let lines = head.split('\n').map(str::to_string).collect();
            self.screen.borrow_mut().set_window(lines, true);
        } else {
            self.screen.borrow_mut().print(&format!("{head}\n"));
        }
        Ok(())
    }

    /// The journal's directory for the run log: relative to the cwd when it is under it,
    /// in every view, so a path in a note can be pasted.
    fn journal_rel(&self) -> String {
        let dir = self.journal.dir();
        match dir.strip_prefix(&self.opts.cwd) {
            Ok(rest) => format!("./{}", rest.display()),
            Err(_) => dir.display().to_string(),
        }
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

    /// Adds a `↳` note under the command running now, or prints it.
    fn note(&mut self, text: &str) {
        self.note_line(&format!("  ↳ {text}"));
    }

    /// Adds a line under the command running now, or prints it.
    fn note_line(&mut self, line: &str) {
        let line = line.to_string();
        match &mut self.building {
            Some(block) => block.notes.push(line),
            None => self.say(&line),
        }
    }

    /// The command block that ends now: the live window goes, and the block goes to the run
    /// log, or to the poll try that holds it.
    fn finish_command(&mut self, ok: bool) {
        let exit = self.last_exit.take();
        let Some(mut block) = self.building.take() else {
            // The `$` line printed before the command ran, so the tag takes a line of its own.
            // Exit 0 needs none.
            if let Some(exit) = exit.filter(|exit| *exit != log::Exit::Ok) {
                let theme = self.opts.theme;
                let tag = theme.exit_tag(exit.tone(), &exit.tag());
                let pad = self.pad();
                self.screen.borrow_mut().print(&format!("{pad}{tag}\n"));
            }
            return;
        };
        block.ok = ok;
        block.exit = exit;
        self.screen.borrow_mut().set_window(Vec::new(), true);
        self.emit(Block::Command(block), false);
    }

    /// Prints a block into the run log. `full` shows every command's output, as for a unit
    /// that failed or was cancelled.
    fn render(&mut self, block: Block, full: bool) {
        match block {
            Block::Lines(line) => self.say(&line),
            Block::Command(block) => {
                let full = full || block.full;
                self.render_command(&block, full);
            }
        }
    }

    fn render_command(&mut self, block: &CommandBlock, full: bool) {
        let theme = self.opts.theme;
        let pad = self.pad();
        let mode = log::mode(block.ok, full, block.lines);
        // Exit 0 needs no tag: the green `$` says it. Any other ending gets one.
        let tag = block
            .exit
            .as_ref()
            .filter(|exit| **exit != log::Exit::Ok)
            .map(|exit| theme.exit_tag(exit.tone(), &exit.tag()));
        // The `$` takes the color of how the command ended.
        let mut head = match &block.exit {
            Some(exit) => block.head.replacen(
                &theme.shell_mark(None),
                &theme.shell_mark(Some(exit.tone())),
                1,
            ),
            None => block.head.clone(),
        };
        if block.lines == 0
            && let Some(tag) = &tag
        {
            // No output: the tag ends the `$` line.
            let mut rows: Vec<String> = head.split('\n').map(str::to_string).collect();
            let at = block.shell_rows.clamp(1, rows.len()) - 1;
            rows[at] = format!("{}  {tag}", rows[at]);
            head = rows.join("\n");
        }
        self.screen.borrow_mut().print(&format!("{head}\n"));
        // Each output line goes out with a newline before the next, so the last can take
        // the tag.
        let end_line = |text: &mut String| text.push('\n');
        let mut text = String::new();
        let mut shown = 0;
        if let (log::Mode::Full, Some(spool)) = (mode, &block.spool) {
            let mut screen = self.screen.borrow_mut();
            let read = log::read_spool(spool, |_, line| {
                if shown > 0 {
                    end_line(&mut text);
                }
                // Print in batches, so a long output does not redraw the pane per line.
                if shown > 0 && shown % 200 == 0 {
                    screen.print(&std::mem::take(&mut text));
                }
                text.push_str(pad);
                text.push_str(&theme.output(line));
                shown += 1;
            });
            drop(screen);
            if read.is_err() {
                if shown > 0 {
                    end_line(&mut text);
                }
                self.screen.borrow_mut().print(&std::mem::take(&mut text));
                self.say(&format!("  ↳ the full output is in {}", block.journal));
            }
        } else {
            let take = if mode == log::Mode::Folded {
                log::HEAD
            } else {
                block.first.len()
            };
            for line in block.first.iter().take(take) {
                if shown > 0 {
                    end_line(&mut text);
                }
                text.push_str(pad);
                text.push_str(&theme.output(line));
                shown += 1;
            }
        }
        if !text.is_empty() {
            // A folded output's tag goes in its note instead.
            if mode != log::Mode::Folded
                && let Some(tag) = &tag
            {
                text.push_str("  ");
                text.push_str(tag);
            }
            end_line(&mut text);
            self.screen.borrow_mut().print(&text);
        } else if block.lines > 0
            && mode != log::Mode::Folded
            && let Some(tag) = &tag
        {
            // The output had lines, but none could show: the tag takes its own line.
            self.screen.borrow_mut().print(&format!("{pad}{tag}\n"));
        }
        if mode == log::Mode::Folded
            && let Some(spool) = &block.spool
        {
            self.folded.borrow_mut().push(Folded {
                step: block.step.clone(),
                label: block.label.clone(),
                hidden: block.lines - shown,
                spool: spool.clone(),
                output: block.output,
            });
        }
        if shown < block.lines {
            let status = match (&block.exit, mode) {
                (Some(exit), log::Mode::Folded) if *exit != log::Exit::Ok => {
                    format!("{} · ", exit.tag())
                }
                _ => String::new(),
            };
            self.say(&format!(
                "  ↳ {status}+{} lines · {}",
                block.lines - shown,
                block.journal
            ));
        }
        for note in &block.notes {
            self.say(note);
        }
    }

    /// The spool file for an invocation, in the run's own scratch root, so it lives until
    /// the run ends. The step's scratch directory is gone once the step ends.
    fn spool_for(&self, invocation: &str) -> Option<log::Spool> {
        let dir = self.scratch_root.join("spool");
        if !dir.is_dir() {
            create_private_dir(&dir).ok()?;
        }
        let name = invocation.replace(['/', '[', ']', '@'], "_");
        log::Spool::create(dir.join(format!("{name}.out"))).ok()
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
                let re = regex::Regex::new(&self.regex_pattern(re)?)
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

/// The `#` line an answer leaves in the run log: `# region = us-east-1  (asked)`. A secret
/// shows as `••••`, and a value is escaped and kept to one line.
fn answer_record(name: &str, value: &str, secret: bool, how: &str) -> String {
    let shown = if secret {
        "••••".to_string()
    } else {
        render::escape_controls(value)
    };
    format!("→ {name} = {shown}  ({how})")
}

/// How many lines one scroll key moves a prompt's content: half the terminal.
fn scroll_step() -> usize {
    (screen::height() / 2).max(1)
}

/// How a command shows the step's scratch directory before it runs.
const KORIN_OUT_LABEL: &str = "<korin.out>";

/// The labels a legend can explain that a line shows: each shortened path prefix, such as
/// `<pwd>`, and `<korin.out>`, in the order they appear.
fn legend_labels(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(render::PREFIX_OPEN) {
        let after = &rest[start + render::PREFIX_OPEN.len_utf8()..];
        let Some(end) = after.find(render::PREFIX_CLOSE) else {
            break;
        };
        let label = after[..end].to_string();
        if !out.contains(&label) {
            out.push(label);
        }
        rest = &after[end + render::PREFIX_CLOSE.len_utf8()..];
    }
    if text.contains(KORIN_OUT_LABEL) {
        out.push(KORIN_OUT_LABEL.to_string());
    }
    out
}

/// The step id a rendered step line starts with, such as `2.1` in `    2.1  read ❯ ls`.
fn line_id(line: &str) -> Option<&str> {
    let body = line.trim_start_matches(' ');
    let id = body.split(' ').next()?;
    let is_id = !id.is_empty()
        && id.chars().all(|c| c.is_ascii_digit() || c == '.')
        && body[id.len()..].starts_with("  ");
    is_id.then_some(id)
}

/// The step with this id, inside blocks and templates too.
fn find_step<'s>(steps: &'s [Step], id: &str) -> Option<&'s Step> {
    for step in steps {
        if step.id == id {
            return Some(step);
        }
        let inner = match &step.kind {
            StepKind::When { body, .. }
            | StepKind::Each { body, .. }
            | StepKind::Poll { body, .. }
            | StepKind::Call { body, .. } => find_step(body, id),
            _ => None,
        };
        if inner.is_some() {
            return inner;
        }
    }
    None
}

/// A duration as the pane shows it: `3s`, `3m40s`, `1h02m`.
fn elapsed(took: Duration) -> String {
    let secs = took.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
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
/// Reads digits one key at a time, echoing them. Backspace erases one, Enter gives the
/// digits, and Esc gives `None` at once. Other keys, such as an arrow's escape sequence,
/// are ignored. Ctrl-C and `SIGTERM` cancel the run.
fn read_digits_interruptible(
    signals: &Signals,
    prompt: &str,
    after: &str,
) -> Result<Option<String>, Stop> {
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
    print!("{prompt}{after}");
    let _ = std::io::stdout().flush();
    let mut digits = String::new();
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
        let bytes = &buf[..read];
        match bytes {
            // A lone Esc goes back.
            [0x1b] => {
                println!();
                return Ok(None);
            }
            // An arrow or another key that sends Esc and more in one read.
            [0x1b, ..] => continue,
            _ => {}
        }
        // Keys typed fast can arrive in one read, such as `2` and Enter.
        for byte in bytes {
            match byte {
                b'\r' | b'\n' => {
                    println!();
                    return Ok(Some(digits));
                }
                0x7f | 0x08 => {
                    if digits.pop().is_some() {
                        print!("\x08 \x08");
                    }
                }
                b'0'..=b'9' => {
                    digits.push(char::from(*byte));
                    print!("{}", char::from(*byte));
                }
                _ => {}
            }
        }
        let _ = std::io::stdout().flush();
    }
}

/// A prompt as it prints: in a live view, inside a box. `shown` is the prompt line with the
/// box's left side. `right` draws the right side without moving the cursor. `bottom` draws
/// the bottom border on the next row, then puts the cursor back after the prompt.
#[derive(Debug, Default)]
struct Framed {
    shown: String,
    right: String,
    bottom: String,
}

impl Framed {
    fn plain(prompt: &str) -> Self {
        Self {
            shown: prompt.to_string(),
            ..Self::default()
        }
    }

    /// What prints once, after the prompt line: the rest of the box.
    fn after(&self) -> String {
        format!("{}{}", self.right, self.bottom)
    }
}

fn read_line_interruptible(signals: &Signals, prompt: &str, after: &str) -> Result<String, Stop> {
    // Discard anything typed before the prompt appeared, so type-ahead never answers a gate
    // the human has not read.
    if std::io::stdin().is_terminal() {
        let _ = nix::sys::termios::tcflush(std::io::stdin(), nix::sys::termios::FlushArg::TCIFLUSH);
    }
    print!("{prompt}{after}");
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
    prompt: &Framed,
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
        // Clearing the line takes the box's right side with it, so it is drawn again.
        print!(
            "\r{}{}\x1b[K{}",
            prompt.shown,
            theme.hidden_count(count),
            prompt.right
        );
        let _ = std::io::stdout().flush();
    };
    print!("{}{}", prompt.shown, prompt.after());
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
                    print!(
                        "\r{}{}\x1b[K{}\r\n",
                        prompt.shown,
                        theme.hidden_done(),
                        prompt.right
                    );
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
    after: &str,
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
    print!("{prompt}{after}");
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
