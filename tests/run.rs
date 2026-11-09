//! Runner integration tests. They run the real `korin` binary without a terminal, so each
//! wizard declares `unattended allowed` and passes `--yes` unless the test is about refusal.

// Each emptiness assert carries a message that prints the offending items.
#![allow(clippy::assert_is_empty)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Env {
    dir: tempfile::TempDir,
    tmp: tempfile::TempDir,
}

struct Output {
    code: i32,
    text: String,
    elapsed: Duration,
}

const SECRET: &str = "S3CRET-VALUE-9f2c";

impl Env {
    fn new() -> Self {
        let env = Self {
            dir: tempfile::tempdir().unwrap(),
            tmp: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(env.path("bin")).unwrap();
        std::fs::create_dir_all(env.path("aws")).unwrap();
        let fake = std::fs::read_to_string("tests/fixtures/fake-aws").unwrap();
        env.script("bin/aws", &fake);
        env
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.path(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn script(&self, rel: &str, text: &str) {
        self.write(rel, text);
        std::fs::set_permissions(self.path(rel), std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Adds a fake-aws response for `argv`, for call number `n` (or every call when `None`).
    fn aws(&self, argv: &str, n: Option<u32>, stdout: &str, stderr: &str, exit: i32) {
        let index = std::fs::read_dir(self.path("aws/responses")).map_or(0, Iterator::count);
        let name = format!("aws/responses/{index:03}");
        self.write(&format!("{name}/argv"), argv);
        if let Some(n) = n {
            self.write(&format!("{name}/call"), &n.to_string());
        }
        self.write(&format!("{name}/stdout"), stdout);
        self.write(&format!("{name}/stderr"), stderr);
        self.write(&format!("{name}/exit"), &exit.to_string());
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("aws/calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn wizard(&self, body: &str) {
        self.write(
            "w.korin",
            &format!("korin 1\ntitle \"t\"\nunattended allowed\n{body}"),
        );
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_korin"));
        let path = format!(
            "{}:{}",
            self.path("bin").display(),
            std::env::var("PATH").unwrap()
        );
        cmd.args(args)
            .current_dir(self.dir.path())
            .env("PATH", path)
            .env("TMPDIR", self.tmp.path())
            .env("FAKE_AWS_DIR", self.path("aws"))
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        let start = Instant::now();
        let out = self.command(args).output().unwrap();
        Output {
            code: out.status.code().unwrap_or(-1),
            text: format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
            elapsed: start.elapsed(),
        }
    }

    fn run_yes_env(&self, extra: &[&str], env: &[(&str, &str)]) -> Output {
        let mut args = vec!["run", "w.korin", "--yes", "--kill-grace", "1"];
        args.extend_from_slice(extra);
        let mut cmd = self.command(&args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let start = Instant::now();
        let out = cmd.output().unwrap();
        Output {
            code: out.status.code().unwrap_or(-1),
            text: format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
            elapsed: start.elapsed(),
        }
    }

    fn run_yes(&self, extra: &[&str]) -> Output {
        let mut args = vec!["run", "w.korin", "--yes", "--kill-grace", "1"];
        args.extend_from_slice(extra);
        self.run(&args)
    }

    fn run_dir(&self) -> PathBuf {
        let runs = self.path(".korin/runs");
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(runs)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        dirs.sort();
        dirs.pop().unwrap()
    }

    fn events(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.run_dir().join("events.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn events_of(&self, kind: &str) -> Vec<serde_json::Value> {
        self.events()
            .into_iter()
            .filter(|e| e["kind"] == kind)
            .collect()
    }

    /// Every file under the run directories and the scratch root that contains `needle`,
    /// plus "terminal" if the captured output does.
    fn leaks(&self, needle: &str, output: &Output) -> Vec<String> {
        let mut hits = Vec::new();
        for root in [self.path(".korin"), self.tmp.path().to_path_buf()] {
            scan(&root, needle, &mut hits);
        }
        if output.text.contains(needle) {
            hits.push("terminal".into());
        }
        hits
    }

    fn scratch_dirs(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.tmp.path()).unwrap().flatten() {
            if entry.file_name().to_string_lossy().starts_with("korin-") {
                for inner in std::fs::read_dir(entry.path()).unwrap().flatten() {
                    out.push(inner.path());
                }
            }
        }
        out
    }
}

fn scan(dir: &Path, needle: &str, hits: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan(&path, needle, hits);
        } else if std::fs::read(&path).is_ok_and(|b| String::from_utf8_lossy(&b).contains(needle)) {
            hits.push(path.display().to_string());
        }
    }
}

// ---- unattended ----

#[test]
fn refuses_without_terminal_before_any_command() {
    let env = Env::new();
    env.aws("sts get-caller-identity", None, "{}", "", 0);
    env.write(
        "w.korin",
        "korin 1\ntitle \"t\"\nstage \"a\"\n  read aws sts get-caller-identity\n",
    );
    let out = env.run(&["run", "w.korin"]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(
        out.text.contains("refusing to run without a terminal"),
        "{}",
        out.text
    );
    assert!(env.calls().is_empty());
    let out = env.run(&["run", "w.korin", "--yes"]);
    assert_eq!(out.code, 1);
    assert!(env.calls().is_empty());
}

#[test]
fn unattended_runs_with_yes_and_logs_auto_gates() {
    let env = Env::new();
    env.aws(
        "sts get-caller-identity",
        None,
        "{\"Account\":\"123456789012\"}",
        "",
        0,
    );
    env.wizard("stage \"a\"\n  read aws sts get-caller-identity\n    capture account = jq .Account\n  record \"account {account}\"\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    assert_eq!(env.calls().len(), 1);
    assert_eq!(env.events_of("gate_auto").len(), 1);
    let report = std::fs::read_to_string(env.run_dir().join("report.md")).unwrap();
    assert!(report.contains("account 123456789012"), "{report}");
}

#[test]
fn missing_secret_stops_before_any_command() {
    let env = Env::new();
    env.aws("sts get-caller-identity", None, "{}", "", 0);
    env.wizard(
        "param pw secret\nstage \"a\"\n  read aws sts get-caller-identity\n    env PW={pw}\n",
    );
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(out.text.contains("KORIN_SECRET_PW"), "{}", out.text);
    assert!(env.calls().is_empty());
    let out = env.run_yes(&["--param", "pw=nope-not-here"]);
    assert!(
        out.text.contains("cannot be passed on the command line"),
        "{}",
        out.text
    );
}

#[test]
fn secret_param_reaches_env_but_never_artifacts() {
    let env = Env::new();
    env.script("bin/show", "#!/bin/sh\necho \"pw is $PW\"\n");
    env.wizard("param pw secret\nstage \"a\"\n  read show\n    env PW={pw}\n");
    let out = env
        .command(&["run", "w.korin", "--yes"])
        .env("KORIN_SECRET_PW", SECRET)
        .output()
        .unwrap();
    let output = Output {
        code: out.status.code().unwrap(),
        text: String::from_utf8_lossy(&out.stdout).into_owned(),
        elapsed: Duration::ZERO,
    };
    assert_eq!(output.code, 0, "{}", output.text);
    assert!(output.text.contains("pw is «secret:pw»"), "{}", output.text);
    assert!(
        env.leaks(SECRET, &output).is_empty(),
        "{:?}",
        env.leaks(SECRET, &output)
    );
}

// ---- secrets ----

fn secret_tool(env: &Env, name: &str, body: &str) {
    env.script(&format!("bin/{name}"), &format!("#!/bin/sh\n{body}\n"));
}

#[test]
fn captured_secret_is_redacted_on_success_failure_and_later_stop() {
    let env = Env::new();
    secret_tool(
        &env,
        "issue",
        &format!("echo '{{\"token\":\"{SECRET}\",\"note\":\"has {SECRET}\"}}'"),
    );
    secret_tool(
        &env,
        "leak",
        &format!("echo 'later output {SECRET}'; exit 4"),
    );
    env.wizard(concat!(
        "stage \"a\"\n",
        "  read issue\n",
        "    capture secret token = jq .token\n",
        "    capture note = jq .note\n",
        "  read leak\n",
        "    env T={token}\n",
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(
        env.leaks(SECRET, &out).is_empty(),
        "{:?}",
        env.leaks(SECRET, &out)
    );
    assert!(out.text.contains("«secret:token»"), "{}", out.text);

    let env = Env::new();
    secret_tool(&env, "issue", &format!("echo '{{\"token\":\"{SECRET}\"}}'"));
    env.wizard("stage \"a\"\n  read issue\n    capture secret token = jq .token\n    capture other = jq .missing\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(out.text.contains("yielded null"), "{}", out.text);
    assert!(
        env.leaks(SECRET, &out).is_empty(),
        "{:?}",
        env.leaks(SECRET, &out)
    );
}

#[test]
fn matched_expect_withholds_secret_output() {
    let env = Env::new();
    secret_tool(
        &env,
        "issue",
        &format!("echo 'out {SECRET}'; echo 'err {SECRET}' >&2; exit 2"),
    );
    env.wizard(concat!(
        "stage \"a\"\n",
        "  read issue\n",
        "    expect denied = exit 2\n",
        "    capture secret token = jq .token\n",
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(out.text.contains("output withheld"), "{}", out.text);
    assert!(
        env.leaks(SECRET, &out).is_empty(),
        "{:?}",
        env.leaks(SECRET, &out)
    );
    let stdout = std::fs::read_to_string(env.run_dir().join("out/0001-s1.1.stdout")).unwrap();
    assert!(stdout.starts_with("withheld:"), "{stdout}");
}

#[test]
fn absent_optional_secret_withholds_output() {
    let env = Env::new();
    secret_tool(&env, "issue", &format!("echo '{{\"other\":\"{SECRET}\"}}'"));
    env.wizard("stage \"a\"\n  read issue\n    capture secret token? = jq .token\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(
        env.leaks(SECRET, &out).is_empty(),
        "{:?}",
        env.leaks(SECRET, &out)
    );
}

#[test]
fn secret_file_captures_never_persist() {
    for (body, code) in [
        (format!("printf '%s' '{SECRET}' > \"$KORIN_OUT/r\""), 0),
        (format!("printf 'not json {SECRET}' > \"$KORIN_OUT/r\""), 1),
        (String::from("true"), 1),
    ] {
        let env = Env::new();
        secret_tool(&env, "issue", &body);
        let mode = if code == 0 { "text" } else { "jq .x" };
        env.wizard(&format!(
            "stage \"a\"\n  read issue\n    capture secret t = file {{korin.out}}/r {mode}\n"
        ));
        let out = env.run_yes(&[]);
        assert_eq!(out.code, code, "{body}: {}", out.text);
        assert!(
            env.leaks(SECRET, &out).is_empty(),
            "{body}: {:?}",
            env.leaks(SECRET, &out)
        );
        assert!(
            env.scratch_dirs().is_empty(),
            "scratch left behind: {:?}",
            env.scratch_dirs()
        );
    }
}

const WITHHOLDING_STEP: &str = concat!(
    "  read issue\n",
    "    expect o = exit 2\n",
    "    capture secret tok = jq .token\n",
    "    capture report = file {korin.out}/r.json jq json . on o\n",
);

fn withholding_env() -> Env {
    let env = Env::new();
    secret_tool(
        &env,
        "issue",
        &format!(
            "echo '{{\"token\":\"{SECRET}\"}}' > \"$KORIN_OUT/r.json\"; echo 'raw {SECRET}'; exit 2"
        ),
    );
    env.script(
        "print-token.sh",
        "#!/bin/sh\ntouch \"$(dirname \"$0\")/spawned\"\necho \"$1 $T\"\n",
    );
    env
}

#[test]
fn combined_case_withholds_file_copy_capture_event_and_note() {
    let env = withholding_env();
    env.wizard(&format!(
        "stage \"a\"\n{WITHHOLDING_STEP}  when o\n    note \"report: {{report}}\"\n"
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(
        out.text.contains("report: «withheld:report»"),
        "{}",
        out.text
    );
    assert!(
        env.leaks(SECRET, &out).is_empty(),
        "{:?}",
        env.leaks(SECRET, &out)
    );
    let capture = env
        .events_of("capture")
        .into_iter()
        .find(|e| e["name"] == "report")
        .unwrap();
    assert_eq!(capture["withheld"], true);
    assert!(capture.get("value").is_none());
    let copy = std::fs::read_to_string(env.run_dir().join("out/0001-s1.1/report")).unwrap();
    assert!(copy.starts_with("withheld:"), "{copy}");
}

#[test]
fn withheld_values_never_reach_a_command() {
    let variants = [
        "    script read ./print-token.sh {report}\n",
        "    script read ./print-token.sh x\n      env T={report}\n",
        "    m.consume({report})\n",
        "    each item in [\"{report}\"]\n      script read ./print-token.sh {item}\n",
    ];
    for variant in variants {
        let env = withholding_env();
        env.write(
            "lib/m.korin",
            "define consume(v)\n  script read ../print-token.sh {v}\n",
        );
        env.wizard(&format!(
            "use \"./lib/m.korin\"\nstage \"a\"\n{WITHHOLDING_STEP}stage \"b\"\n  when o\n{variant}"
        ));
        let check = env.run(&["check", "w.korin"]);
        let out = env.run_yes(&[]);
        assert_eq!(out.code, 1, "{variant}: {}", out.text);
        assert!(out.text.contains("is withheld"), "{variant}: {}", out.text);
        assert!(!env.path("spawned").exists(), "{variant}: the consumer ran");
        assert!(
            env.leaks(SECRET, &out).is_empty(),
            "{variant}: {:?}",
            env.leaks(SECRET, &out)
        );
        assert!(
            check.text.contains("may be withheld"),
            "{variant}: {}",
            check.text
        );
    }
}

#[test]
fn withheld_values_work_in_process() {
    let env = withholding_env();
    env.wizard(&format!(
        "stage \"a\"\n{WITHHOLDING_STEP}  when o and report == \"x\"\n    note \"never\"\n  when o\n    assert report != \"\" else \"empty\"\n    note \"in-process ok\"\n"
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    let said: Vec<String> = env
        .events_of("say")
        .iter()
        .map(|e| e["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(said, vec!["in-process ok".to_string()], "{}", out.text);
    assert!(env.leaks(SECRET, &out).is_empty());
}

// ---- cancellation and processes ----

#[test]
fn timeout_is_a_cancellation_that_expects_cannot_catch() {
    let env = Env::new();
    env.aws("sts get-caller-identity", None, "{}", "", 0);
    env.script("bin/slow", "#!/bin/sh\nsleep 30\n");
    env.wizard(concat!(
        "stage \"a\"\n",
        "  read slow\n",
        "    expect t = signal TERM\n",
        "    expect e = exit != 0\n",
        "    timeout 1s\n",
        "  read aws sts get-caller-identity\n",
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(out.text.contains("timed out"), "{}", out.text);
    assert!(env.calls().is_empty(), "a later command ran");
    assert!(out.elapsed < Duration::from_secs(8), "{:?}", out.elapsed);
}

#[test]
fn ctrl_c_cancels_and_no_later_command_runs() {
    let env = Env::new();
    env.aws("sts get-caller-identity", None, "{}", "", 0);
    env.script("bin/slow", "#!/bin/sh\nsleep 30\n");
    env.wizard("stage \"a\"\n  read slow\n    expect t = signal TERM\n  read aws sts get-caller-identity\n");
    let mut child = env
        .command(&["run", "w.korin", "--yes", "--kill-grace", "1"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(800));
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap()),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    let start = Instant::now();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130));
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(env.calls().is_empty());
}

fn pid_gone(path: &Path) -> bool {
    let pid: i32 = std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // A killed process can linger as a zombie until it is reaped; allow a moment for that.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

const STUBBORN: &str = r#"#!/bin/sh
trap '' TERM
( trap '' TERM; sleep 60 ) &
echo $! > "$PWD/gc.pid"
echo started
sleep 60
"#;

#[test]
fn descendant_ignoring_term_is_killed_within_grace() {
    // The grandchild holds stdout open and ignores TERM: first a timeout, then Ctrl-C.
    let env = Env::new();
    env.script("bin/stubborn", STUBBORN);
    // 3s, not 1s: under a full parallel test run the script can take over a second to start.
    env.wizard("stage \"a\"\n  read stubborn\n    timeout 3s\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    // timeout 3s + grace 1s + drain 2s, with slack.
    assert!(out.elapsed < Duration::from_secs(9), "{:?}", out.elapsed);
    assert!(
        pid_gone(&env.path("gc.pid")),
        "the grandchild survived a timeout"
    );

    let env = Env::new();
    env.script("bin/stubborn", STUBBORN);
    env.wizard("stage \"a\"\n  read stubborn\n");
    let mut child = env
        .command(&["run", "w.korin", "--yes", "--kill-grace", "1"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !env.path("gc.pid").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(200));
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap()),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(130));
    assert!(
        pid_gone(&env.path("gc.pid")),
        "the grandchild survived Ctrl-C"
    );
}

#[test]
fn group_is_killed_even_when_the_leader_exits_on_term() {
    // The leader dies on TERM at once and the grandchild holds no pipe, so the run could end
    // before the grace period. The group must still get SIGKILL.
    let env = Env::new();
    env.script(
        "bin/leader",
        "#!/bin/sh\necho leader-started\n( trap '' TERM; exec sleep 60 ) >/dev/null 2>&1 &\necho $! > \"$PWD/gc.pid\"\necho pid-written\nsleep 60\n",
    );
    env.wizard("stage \"a\"\n  read leader\n    timeout 3s\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(
        env.path("gc.pid").exists(),
        "no pid file; output:\n{}",
        out.text
    );
    assert!(pid_gone(&env.path("gc.pid")), "the grandchild survived");
}

#[test]
fn poll_deadline_caps_in_flight_command_and_keeps_every_attempt() {
    let env = Env::new();
    env.script("bin/probe", "#!/bin/sh\necho '{\"status\":\"creating\"}'\n");
    env.wizard(concat!(
        "stage \"a\"\n",
        "  poll every 1s for 6s\n",
        "    read probe\n",
        "      capture status = jq .status\n",
        "    until status == \"available\"\n",
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(out.text.contains("timed out"), "{}", out.text);
    let ends = env.events_of("command_end");
    assert!(ends.len() >= 3, "{ends:?}");
    let ids: Vec<String> = ends
        .iter()
        .map(|e| e["invocation_id"].as_str().unwrap().to_string())
        .collect();
    for id in &ids {
        assert!(
            env.run_dir().join(format!("out/{id}.stdout")).exists(),
            "{id}"
        );
    }
    assert!(
        ids[0].ends_with("@1") && ids[1].ends_with("@2") && ids[2].ends_with("@3"),
        "{ids:?}"
    );

    let env = Env::new();
    env.script("bin/slowprobe", "#!/bin/sh\nsleep 20\n");
    env.wizard(concat!(
        "stage \"a\"\n",
        "  poll every 1s for 2s\n",
        "    read slowprobe\n",
        "      capture status = text\n",
        "    until status == \"x\"\n",
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(out.elapsed < Duration::from_secs(10), "{:?}", out.elapsed);
}

// ---- expects ----

#[test]
fn expects_reset_on_every_attempt() {
    let env = Env::new();
    env.aws(
        "ssm get-parameter --name /x --output json",
        Some(1),
        "",
        "An error occurred (ParameterNotFound)",
        254,
    );
    env.aws(
        "ssm get-parameter --name /x --output json",
        Some(2),
        "{\"Parameter\":{\"Value\":\"v2\"}}",
        "",
        0,
    );
    env.wizard(concat!(
        "use aws\n",
        "stage \"a\"\n",
        "  poll every 1s for 10s\n",
        "    aws.ssm_get(/x) as prev\n",
        "    until not prev.missing\n",
        "  when not prev.missing\n",
        "    record \"value {prev.value}\"\n",
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    let expects = env.events_of("expect");
    assert_eq!(expects.len(), 1, "{expects:?}");
    assert!(
        expects[0]["invocation_id"]
            .as_str()
            .unwrap()
            .ends_with("@1")
    );
    let report = std::fs::read_to_string(env.run_dir().join("report.md")).unwrap();
    assert!(report.contains("value v2"), "{report}");
}

#[test]
fn first_matching_expect_wins() {
    let env = Env::new();
    env.script("bin/fails", "#!/bin/sh\nexit 1\n");
    env.wizard(concat!(
        "stage \"a\"\n",
        "  read fails\n",
        "    expect a = exit 1\n",
        "    expect b = exit != 0\n",
        "  when b\n",
        "    note \"b matched\"\n",
        "  when a\n",
        "    note \"a matched\"\n",
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    let said: Vec<String> = env
        .events_of("say")
        .iter()
        .map(|e| e["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(said, vec!["a matched".to_string()], "{}", out.text);
}

// ---- file captures ----

#[test]
fn file_capture_freshness() {
    // A stale file from an earlier run counts as missing.
    let env = Env::new();
    env.write("review.md", "old notes");
    env.script("bin/editor", "#!/bin/sh\ntrue\n");
    env.wizard(concat!(
        "stage \"a\"\n",
        "  read editor\n",
        "    expect skipped = missing review.md\n",
        "    capture notes = file review.md text\n",
        "  when skipped\n",
        "    note \"no new notes\"\n",
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(out.text.contains("no new notes"), "{}", out.text);

    // Without an expect, a missing file stops the run.
    env.wizard("stage \"a\"\n  read editor\n    capture notes = file absent.md text\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(
        out.text.contains("missing, or was not written"),
        "{}",
        out.text
    );

    // Malformed JSON stops, with an excerpt.
    env.script(
        "bin/writer",
        "#!/bin/sh\necho 'not { json' > \"$KORIN_OUT/r.json\"\necho stdout-line\n",
    );
    env.wizard("stage \"a\"\n  read writer\n    capture r = file {korin.out}/r.json jq .x\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(out.text.contains("not JSON"), "{}", out.text);
    assert!(
        out.text.contains("r.json") && out.text.contains("not { json"),
        "{}",
        out.text
    );

    // A fresh file under {korin.out} is accepted and copied into the journal.
    env.script(
        "bin/writer",
        "#!/bin/sh\necho '{\"x\":\"ok\"}' > \"$KORIN_OUT/r.json\"\n",
    );
    env.wizard("stage \"a\"\n  read writer\n    capture r = file {korin.out}/r.json jq .x\n  record \"got {r}\"\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(env.run_dir().join("out/0001-s1.1/r").exists());
}

// ---- executables and --from ----

#[test]
fn relative_executables_resolve_against_the_wizard_and_missing_ones_stop_early() {
    let env = Env::new();
    env.script("wiz/fix.sh", "#!/bin/sh\necho fixed\n");
    env.write(
        "wiz/w.korin",
        "korin 1\ntitle \"t\"\nunattended allowed\nstage \"a\"\n  script read ./fix.sh\n",
    );
    let out = env.run(&["run", "wiz/w.korin", "--yes"]);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(out.text.contains("fixed"));

    env.aws("sts get-caller-identity", None, "{}", "", 0);
    env.wizard(
        "stage \"a\"\n  read aws sts get-caller-identity\nstage \"b\"\n  read no-such-tool-xyz\n",
    );
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(
        out.text
            .contains("cannot find executable `no-such-tool-xyz`"),
        "{}",
        out.text
    );
    assert!(env.calls().is_empty());
}

#[test]
fn from_is_rejected_before_any_command() {
    let env = Env::new();
    env.aws("sts get-caller-identity", None, "{}", "", 0);
    env.wizard(concat!(
        "stage \"probe\"\n",
        "  read aws sts get-caller-identity\n",
        "    expect missing = exit 1\n",
        "stage \"fix\"\n",
        "  when missing\n",
        "    read aws sts get-caller-identity\n",
    ));
    let out = env.run_yes(&["--from", "fix"]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(
        out.text.contains("comes from skipped stage"),
        "{}",
        out.text
    );
    assert!(env.calls().is_empty());
}

#[test]
fn check_json_format() {
    let env = Env::new();
    env.write(
        "w.korin",
        "korin 1\ntitle \"t\"\nallow git\nstage \"a\"\n  read aws x\n",
    );
    let out = env
        .command(&["check", "w.korin", "--format", "json"])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    insta::assert_json_snapshot!(json);
}

#[test]
fn scratch_directories_are_private_and_never_reused() {
    use std::os::unix::fs::MetadataExt;
    let env = Env::new();
    env.script("bin/where", "#!/bin/sh\nstat -f '%Lp' \"$KORIN_OUT\" 2>/dev/null || stat -c '%a' \"$KORIN_OUT\"\nstat -f '%Lp' \"$(dirname \"$KORIN_OUT\")\" 2>/dev/null || stat -c '%a' \"$(dirname \"$KORIN_OUT\")\"\n");
    env.wizard("stage \"a\"\n  read where\n    capture modes = lines\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    let events = env.events_of("capture");
    assert_eq!(
        events[0]["value"],
        serde_json::json!(["700", "700"]),
        "{}",
        out.text
    );

    // A dead run's leftover directory is swept only when the current user owns it.
    let leftover = env.tmp.path().join("korin-999999-old");
    std::fs::create_dir(&leftover).unwrap();
    assert_eq!(
        std::fs::symlink_metadata(&leftover).unwrap().uid(),
        nix::unistd::getuid().as_raw()
    );
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(
        !leftover.exists(),
        "an owned leftover from a dead run was not swept"
    );
}

// ---- review fixes ----

#[test]
fn secret_split_across_writes_is_still_redacted() {
    let env = Env::new();
    let (head, tail) = SECRET.split_at(7);
    env.script(
        "bin/slowprint",
        &format!("#!/bin/sh\nprintf 'value {head}'\nsleep 0.3\nprintf '{tail} done\\n'\n"),
    );
    env.wizard("param pw secret\nstage \"a\"\n  read slowprint\n    env PW={pw}\n");
    let out = env.run_yes_env(&[], &[("KORIN_SECRET_PW", SECRET)]);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(out.text.contains("value «secret:pw» done"), "{}", out.text);
    assert!(
        env.leaks(SECRET, &out).is_empty(),
        "{:?}",
        env.leaks(SECRET, &out)
    );
}

#[test]
fn json_secrets_redact_in_every_encoding() {
    let env = Env::new();
    env.script(
        "bin/creds",
        r#"#!/bin/sh
cat <<'EOF'
{
  "Credentials": {
    "AccessKeyId": "AKIAEXAMPLEKEY123",
    "SecretAccessKey": "wJalrXUtnFEMIK7MDENGbPxRfiCY"
  }
}
EOF
"#,
    );
    env.script(
        "bin/sm",
        r#"#!/bin/sh
cat <<'EOF'
{"SecretString":"{\"password\":\"hunter2-long-pw\"}"}
EOF
"#,
    );
    env.wizard(concat!(
        "stage \"a\"\n",
        "  read creds\n",
        "    capture secret creds = jq json .Credentials\n",
        "  read sm\n",
        "    capture secret db = jq .SecretString\n",
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    for needle in [
        "AKIAEXAMPLEKEY123",
        "wJalrXUtnFEMIK7MDENGbPxRfiCY",
        "hunter2-long-pw",
    ] {
        assert!(
            env.leaks(needle, &out).is_empty(),
            "{needle}: {:?}",
            env.leaks(needle, &out)
        );
    }
}

#[test]
fn children_never_inherit_korin_secret_variables() {
    let env = Env::new();
    env.script("bin/dumpenv", "#!/bin/sh\nenv\n");
    env.wizard("param pw secret\nstage \"a\"\n  read dumpenv\n");
    let out = env.run_yes_env(&[], &[("KORIN_SECRET_PW", SECRET)]);
    assert_eq!(out.code, 0, "{}", out.text);
    let stdout = std::fs::read_to_string(env.run_dir().join("out/0001-s1.1.stdout")).unwrap();
    assert!(!stdout.contains("KORIN_SECRET_PW"), "{stdout}");
    assert!(
        env.leaks(SECRET, &out).is_empty(),
        "{:?}",
        env.leaks(SECRET, &out)
    );
}

#[test]
fn stop_messages_never_quote_unknown_secrets() {
    // A jq error on the unresolved secret would quote it.
    let env = Env::new();
    secret_tool(&env, "issue", &format!("echo '{{\"token\":\"{SECRET}\"}}'"));
    env.wizard("stage \"a\"\n  read issue\n    capture secret tok = jq .token.inner\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(out.text.contains("details withheld"), "{}", out.text);
    assert!(
        env.leaks(SECRET, &out).is_empty(),
        "{:?}",
        env.leaks(SECRET, &out)
    );

    // An ordering comparison on a withheld value would print it.
    let env = withholding_env();
    env.wizard(&format!(
        "stage \"a\"\n{WITHHOLDING_STEP}  when o\n    assert report > 3 else \"never\"\n"
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(
        out.text.contains("`report > 3` compares a value"),
        "{}",
        out.text
    );
    assert!(
        env.leaks(SECRET, &out).is_empty(),
        "{:?}",
        env.leaks(SECRET, &out)
    );
}

#[test]
fn displayed_arguments_are_quoted_and_escaped() {
    let env = Env::new();
    env.script(
        "bin/emit",
        r#"#!/bin/sh
cat <<'EOF'
{"v":"a b\rX"}
EOF
"#,
    );
    env.script("bin/take", "#!/bin/sh\ntrue\n");
    env.wizard("stage \"a\"\n  read emit\n    capture v = jq .v\n  read take {v}\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(out.text.contains(r"1.2 $ take $'a b\rX'"), "{}", out.text);
}

#[test]
fn poll_deadline_cancellation_reports_the_poll_and_expects_cannot_catch_it() {
    let env = Env::new();
    env.aws("sts get-caller-identity", None, "{}", "", 0);
    env.script("bin/slowish", "#!/bin/sh\nsleep 20\n");
    env.wizard(concat!(
        "stage \"a\"\n",
        "  poll every 1s for 2s\n",
        "    read slowish\n",
        "      expect t = signal TERM\n",
        "      expect e = exit != 0\n",
        "      capture s = text\n",
        "    until not t and not e and s == \"done\"\n",
        "  read aws sts get-caller-identity\n",
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(
        out.text.contains("poll at step 1.1 timed out"),
        "{}",
        out.text
    );
    assert!(env.calls().is_empty(), "a later command ran");
}

#[test]
fn short_ask_secret_from_the_environment_stops_before_stage_one() {
    let env = Env::new();
    env.aws("sts get-caller-identity", None, "{}", "", 0);
    env.wizard("stage \"a\"\n  read aws sts get-caller-identity\n  ask secret pin \"PIN?\"\n");
    let out = env.run_yes_env(&[], &[("KORIN_SECRET_PIN", "123")]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(out.text.contains("shorter than 4"), "{}", out.text);
    assert!(env.calls().is_empty());
}

#[test]
fn jq_capture_on_non_json_stdout_stops_with_an_excerpt() {
    let env = Env::new();
    env.script("bin/plain", "#!/bin/sh\necho 'plain text output'\n");
    env.wizard("stage \"a\"\n  read plain\n    capture x = jq .x\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(
        out.text.contains("not JSON") && out.text.contains("plain text output"),
        "{}",
        out.text
    );
}

#[test]
fn non_executable_path_stops_before_stage_one() {
    let env = Env::new();
    env.aws("sts get-caller-identity", None, "{}", "", 0);
    env.write("tool.sh", "#!/bin/sh\necho hi\n");
    env.wizard("stage \"a\"\n  read aws sts get-caller-identity\n  script read ./tool.sh\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(out.text.contains("cannot find executable"), "{}", out.text);
    assert!(env.calls().is_empty());
}

#[test]
fn plan_names_the_binding_that_blocks_from() {
    let env = Env::new();
    env.wizard(concat!(
        "stage \"probe\"\n",
        "  read aws sts get-caller-identity\n",
        "    expect missing = exit 1\n",
        "stage \"fix\"\n",
        "  when missing\n",
        "    read aws sts get-caller-identity\n",
    ));
    let out = env.run(&["plan", "w.korin"]);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(
        out.text.contains("not a --from start: uses `missing`"),
        "{}",
        out.text
    );
    let out = env
        .command(&["check", "w.korin", "--format", "json", "--from", "fix"])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    insta::assert_json_snapshot!(json);
}

#[test]
fn fmt_accepts_a_wizard_that_starts_with_a_comment() {
    let env = Env::new();
    env.write(
        "w.korin",
        "# A comment first.\nkorin 1\ntitle \"t\"\n\nstage \"a\"\n  note \"x\"\n",
    );
    let out = env.run(&["fmt", "--check", "w.korin"]);
    assert_eq!(out.code, 0, "{}", out.text);
}

#[test]
fn json_escaped_param_secret_is_redacted() {
    let env = Env::new();
    let password = r#"pa"ss\word-12"#;
    env.script(
        "bin/echojson",
        "#!/bin/sh\nprintf '{\"pw\":\"%s\"}\\n' \"$(printf '%s' \"$PW\" | sed 's/[\\\\\"]/\\\\&/g')\"\n",
    );
    env.wizard("param pw secret\nstage \"a\"\n  read echojson\n    env PW={pw}\n");
    let out = env.run_yes_env(&[], &[("KORIN_SECRET_PW", password)]);
    assert_eq!(out.code, 0, "{}", out.text);
    let escaped = r#"pa\"ss\\word-12"#;
    assert!(out.text.contains("«secret:pw»"), "{}", out.text);
    assert!(
        env.leaks(escaped, &out).is_empty(),
        "{:?}",
        env.leaks(escaped, &out)
    );
    assert!(
        env.leaks(password, &out).is_empty(),
        "{:?}",
        env.leaks(password, &out)
    );
}

#[test]
fn jq_json_keeps_strings_as_json() {
    let env = Env::new();
    env.script(
        "bin/doc",
        "#!/bin/sh\necho '{\"s\":\"hello\",\"n\":\"1234\"}'\n",
    );
    env.wizard(concat!(
        "stage \"a\"\n",
        "  read doc\n",
        "    capture s = jq json .s\n",
        "    capture n = jq json .n\n",
        "  record \"s={s} n={n}\"\n",
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    let report = std::fs::read_to_string(env.run_dir().join("report.md")).unwrap();
    assert!(report.contains(r#"s="hello" n="1234""#), "{report}");
}

#[test]
fn secret_file_capture_refuses_a_symlink_out_of_scratch() {
    let env = Env::new();
    env.write("outside.txt", SECRET);
    env.script(
        "bin/linker",
        "#!/bin/sh\nln -s \"$PWD/outside.txt\" \"$KORIN_OUT/r\"\n",
    );
    env.wizard("stage \"a\"\n  read linker\n    capture secret t = file {korin.out}/r text\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(
        out.text.contains("capture `t` at step 1.1 failed"),
        "{}",
        out.text
    );
    assert!(
        env.leaks(SECRET, &out).is_empty(),
        "{:?}",
        env.leaks(SECRET, &out)
    );
}

#[test]
fn numeric_json_secret_is_redacted_in_pretty_output() {
    let env = Env::new();
    env.script(
        "bin/pin",
        "#!/bin/sh\nprintf '{\\n  \"pin\": 987654321,\\n  \"ok\": true\\n}\\n'\n",
    );
    env.wizard("stage \"a\"\n  read pin\n    capture secret creds = jq json .\n");
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(
        env.leaks("987654321", &out).is_empty(),
        "{:?}",
        env.leaks("987654321", &out)
    );
}

#[test]
fn secret_file_capture_refuses_a_symlinked_directory_out_of_scratch() {
    let env = Env::new();
    env.script(
        "bin/linker",
        concat!(
            "#!/bin/sh\n",
            "mkdir -p \"$PWD/outdir\"\n",
            "ln -s \"$PWD/outdir\" \"$KORIN_OUT/link\"\n",
            "printf 'pw-0123456789' > \"$KORIN_OUT/link/token\"\n",
        ),
    );
    env.wizard(
        "stage \"a\"\n  read linker\n    capture secret t = file {korin.out}/link/token text\n",
    );
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 1, "{}", out.text);
    assert!(
        out.text.contains("capture `t` at step 1.1 failed"),
        "{}",
        out.text
    );
    assert!(
        env.leaks("pw-0123456789", &out).is_empty(),
        "{:?}",
        env.leaks("pw-0123456789", &out)
    );
}

#[test]
fn secret_file_capture_accepts_a_real_subdirectory_of_scratch() {
    let env = Env::new();
    env.script(
        "bin/writer",
        "#!/bin/sh\nmkdir \"$KORIN_OUT/sub\"\nprintf 'pw-0123456789' > \"$KORIN_OUT/sub/token\"\n",
    );
    env.wizard(concat!(
        "stage \"a\"\n",
        "  read writer\n",
        "    capture secret t = file {korin.out}/sub/token text\n",
        "  note \"done\"\n",
    ));
    let out = env.run_yes(&[]);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(
        env.leaks("pw-0123456789", &out).is_empty(),
        "{:?}",
        env.leaks("pw-0123456789", &out)
    );
}

#[test]
fn empty_path_entry_resolves_to_an_absolute_executable() {
    // `tool` is found through the empty PATH entry (the cwd). The step's own PATH must not
    // send the spawn to `evil/tool`.
    let env = Env::new();
    env.script("tool", "#!/bin/sh\necho good\n");
    env.script("evil/tool", "#!/bin/sh\necho evil\n");
    let evil = env.path("evil");
    env.wizard(&format!(
        "stage \"a\"\n  read tool\n    env PATH={}\n",
        evil.display()
    ));
    let out = env.run_yes_env(&[], &[("PATH", ":/usr/bin:/bin")]);
    assert_eq!(out.code, 0, "{}", out.text);
    let stdout = std::fs::read_to_string(env.run_dir().join("out/0001-s1.1.stdout")).unwrap();
    assert_eq!(stdout.trim(), "good", "{}", out.text);
}

#[test]
fn kill_grace_is_bounded() {
    let env = Env::new();
    env.aws("sts get-caller-identity", None, "{}", "", 0);
    env.wizard("stage \"a\"\n  read aws sts get-caller-identity\n");
    let out = env.run(&[
        "run",
        "w.korin",
        "--yes",
        "--kill-grace",
        "18446744073709551615",
    ]);
    assert_eq!(out.code, 2, "{}", out.text);
    assert!(out.text.contains("--kill-grace"), "{}", out.text);
    assert!(env.calls().is_empty(), "a command ran");
}
