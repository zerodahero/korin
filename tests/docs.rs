//! Keeps the docs site honest. Every example under `docs-site/examples/` and
//! `skills/korin/examples/` must pass `korin check` and `korin plan`, except those under
//! `invalid/`, which show diagnostics. Examples with a sidecar (`<name>.toml`) run in a pty,
//! and what the terminal shows at the end is saved as HTML for the site. CLI help and `korin modules` are
//! saved too. A test fails when a saved file no longer matches; `KORIN_UPDATE_DOCS=1`
//! rewrites them.

mod common;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::pty::Session;
use korin::theme::{Rgb, palette};
use portable_pty::CommandBuilder;
use serde::Deserialize;

const KORIN: &str = env!("CARGO_BIN_EXE_korin");
/// Tall enough that nothing scrolls off, so the final screen is the whole transcript.
const ROWS: u16 = 200;
const COLS: u16 = 100;
/// How long korin must be quiet before the test decides it is waiting for an answer.
const QUIET: Duration = Duration::from_millis(400);

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn examples() -> PathBuf {
    root().join("docs-site/examples")
}

fn generated() -> PathBuf {
    root().join("docs-site/docs/generated")
}

/// A sidecar: how to run one example for its transcript.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Sidecar {
    /// The korin subcommand: `run` (the default), `plan` or `check`.
    #[serde(default = "default_command")]
    command: String,
    /// The wizard to run, relative to the sidecar. Default: the sidecar's name with `.korin`.
    wizard: Option<String>,
    /// `--param` values.
    #[serde(default)]
    params: BTreeMap<String, String>,
    /// Extra arguments after the wizard, such as `--from`.
    #[serde(default)]
    args: Vec<String>,
    /// Extra environment, such as `KORIN_SECRET_TOKEN`.
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// Typed in order, each followed by Enter, whenever korin waits for input.
    #[serde(default)]
    answers: Vec<String>,
    /// A shell command run first, with `sh -c`, in the directory the example was copied to.
    setup: Option<String>,
    /// Copies the wizard to this name before the run, so the transcript shows the name a
    /// reader uses.
    run_as: Option<String>,
    /// Puts `docs-site/examples/bin` first on `PATH`.
    #[serde(default)]
    fakes: bool,
    /// Where the example's directory is copied, relative to the home directory.
    #[serde(default = "default_place")]
    place: String,
    /// Where korin runs, relative to the home directory. Default: `place`.
    cwd: Option<String>,
    /// The exit code korin must end with.
    #[serde(default)]
    exit: u32,
    /// Prompts (1 for the first answer) at which to also save the screen, before answering,
    /// as `<name>.prompt-<n>.html`. They show a gate's preview and the steps still to come.
    #[serde(default)]
    snapshots: Vec<usize>,
    /// Runs without a terminal, as CI would: stdin is `/dev/null`, and stdout and stderr are
    /// captured. Default: a pty.
    #[serde(default = "default_terminal")]
    terminal: bool,
    /// Also save the run's `report.md` and `events.jsonl` as `<name>.report.md` and
    /// `<name>.events.jsonl`.
    #[serde(default)]
    journal: bool,
}

/// What one sidecar produces: the final transcript, then any prompt snapshots.
struct Transcript {
    last: String,
    prompts: Vec<(usize, String)>,
    /// `(extension, content)` of journal files to save beside the transcript.
    journal: Vec<(&'static str, String)>,
}

fn default_command() -> String {
    "run".into()
}

fn default_terminal() -> bool {
    true
}

fn default_place() -> String {
    "project".into()
}

fn files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            files(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

/// Compares `content` with the committed file at `path`, or rewrites it when updating.
/// Returns a failure message, if any.
fn compare(path: &Path, content: &str) -> Option<String> {
    let shown = path.strip_prefix(root()).unwrap().display();
    if std::env::var_os("KORIN_UPDATE_DOCS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if std::fs::read_to_string(path).ok().as_deref() != Some(content) {
            std::fs::write(path, content).unwrap();
        }
        return None;
    }
    match std::fs::read_to_string(path) {
        Ok(old) if old == content => None,
        Ok(old) => Some(format!(
            "{shown} is out of date. Run `KORIN_UPDATE_DOCS=1 cargo nextest run --test docs`.\n{}",
            first_difference(&old, content)
        )),
        Err(_) => Some(format!(
            "{shown} is missing. Run `KORIN_UPDATE_DOCS=1 cargo nextest run --test docs`."
        )),
    }
}

fn first_difference(old: &str, new: &str) -> String {
    let (old, new): (Vec<_>, Vec<_>) = (old.lines().collect(), new.lines().collect());
    for i in 0..old.len().max(new.len()) {
        let (a, b) = (old.get(i), new.get(i));
        if a != b {
            return format!(
                "first difference at line {}:\n  committed: {}\n  now:       {}",
                i + 1,
                a.unwrap_or(&"<none>"),
                b.unwrap_or(&"<none>")
            );
        }
    }
    String::new()
}

// ---- examples are valid ----

#[test]
fn every_docs_example_checks_and_plans() {
    let mut wizards = Vec::new();
    files(&examples(), "korin", &mut wizards);
    // The agent skill's examples teach the language too, so they must stay valid.
    files(&root().join("skills/korin/examples"), "korin", &mut wizards);
    assert_ne!(
        wizards.len(),
        0,
        "no examples under {}",
        examples().display()
    );
    let mut failures = Vec::new();
    for wizard in wizards {
        // Module files under lib/ hold only `use` and `define`; their wizards check them.
        // Files under invalid/ show what `korin check` reports, so they must fail.
        if wizard
            .components()
            .any(|c| c.as_os_str() == "lib" || c.as_os_str() == "invalid")
        {
            continue;
        }
        for cmd in ["check", "plan"] {
            let out = Command::new(KORIN)
                .args(["--color", "never", cmd])
                .arg(&wizard)
                .output()
                .unwrap();
            if !out.status.success() {
                failures.push(format!(
                    "korin {cmd} {}:\n{}{}",
                    wizard.display(),
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ---- transcripts ----

#[test]
fn transcripts_match_the_examples() {
    let mut sidecars = Vec::new();
    files(&examples(), "toml", &mut sidecars);
    let results: Vec<(PathBuf, Result<Transcript, String>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = sidecars
            .iter()
            .map(|sidecar| (sidecar.clone(), scope.spawn(|| transcript(sidecar))))
            .collect();
        handles
            .into_iter()
            .map(|(path, h)| (path, h.join().unwrap()))
            .collect()
    });
    let mut failures = Vec::new();
    let mut expected = Vec::new();
    for (sidecar, result) in results {
        let base = generated()
            .join("transcripts")
            .join(sidecar.strip_prefix(examples()).unwrap());
        let path = base.with_extension("html");
        match result {
            Ok(transcript) => {
                failures.extend(compare(&path, &transcript.last));
                for (ext, content) in &transcript.journal {
                    let file = base.with_extension(ext);
                    failures.extend(compare(&file, content));
                    expected.push(file);
                }
                for (n, html) in &transcript.prompts {
                    let snapshot = base.with_extension(format!("prompt-{n}.html"));
                    failures.extend(compare(&snapshot, html));
                    expected.push(snapshot);
                }
            }
            Err(e) => failures.push(format!("{}: {e}", sidecar.display())),
        }
        expected.push(path);
    }
    // A transcript whose sidecar is gone would show a run that no longer happens.
    let mut existing = Vec::new();
    if generated().join("transcripts").exists() {
        for ext in ["html", "md", "jsonl"] {
            files(&generated().join("transcripts"), ext, &mut existing);
        }
    }
    for path in existing {
        if !expected.contains(&path) {
            if std::env::var_os("KORIN_UPDATE_DOCS").is_some() {
                std::fs::remove_file(&path).unwrap();
            } else {
                failures.push(format!("{} has no sidecar", path.display()));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

fn transcript(sidecar_path: &Path) -> Result<Transcript, String> {
    let text = std::fs::read_to_string(sidecar_path).unwrap();
    let sidecar: Sidecar = toml::from_str(&text).map_err(|e| e.to_string())?;
    let source_dir = sidecar_path.parent().unwrap();
    let wizard = sidecar.wizard.clone().unwrap_or_else(|| {
        format!(
            "{}.korin",
            sidecar_path.file_stem().unwrap().to_string_lossy()
        )
    });

    let home = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    let place = home.join(&sidecar.place);
    copy_dir(source_dir, &place);
    std::fs::create_dir_all(home.join("tmp")).unwrap();
    let mut path_var = std::env::var("PATH").unwrap();
    if sidecar.fakes {
        let bin = home.join(".fakes");
        copy_dir(&examples().join("bin"), &bin);
        path_var = format!("{}:{path_var}", bin.display());
    }
    let env = hermetic_env(&home, &path_var);

    if let Some(setup) = &sidecar.setup {
        let out = Command::new("sh")
            .args(["-c", setup])
            .current_dir(&place)
            .env_clear()
            .envs(&env)
            .output()
            .unwrap();
        if !out.status.success() {
            return Err(format!(
                "setup failed:\n{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }

    let wizard = match &sidecar.run_as {
        Some(name) => {
            std::fs::copy(place.join(&wizard), place.join(name)).unwrap();
            name.clone()
        }
        None => wizard,
    };
    let cwd = home.join(sidecar.cwd.as_deref().unwrap_or(&sidecar.place));
    let wizard_path = relative_path(&place.join(&wizard), &cwd);
    let mut cmd = CommandBuilder::new(KORIN);
    cmd.arg(&sidecar.command);
    cmd.arg(&wizard_path);
    for (name, value) in &sidecar.params {
        cmd.arg("--param");
        cmd.arg(format!("{name}={value}"));
    }
    cmd.args(&sidecar.args);
    cmd.cwd(&cwd);
    cmd.env_clear();
    for (key, value) in env.iter().chain(&sidecar.env) {
        cmd.env(key, value);
    }

    if !sidecar.terminal {
        return headless(&sidecar, &wizard_path, &cwd, &env, &home);
    }
    let mut session = Session::spawn(cmd, ROWS, COLS);
    let mut answers = sidecar.answers.iter();
    let mut prompts = Vec::new();
    let mut prompt = 0;
    loop {
        if session.settle(QUIET) {
            break;
        }
        // Quiet is not enough: korin may still be starting, or a command may be slow.
        if !at_prompt(&session.bytes()) {
            continue;
        }
        let Some(answer) = answers.next() else {
            return Err(format!(
                "korin is waiting for input, but no answers are left:\n{}",
                session.text()
            ));
        };
        prompt += 1;
        if sidecar.snapshots.contains(&prompt) {
            prompts.push((prompt, render(&normalize(&session.bytes(), &home))));
        }
        session.send(&format!("{answer}\r"));
    }
    let code = session.wait();
    if code != sidecar.exit {
        return Err(format!(
            "korin exited {code}, expected {}:\n{}",
            sidecar.exit,
            session.text()
        ));
    }
    if answers.next().is_some() {
        return Err(format!("answers were left over:\n{}", session.text()));
    }
    if let Some(missing) = sidecar.snapshots.iter().find(|n| **n > prompt) {
        return Err(format!(
            "snapshot {missing} asked for, but korin asked only {prompt} times"
        ));
    }
    let mut journal = Vec::new();
    if sidecar.journal {
        let runs = cwd.join(".korin/runs");
        let run = std::fs::read_dir(&runs)
            .map_err(|e| format!("no journal in {}: {e}", runs.display()))?
            .next()
            .ok_or("no run directory")?
            .unwrap()
            .path();
        let ts = regex::Regex::new(r#""ts":"[^"]+""#).unwrap();
        // A real program's path depends on the machine (git is /usr/bin/git on one,
        // /opt/homebrew/bin/git on another). Fakes live under the home, which is already fixed.
        let exe = regex::Regex::new(r#""exe":"(?:/[^"/]+)*/([^"/]+)""#).unwrap();
        for (file, ext) in [("report.md", "report.md"), ("events.jsonl", "events.jsonl")] {
            let bytes = std::fs::read(run.join(file)).map_err(|e| format!("{file}: {e}"))?;
            let text = String::from_utf8(normalize(&bytes, &home)).unwrap();
            let text = ts.replace_all(&text, r#""ts":"2026-01-15T10:00:00.000000Z""#);
            let text = exe.replace_all(&text, |c: &regex::Captures| {
                if c[0].starts_with(r#""exe":"/home/ada/"#) {
                    c[0].to_string()
                } else {
                    format!(r#""exe":"/usr/bin/{}""#, &c[1])
                }
            });
            journal.push((ext, text.into_owned()));
        }
    }
    Ok(Transcript {
        last: render(&normalize(&session.bytes(), &home)),
        prompts,
        journal,
    })
}

/// Runs korin with no terminal, as in CI, and renders what a CI log would show.
fn headless(
    sidecar: &Sidecar,
    wizard: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    home: &Path,
) -> Result<Transcript, String> {
    let mut cmd = Command::new(KORIN);
    cmd.arg(&sidecar.command).arg(wizard);
    for (name, value) in &sidecar.params {
        cmd.arg("--param").arg(format!("{name}={value}"));
    }
    let out = cmd
        .args(&sidecar.args)
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter().chain(&sidecar.env))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    let mut bytes = out.stdout;
    bytes.extend_from_slice(&out.stderr);
    if u32::try_from(code).ok() != Some(sidecar.exit) {
        return Err(format!(
            "korin exited {code}, expected {}:\n{}",
            sidecar.exit,
            String::from_utf8_lossy(&bytes)
        ));
    }
    // A pipe has no terminal to turn "\n" into "\r\n"; the emulator needs both.
    let bytes = String::from_utf8(normalize(&bytes, home))
        .unwrap()
        .replace('\n', "\r\n");
    Ok(Transcript {
        last: render(bytes.as_bytes()),
        prompts: Vec::new(),
        journal: Vec::new(),
    })
}

/// Whether the cursor sits on a korin prompt (`  ? …`), waiting for an answer.
fn at_prompt(bytes: &[u8]) -> bool {
    let mut parser = vt100::Parser::new(
        std::num::NonZero::new(ROWS).unwrap(),
        std::num::NonZero::new(COLS).unwrap(),
        0,
    );
    parser.process(bytes);
    let screen = parser.screen();
    let (row, _) = screen.cursor_position();
    let line: String = (0..COLS)
        .filter_map(|col| screen.cell(row, col))
        .map(|cell| {
            if cell.has_contents() {
                cell.contents()
            } else {
                " "
            }
        })
        .collect();
    line.trim_start().starts_with("? ")
}

/// The environment every example runs in: nothing from the developer's shell, so transcripts
/// are the same on every machine.
fn hermetic_env(home: &Path, path: &str) -> BTreeMap<String, String> {
    let home_s = home.display().to_string();
    [
        ("HOME", home_s.as_str()),
        ("PATH", path),
        ("TMPDIR", &format!("{home_s}/tmp")),
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("LANG", "en_US.UTF-8"),
        ("USER", "ada"),
        // git: no user config, a fixed identity and fixed dates, so commit IDs never change.
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_AUTHOR_NAME", "Ada"),
        ("GIT_AUTHOR_EMAIL", "ada@example.com"),
        ("GIT_COMMITTER_NAME", "Ada"),
        ("GIT_COMMITTER_EMAIL", "ada@example.com"),
        ("GIT_AUTHOR_DATE", "2026-01-15T10:00:00Z"),
        ("GIT_COMMITTER_DATE", "2026-01-15T10:00:00Z"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

fn relative_path(target: &Path, from: &Path) -> String {
    let (t, f): (Vec<_>, Vec<_>) = (target.components().collect(), from.components().collect());
    let common = t.iter().zip(&f).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".into(); f.len() - common];
    parts.extend(
        t[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

/// Replaces values that change on every run: the run's time stamp, and the temporary home
/// directory, which becomes `/home/ada`.
fn normalize(bytes: &[u8], home: &Path) -> Vec<u8> {
    let stamp = regex::bytes::Regex::new(r"\d{8}T\d{6}Z").unwrap();
    let mut out = stamp
        .replace_all(bytes, &b"20260115T100000Z"[..])
        .into_owned();
    let home = home.display().to_string();
    // macOS reports a temporary directory both with and without its `/private` prefix.
    for form in [
        home.clone(),
        home.trim_start_matches("/private").to_string(),
    ] {
        let pattern = regex::bytes::Regex::new(&regex::escape(&form)).unwrap();
        out = pattern.replace_all(&out, &b"/home/ada"[..]).into_owned();
    }
    out
}

/// Feeds the bytes to a terminal emulator, and writes its screen as HTML.
fn render(bytes: &[u8]) -> String {
    let mut parser = vt100::Parser::new(
        std::num::NonZero::new(ROWS).unwrap(),
        std::num::NonZero::new(COLS).unwrap(),
        0,
    );
    parser.process(bytes);
    let screen = parser.screen();
    let mut lines = Vec::new();
    for row in 0..ROWS {
        let mut line = String::new();
        let mut open: Option<String> = None;
        let mut pending_space = String::new();
        for col in 0..COLS {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let text = if cell.has_contents() {
                cell.contents()
            } else {
                " "
            };
            let class = classes(cell);
            // Spaces carry no visible style, so they never open a span; they are written
            // once something visible follows.
            if text == " " && !cell.underline() {
                pending_space.push(' ');
                continue;
            }
            if open.as_deref().unwrap_or_default() == class {
                line.push_str(&pending_space);
            } else {
                if open.take().is_some() {
                    line.push_str("</span>");
                }
                line.push_str(&pending_space);
                if !class.is_empty() {
                    let _ = write!(line, "<span class=\"{class}\">");
                    open = Some(class);
                }
            }
            pending_space.clear();
            line.push_str(&escape(text));
        }
        if open.is_some() {
            line.push_str("</span>");
        }
        lines.push(line);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    format!(
        "<pre class=\"k-term\"><code>{}</code></pre>\n",
        lines.join("\n")
    )
}

fn classes(cell: &vt100::Cell) -> String {
    let mut out = Vec::new();
    match cell.fgcolor() {
        vt100::Color::Default => {}
        vt100::Color::Rgb(r, g, b) => out.push(color_class(Rgb(r, g, b))),
        vt100::Color::Idx(i) => panic!("a 256-color index ({i}) in a truecolor run"),
    }
    if cell.bold() {
        out.push("k-bold");
    }
    if cell.dim() {
        out.push("k-dim");
    }
    if cell.italic() {
        out.push("k-italic");
    }
    out.retain(|c| !c.is_empty());
    out.join(" ")
}

fn color_class(color: Rgb) -> &'static str {
    match color {
        palette::FG => "",
        palette::FG_DIM => "k-fg-dim",
        palette::BORDER => "k-border",
        palette::BLUE => "k-blue",
        palette::CYAN => "k-cyan",
        palette::TEAL => "k-teal",
        palette::GREEN => "k-green",
        palette::MAGENTA => "k-magenta",
        palette::RED => "k-red",
        palette::COMMENT => "k-comment",
        other => panic!("{other:?} is not in theme::palette; add a class for it"),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ---- CLI help and the stdlib listing ----

#[test]
fn cli_help_and_modules_match() {
    let outputs: [(&str, &[&str]); 7] = [
        ("help/korin.txt", &["--help"]),
        ("help/run.txt", &["run", "--help"]),
        ("help/plan.txt", &["plan", "--help"]),
        ("help/check.txt", &["check", "--help"]),
        ("help/fmt.txt", &["fmt", "--help"]),
        ("help/modules.txt", &["modules", "--help"]),
        ("modules.txt", &["modules"]),
    ];
    let mut failures = Vec::new();
    for (name, args) in outputs {
        let out = Command::new(KORIN)
            .args(args)
            .env("NO_COLOR", "1")
            .env("COLUMNS", "100")
            .output()
            .unwrap();
        assert!(out.status.success(), "korin {args:?} failed");
        let text = String::from_utf8(out.stdout).unwrap();
        failures.extend(compare(&generated().join(name), &text));
    }
    // `check --format json` for an invalid example, as an agent would read it.
    let out = Command::new(KORIN)
        .args(["check", "--format", "json", "unguarded.korin"])
        .current_dir(examples().join("reference/cli/invalid"))
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "check should reject unguarded.korin"
    );
    let text = String::from_utf8(out.stdout).unwrap();
    failures.extend(compare(&generated().join("check-json.txt"), &text));
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
