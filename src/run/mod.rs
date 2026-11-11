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
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use serde_json::json;
use unicode_width::UnicodeWidthStr as _;

use crate::ast::{CmpOp, CommandKind, GateMode, SayKind, Span};
use crate::diag::Diag;
use crate::ir::{
    self, Command, Expr, IPart, Interpolated, Match, Program, Step, StepKind, Sym, SymKind,
};
use crate::redact::{Redactor, StreamRedactor};
use crate::render::{self, Values as _};
use crate::theme::Theme;
use crate::view::{Prefixes, View};
use capture::{Data, Extract, Fingerprint, extract};
use journal::Journal;
use process::{Cancel, Signals, Spec, Status, Suspended};
use zeroize::Zeroizing;

/// The default timeout for `read`, `mutate` and `script` steps.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_mins(15);

/// Resolves `--from` to a 1-based stage number and checks that the run can start there.
///
/// # Errors
///
/// Returns a diagnostic if no stage matches, or if a later step uses a value produced in a
/// skipped stage.
pub fn resolve_from(program: &Program, from: &str) -> Result<usize, Box<Diag>> {
    let stage = from
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=program.stages.len()).contains(n))
        .or_else(|| {
            program
                .stages
                .iter()
                .find(|s| s.title == from)
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
                    "from",
                    Span::default(),
                    format!("no stage matches `{from}`"),
                )
                .help(format!("stages are: {}", titles.join(", "))),
            )
        })?;
    if let Some(first) = program.from_blockers(stage).first() {
        let info = program.sym(first.sym);
        let defined = &program.stages[info.stage - 1];
        return Err(Box::new(
            Diag::error(
                &first.loc.file,
                "from",
                first.loc.span,
                format!(
                    "cannot start at stage {stage}: `{}` comes from skipped stage {} \"{}\"",
                    info.name, defined.number, defined.title
                ),
            )
            .label("used here")
            .help(format!("start at stage {} or earlier", info.stage)),
        ));
    }
    Ok(stage)
}

pub struct Options {
    pub params: Vec<(String, String)>,
    pub yes: bool,
    pub from: Option<String>,
    pub grace: Duration,
    /// Gates and `confirm` read a whole line, ending with Enter, instead of a single key.
    pub require_enter: bool,
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
}

impl Ending {
    pub fn exit_code(self) -> u8 {
        match self {
            Ending::Completed => 0,
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
}

impl Stop {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            ending: Ending::Failed,
            message: message.into(),
            details: Vec::new(),
            poll_deadline: false,
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
        }
    }

    fn detail(mut self, line: impl Into<String>) -> Self {
        self.details.push(line.into());
        self
    }
}

/// An answer at a gate or a `confirm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Yes,
    No,
    /// Switch between the compact and the full view, then ask again.
    Toggle,
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
            view: Cell::new(if tty { View::Compact } else { View::Full }),
            named: Vec::new(),
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
            for stage in &p.stages {
                if stage.number < from {
                    continue;
                }
                self.stage(stage)?;
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
        let shown = self.opts.theme.lines(&self.redact_marked(line));
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
        let shown = self.opts.theme.lines(&chosen);
        self.screen.borrow_mut().print(&format!("{shown}\n"));
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

    /// With a live view, shows the steps still to run as ghost lines under the output.
    fn update_ghost(&self) {
        if !self.screen.borrow().live() {
            return;
        }
        let theme = self.opts.theme;
        let mut lines = Vec::new();
        for step in self.remaining() {
            render::step_lines(self.p, step, 1, self, None, &mut lines);
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
            theme.footer(&self.redactor.redact(stage), &self.redactor.redact(step))
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
            let value =
                rpassword::prompt_password(self.opts.theme.ask(&format!("{name} (hidden):")))
                    .map_err(|e| Stop::failed(format!("cannot read `{name}`: {e}")))?;
            if self.signals.take_interrupt() {
                return Err(Stop::cancelled("interrupted with Ctrl-C"));
            }
            Ok(value)
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

    /// Asks a yes/no question that sits under a preview. In a terminal, one key answers it:
    /// `y` approves, and `n`, Enter or Esc declines. `f` switches between the compact and
    /// the full view: korin erases the preview, draws it again with `redraw`, and asks
    /// again. With `--require-enter`, or without a terminal, it reads a line instead, and a
    /// line of `f` switches the view.
    ///
    /// `question` builds the question for the current view, since it may hold a path.
    fn ask_yes(
        &self,
        question: &dyn Fn(&Self) -> String,
        redraw: &dyn Fn(&Self),
    ) -> Result<bool, Stop> {
        loop {
            let hint = format!("y/N · f {}", self.view.get().other());
            let prompt = self.opts.theme.question(&question(self), &hint);
            let answer = if self.opts.require_enter || !std::io::stdin().is_terminal() {
                let line = self.read_line(&prompt)?;
                match line.trim() {
                    "y" | "Y" | "yes" | "Yes" => Answer::Yes,
                    "f" | "F" => Answer::Toggle,
                    _ => Answer::No,
                }
            } else {
                self.screen.borrow_mut().hide();
                let (answer, echo) = read_key_interruptible(&self.signals, &prompt, |key| {
                    match key {
                        [b'y' | b'Y', ..] => Some((Answer::Yes, "y")),
                        [b'f' | b'F', ..] => Some((Answer::Toggle, "f")),
                        [b'n' | b'N' | b'\r' | b'\n', ..] | [0x1b] => Some((Answer::No, "n")),
                        // Anything else, such as an arrow key's escape sequence, is ignored.
                        _ => None,
                    }
                })?;
                self.screen.borrow_mut().echoed(&format!("{prompt}{echo}"));
                answer
            };
            match answer {
                Answer::Yes => return Ok(true),
                Answer::No => return Ok(false),
                Answer::Toggle => {
                    self.view.set(self.view.get().toggled());
                    {
                        let mut screen = self.screen.borrow_mut();
                        screen.erase_to_mark();
                        screen.mark();
                    }
                    redraw(self);
                }
            }
        }
    }

    // ---- stages ----

    fn stage(&mut self, stage: &'p ir::Stage) -> Result<(), Stop> {
        let total = self.p.stages.len();
        self.say(&format!(
            "\n{}",
            render::stage_rule(stage.number, total, &stage.title, self.width())
        ));
        // Everything after the header is the preview; a live view erases it once approved.
        self.screen.borrow_mut().mark();
        if !stage.steps.is_empty() {
            self.say("");
            self.say_steps(&stage.steps.iter().collect::<Vec<_>>());
        }
        self.journal.event(
            "stage_start",
            json!({"stage": stage.number, "title": stage.title, "mutating": stage.mutating}),
        );
        self.journal
            .report(&format!("\n## Stage {} · {}\n", stage.number, stage.title));
        self.check_withheld_boundaries(&stage.steps)?;
        let gated = self.p.gates == GateMode::All || stage.mutating;
        if gated {
            if self.opts.yes {
                self.say("  approved by --yes");
                self.journal
                    .event("gate_auto", json!({"stage": stage.number}));
                self.journal.report("- gate approved by `--yes`");
            } else if self.ask_yes(&|_| format!("Run stage {}?", stage.number), &|r| {
                if !stage.steps.is_empty() {
                    r.say("");
                    r.say_steps(&stage.steps.iter().collect::<Vec<_>>());
                }
            })? {
                self.journal
                    .event("gate", json!({"stage": stage.number, "approved": true}));
                self.journal.report("- gate approved");
            } else {
                self.journal
                    .event("gate", json!({"stage": stage.number, "approved": false}));
                return Err(Stop::failed(format!(
                    "stage {} was not approved; nothing in it ran",
                    stage.number
                )));
            }
        }
        self.screen.borrow_mut().erase_to_mark();
        let prefix = format!("s{}", stage.number);
        let before = self.seq;
        self.footer = Some((
            format!(
                "Stage {}/{total} · {}",
                stage.number,
                render::escape_controls(&stage.title)
            ),
            String::new(),
        ));
        let result = self.steps(&stage.steps, &prefix, "", None);
        self.footer = None;
        self.screen.borrow_mut().clear_all();
        result?;
        // The stage-end line.
        let commands = self.seq - before;
        let tail = match commands {
            0 => String::new(),
            1 => " · 1 command".into(),
            n => format!(" · {n} commands"),
        };
        self.say(&format!("  ✓ stage {} done{tail}", stage.number));
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
                    self.say_steps(&steps);
                }
                let redraw = |r: &Self| {
                    if !steps.is_empty() {
                        r.say("  next:");
                        r.say_steps(&steps);
                    }
                };
                if self.ask_yes(&|r| r.shorten_text(&text), &redraw)? {
                    self.screen.borrow_mut().erase_to_mark();
                    self.update_ghost();
                    self.journal
                        .event("confirm", json!({"step": step.id, "approved": true}));
                    Ok(())
                } else {
                    self.journal
                        .event("confirm", json!({"step": step.id, "approved": false}));
                    Err(Stop::failed(format!(
                        "not confirmed at step {}: {text}",
                        step.id
                    )))
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
                let viewed = self.shorten_text(&text);
                match kind {
                    SayKind::Note => self.say(&format!("\n  {viewed}")),
                    SayKind::Warn => self.say(&format!("\n  ⚠ {viewed}")),
                    SayKind::Record => {
                        self.say(&format!("\n  ✓ {viewed}"));
                        self.journal.report(&format!("- {text}"));
                    }
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
        let viewed = render::argv(self.p, &command.argv, self);
        let prefix = format!("  {} ❯ ", step.id);
        let broken =
            render::command_lines(self.p, &command.argv, self, prefix.width(), self.width());
        self.say_block(
            &format!("\n{prefix}{viewed}"),
            &format!("\n{prefix}{}", broken.join("\n")),
        );
        // The journal, the suspend prompt and any failure detail show the full command.
        let shown = render::strip_marks(&render::argv(self.p, &command.argv, &Full(self)));
        self.journal.event(
            "command_start",
            json!({"invocation_id": invocation, "step": step.id, "command_kind": command.kind.keyword(), "argv": shown, "exe": exe.display().to_string()}),
        );

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
            let gutter = self.opts.theme.gutter();
            let screen = &self.screen;
            let live = screen.borrow().live();
            let prefixes = self.prefixes();
            let theme = self.opts.theme;
            // Shortens whole lines only, for the terminal; the journal gets the raw bytes.
            let view_lines = |text: &str| match &prefixes {
                Some(prefixes) => theme.output(&prefixes.shorten_text(text)),
                None => text.to_string(),
            };
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
                self.say(&format!("  ⚠ step {}: output pipes stayed open after the command was cancelled; a process it started may still be running", step.id));
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
                let items = match data {
                    Data::Str(s) => vec![s.clone()],
                    Data::List(items) => items.clone(),
                    Data::Bool(_) => Vec::new(),
                };
                for item in items {
                    if let Err(e) = self.redactor.add_value(&name, &item) {
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
            let gutter = self.opts.theme.gutter();
            let theme = self.opts.theme;
            let out = theme.output(&self.shorten_text(&self.redactor.redact_bytes(&stdout)));
            let err = theme.output(&self.shorten_text(&self.redactor.redact_bytes(&stderr)));
            let mut screen = self.screen.borrow_mut();
            screen.print(&indent(&out, &gutter));
            screen.print(&indent(&err, &gutter));
        } else if !published {
            self.say(&format!(
                "  output withheld: {}",
                placeholder(stdout.len() + stderr.len(), &unresolved)
            ));
        }
        if escaped {
            self.say(&format!("  ⚠ step {}: output pipes stayed open after the command ended; a process it started may still be running", step.id));
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
            self.say(&format!("    expect: {}", self.p.sym(sym).name));
        }

        // Store values and log captures.
        let mut failures = Vec::new();
        for (capture, result, file) in &results {
            let name = self.p.sym(capture.sym).name.clone();
            match result {
                Extract::Value(data) => {
                    if published {
                        self.journal.event("capture", json!({"invocation_id": invocation, "name": name, "value": data.to_json()}));
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
            Stop::failed(format!("`{}` has no value here", self.p.sym(sym).name))
                .detail("this is a korin bug if `korin check` passed; please report it")
        })
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
    for stage in &p.stages {
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
