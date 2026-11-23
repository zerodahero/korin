//! Tests that need a real terminal: gates, `interactive` handoffs and job control. korin runs
//! in a new session with a pty as its controlling terminal.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::pty::Session;
use portable_pty::CommandBuilder;

fn start(dir: &Path, args: &[&str]) -> Session {
    start_with_env(dir, args, &[])
}

fn start_with_env(dir: &Path, args: &[&str], env: &[(&str, PathBuf)]) -> Session {
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_korin"));
    cmd.args(args);
    cmd.cwd(dir);
    cmd.env(
        "PATH",
        format!(
            "{}:{}",
            dir.join("bin").display(),
            std::env::var("PATH").unwrap()
        ),
    );
    cmd.env("TMPDIR", dir.join("tmp"));
    // Color follows the test, not the developer's shell.
    cmd.env_remove("NO_COLOR");
    cmd.env_remove("CLICOLOR_FORCE");
    cmd.env("TERM", "xterm-256color");
    // The developer's own config must never change a result.
    cmd.env("KORIN_CONFIG", "/nonexistent/korin/config.kdl");
    for (key, value) in env {
        cmd.env(key, value);
    }
    Session::spawn(cmd, 40, 200)
}

fn setup(wizard: &str, scripts: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("bin")).unwrap();
    std::fs::create_dir_all(dir.path().join("tmp")).unwrap();
    std::fs::write(
        dir.path().join("w.korin"),
        format!("korin 1\ntitle \"t\"\n{wizard}"),
    )
    .unwrap();
    for (name, body) in scripts {
        let path = dir.path().join("bin").join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

fn marker(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

fn events(dir: &Path) -> String {
    let runs = dir.join(".korin/runs");
    let run = std::fs::read_dir(runs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::read_to_string(run.join("events.jsonl")).unwrap()
}

#[test]
fn declined_gate_runs_nothing() {
    let dir = setup(
        "stage \"a\"\n  mutate touchit\n",
        &[("touchit", "touch \"$PWD/touched\"")],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    s.answer("n");
    assert_eq!(s.wait(), 1, "{}", s.text());
    assert!(!marker(dir.path(), "touched").exists());
    assert!(s.text().contains("was not approved"), "{}", s.text());
}

#[test]
fn approved_gate_runs_and_yes_is_rejected_with_a_terminal() {
    let dir = setup(
        "stage \"a\"\n  mutate touchit\n",
        &[("touchit", "touch \"$PWD/touched\"")],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(marker(dir.path(), "touched").exists());

    let mut s = start(dir.path(), &["run", "w.korin", "--yes"]);
    assert_eq!(s.wait(), 1, "{}", s.text());
    assert!(
        s.text().contains("`--yes` is for runs without a terminal"),
        "{}",
        s.text()
    );
}

#[test]
fn interactive_handoff_with_file_capture() {
    let dir = setup(
        concat!(
            "stage \"a\"\n",
            "  interactive fakeui\n",
            "    capture notes = file {korin.out}/notes.md text\n",
            "  record \"notes: {notes}\"\n",
        ),
        &[("fakeui", "echo 'looks good' > \"$KORIN_OUT/notes.md\"")],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(s.text().contains("notes: looks good"), "{}", s.text());
    let events = events(dir.path());
    assert!(
        events.contains("\"handoff_start\"") && events.contains("\"handoff_end\""),
        "{events}"
    );
}

const SUSPENDABLE: &str = concat!(
    "stage \"a\"\n",
    "  interactive fakeui\n",
    "  mutate touchit\n",
);

fn suspendable() -> tempfile::TempDir {
    setup(
        SUSPENDABLE,
        &[
            ("fakeui", "echo ui-ready\nsleep 2\necho ui-done"),
            ("touchit", "touch \"$PWD/next-step-ran\""),
        ],
    )
}

#[test]
fn ctrl_z_then_resume_continues() {
    let dir = suspendable();
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    s.wait_for("ui-ready");
    s.send("\x1a");
    s.wait_for("is suspended");
    s.send("r\r");
    s.wait_for("ui-done");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(marker(dir.path(), "next-step-ran").exists(), "{}", s.text());
}

#[test]
fn ctrl_z_then_kill_cancels() {
    let dir = suspendable();
    let mut s = start(dir.path(), &["run", "w.korin", "--kill-grace", "1"]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    s.wait_for("ui-ready");
    s.send("\x1a");
    s.wait_for("is suspended");
    s.send("k\r");
    assert_eq!(s.wait(), 130, "{}", s.text());
    assert!(!marker(dir.path(), "next-step-ran").exists());
    assert!(
        s.text().contains("killed at the suspend prompt"),
        "{}",
        s.text()
    );
}

#[test]
fn sigterm_during_handoff_restores_and_stops() {
    for stop_first in [false, true] {
        let dir = setup(
            SUSPENDABLE,
            &[
                (
                    "fakeui",
                    "echo ui-ready\necho $$ > \"$PWD/ui.pid\"\nsleep 30",
                ),
                ("touchit", "touch \"$PWD/next-step-ran\""),
            ],
        );
        let mut s = start(dir.path(), &["run", "w.korin", "--kill-grace", "1"]);
        s.wait_for("Run stage 1?");
        s.answer("y");
        s.wait_for("ui-ready");
        if stop_first {
            s.send("\x1a");
            s.wait_for("is suspended");
        }
        let pid = nix::unistd::Pid::from_raw(s.pid());
        nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM).unwrap();
        assert_eq!(s.wait(), 130, "stop_first={stop_first}: {}", s.text());
        assert!(!marker(dir.path(), "next-step-ran").exists());
        let ui = std::fs::read_to_string(dir.path().join("ui.pid")).unwrap();
        let ui = nix::unistd::Pid::from_raw(ui.trim().parse().unwrap());
        assert!(
            nix::sys::signal::kill(ui, None).is_err(),
            "the interactive program is still alive"
        );
    }
}

/// The v1 acceptance test: the RDS rehearsal example, end to end, against fake-aws.
#[test]
fn rds_rehearsal_example_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for sub in ["bin", "tmp", "aws"] {
        std::fs::create_dir_all(root.join(sub)).unwrap();
    }
    std::fs::copy("tests/fixtures/fake-aws", root.join("bin/aws")).unwrap();
    std::fs::set_permissions(root.join("bin/aws"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::copy("examples/rds-rehearsal.korin", root.join("w.korin")).unwrap();

    let counter = std::cell::Cell::new(0);
    let respond = |argv: &str, n: Option<u32>, stdout: &str, stderr: &str, exit: i32| {
        let response = root
            .join("aws/responses")
            .join(format!("{:03}", counter.get()));
        counter.set(counter.get() + 1);
        std::fs::create_dir_all(&response).unwrap();
        std::fs::write(response.join("argv"), argv).unwrap();
        if let Some(n) = n {
            std::fs::write(response.join("call"), n.to_string()).unwrap();
        }
        std::fs::write(response.join("stdout"), stdout).unwrap();
        std::fs::write(response.join("stderr"), stderr).unwrap();
        std::fs::write(response.join("exit"), exit.to_string()).unwrap();
    };
    let cluster = |id: &str, status: &str| {
        format!(
            r#"{{"DBClusters":[{{"Status":"{status}","DBClusterArn":"arn:aws:rds:us-east-1:123456789012:cluster:{id}","EngineVersion":"16.4","Endpoint":"{id}.cluster-x.us-east-1.rds.amazonaws.com","DBClusterMembers":[],"VpcSecurityGroups":[{{"VpcSecurityGroupId":"sg-0a"}},{{"VpcSecurityGroupId":"sg-0b"}}]}}]}}"#
        )
    };
    let scratch = "main-db-cluster-rehearsal";
    respond(
        "sts get-caller-identity --output json",
        None,
        r#"{"Account":"123456789012","Arn":"arn:aws:iam::123456789012:user/operator"}"#,
        "",
        0,
    );
    respond(
        "rds describe-db-clusters --db-cluster-identifier main-db-cluster --output json",
        None,
        &cluster("main-db-cluster", "available"),
        "",
        0,
    );
    respond(
        &format!(
            "rds create-db-cluster-snapshot --db-cluster-identifier main-db-cluster --db-cluster-snapshot-identifier {scratch}-snap --output json"
        ),
        None,
        &format!(
            r#"{{"DBClusterSnapshot":{{"DBClusterSnapshotArn":"arn:aws:rds:us-east-1:123456789012:cluster-snapshot:{scratch}-snap"}}}}"#
        ),
        "",
        0,
    );
    respond(
        &format!(
            "rds describe-db-cluster-snapshots --db-cluster-snapshot-identifier {scratch}-snap --output json"
        ),
        None,
        r#"{"DBClusterSnapshots":[{"Status":"available"}]}"#,
        "",
        0,
    );
    let describe_scratch =
        format!("rds describe-db-clusters --db-cluster-identifier {scratch} --output json");
    respond(
        &describe_scratch,
        Some(1),
        "",
        "An error occurred (DBClusterNotFoundFault): DBClusterNotFound",
        254,
    );
    respond(
        &describe_scratch,
        Some(2),
        &cluster(scratch, "available"),
        "",
        0,
    );
    respond(
        &describe_scratch,
        Some(3),
        &cluster(scratch, "available"),
        "",
        0,
    );
    respond(
        &format!(
            "rds restore-db-cluster-from-snapshot --db-cluster-identifier {scratch} --snapshot-identifier arn:aws:rds:us-east-1:123456789012:cluster-snapshot:{scratch}-snap --engine aurora-postgresql --vpc-security-group-ids sg-0a sg-0b --output json"
        ),
        None,
        "{}",
        "",
        0,
    );
    respond(
        &format!(
            "rds delete-db-cluster --db-cluster-identifier {scratch} --skip-final-snapshot --output json"
        ),
        None,
        "{}",
        "",
        0,
    );

    let mut s = start_with_env(
        root,
        &["run", "w.korin", "--param", "cluster=main-db-cluster"],
        &[("FAKE_AWS_DIR", root.join("aws"))],
    );
    // Stages 1, 2 and 5 only read: under `gates mutating` they pause, and Enter continues.
    let mut pauses = 0;
    for stage in 1..=6 {
        if [3, 4, 6].contains(&stage) {
            s.wait_for(&format!("Run stage {stage}?"));
            s.answer("y");
        } else {
            pauses += 1;
            s.wait_for_nth("↵ continue", pauses);
            s.answer("");
        }
    }
    assert_eq!(s.wait(), 0, "{}", s.text());
    let calls = std::fs::read_to_string(root.join("aws/calls.log")).unwrap();
    assert!(
        calls.contains("--vpc-security-group-ids sg-0a sg-0b"),
        "{calls}"
    );
    assert_eq!(calls.lines().count(), 9, "{calls}");
    let runs = root.join(".korin/runs");
    let run = std::fs::read_dir(runs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let report = std::fs::read_to_string(run.join("report.md")).unwrap();
    assert!(
        report.contains("restored main-db-cluster-rehearsal: engine 16.4"),
        "{report}"
    );
    assert!(
        report.contains("deleted main-db-cluster-rehearsal"),
        "{report}"
    );
}

// ---- review fixes ----

impl Session {
    /// Answers a prompt as a human would: after the prompt's arming delay, a key and Enter.
    /// A single-key prompt takes the key and ignores the Enter; a line prompt takes both.
    fn answer(&mut self, key: &str) {
        std::thread::sleep(Duration::from_millis(400));
        self.send(&format!("{key}\r"));
    }

    /// Waits until `text` has appeared `n` times, such as the nth pause of a run.
    fn wait_for_nth(&self, text: &str, n: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while self.text().matches(text).count() < n {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {text:?} #{n}; output:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The pty's foreground process group, as the terminal driver sees it.
    fn foreground(&self) -> Option<i32> {
        self.master.process_group_leader()
    }

    /// The terminal's modes, compared by their debug form. `PENDIN` is left out: the kernel
    /// sets it on its own when line mode comes back with input still pending.
    fn modes(&self) -> String {
        let modes = format!("{:?}", self.master.get_termios());
        regex::Regex::new(r"c_lflag: \d+, | \| PENDIN")
            .unwrap()
            .replace_all(&modes, "")
            .into_owned()
    }
}

fn stopped(pid: i32) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains('T')
}

#[test]
fn type_ahead_never_answers_a_gate() {
    let dir = setup(
        "stage \"a\"\n  mutate sleepy\nstage \"b\"\n  mutate touchit\n",
        &[("sleepy", "sleep 1"), ("touchit", "touch \"$PWD/touched\"")],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    // Typed while stage 1 runs, before stage 2 is shown.
    std::thread::sleep(Duration::from_millis(300));
    s.send("y\r");
    s.wait_for("Run stage 2?");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !marker(dir.path(), "touched").exists(),
        "type-ahead approved stage 2"
    );
    s.answer("n");
    assert_eq!(s.wait(), 1, "{}", s.text());
    assert!(!marker(dir.path(), "touched").exists());
}

#[test]
fn handoff_returns_the_terminal_and_its_modes() {
    let dir = setup(
        "stage \"a\"\n  interactive fakeui\nstage \"b\"\n  mutate touchit\n",
        &[("fakeui", "stty raw -echo; sleep 0.3"), ("touchit", "true")],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    let before = s.modes();
    assert_eq!(s.foreground(), Some(s.pid()));
    s.answer("y");
    s.wait_for("Run stage 2?");
    // The program changed the modes; korin restored its own and took the terminal back.
    assert_eq!(
        s.foreground(),
        Some(s.pid()),
        "korin is not the foreground group"
    );
    assert_eq!(s.modes(), before, "terminal modes were not restored");
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
}

#[test]
fn korin_is_not_stopped_while_the_program_is_suspended() {
    let dir = suspendable();
    let mut s = start(dir.path(), &["run", "w.korin", "--kill-grace", "1"]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    s.wait_for("ui-ready");
    s.send("\x1a");
    s.wait_for("is suspended");
    assert!(!stopped(s.pid()), "korin itself was stopped");
    assert_eq!(
        s.foreground(),
        Some(s.pid()),
        "korin did not take the terminal back"
    );
    s.send("k\r");
    assert_eq!(s.wait(), 130, "{}", s.text());
}

#[test]
fn nested_confirm_shows_the_rest_of_the_stage() {
    let dir = setup(
        concat!(
            "stage \"a\"\n",
            "  read tool\n",
            "    expect missing = exit 1\n",
            "  when not missing\n",
            "    confirm \"Continue?\"\n",
            "  note \"after-the-block\"\n",
        ),
        &[("tool", "true")],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    s.wait_for("Continue? [y/N · s skip · v values · f full · l legend]");
    let text = s.text();
    let next = &text[text.rfind("next:").expect("confirm lists what comes next")..];
    assert!(next.contains("after-the-block"), "{next}");
    s.answer("n");
    assert_eq!(s.wait(), 1, "{}", s.text());
}

#[test]
fn deeply_nested_confirm_shows_every_remaining_step() {
    let dir = setup(
        concat!(
            "stage \"a\"\n",
            "  read tool\n",
            "    expect missing = exit 1\n",
            "  when not missing\n",
            "    each f in [\"one\"]\n",
            "      confirm \"Continue?\"\n",
            "      note \"inner-rest\"\n",
            "    note \"middle-rest\"\n",
            "  note \"outer-rest\"\n",
        ),
        &[("tool", "true")],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    s.wait_for("Continue? [y/N · s skip · v values · f full · l legend]");
    let text = s.text();
    let next = &text[text.rfind("next:").expect("confirm lists what comes next")..];
    for expected in ["inner-rest", "middle-rest", "outer-rest"] {
        assert!(next.contains(expected), "{expected} missing from:\n{next}");
    }
    s.answer("n");
    assert_eq!(s.wait(), 1, "{}", s.text());
}

#[test]
fn a_terminal_gets_carbonfox_colors_unless_no_color_is_set() {
    for (no_color, colored) in [(None, true), (Some("1"), false)] {
        let dir = setup("stage \"a\"\n  read tool\n", &[("tool", "true")]);
        let env: Vec<(&str, PathBuf)> = match no_color {
            Some(v) => vec![
                ("NO_COLOR", PathBuf::from(v)),
                ("COLORTERM", "truecolor".into()),
            ],
            None => vec![("COLORTERM", "truecolor".into())],
        };
        let mut s = start_with_env(dir.path(), &["run", "w.korin"], &env);
        s.wait_for("Run stage 1?");
        // Blue (78a9ff) for the prompt mark, as in carbonfox.
        assert_eq!(
            s.raw().contains("\x1b[1;38;2;120;169;255m❯"),
            colored,
            "{:?}",
            s.raw()
        );
        assert_eq!(s.raw().contains('\x1b'), colored, "{:?}", s.raw());
        s.answer("y");
        assert_eq!(s.wait(), 0, "{}", s.text());
    }
}

#[test]
fn live_view_erases_the_preview_and_ghosts_the_steps_to_come() {
    let dir = setup(
        "stage \"a\"\n  read slow\n  note \"still-to-come\"\n",
        &[("slow", "echo started; sleep 1; echo finished")],
    );
    let env = vec![("COLORTERM", PathBuf::from("truecolor"))];
    let mut s = start_with_env(dir.path(), &["run", "w.korin"], &env);
    s.wait_for("Run stage 1?");
    let before = s.raw().len();
    s.answer("y");
    s.wait_for("started");
    let running = s.raw()[before..].to_string();
    // The preview (a blank line, two step lines and the prompt) is erased back to the stage
    // header.
    assert!(running.contains("\x1b[4A\x1b[J"), "{running:?}");
    // The footer under the ghost names the stage and the step running now.
    let dim = "\x1b[38;2;123;124;126m";
    assert!(
        running.contains(&format!("{dim}Stage 1/1 · a\x1b[0m")),
        "{running:?}"
    );
    assert!(
        running.contains(&format!("{dim}step 1.1\x1b[0m")),
        "{running:?}"
    );
    // The note still to come is drawn in carbonfox's comment color under the output.
    let ghost = "\x1b[38;2;110;111;112m  1.2  note \"still-to-come\"\x1b[0m";
    assert!(running.contains(ghost), "{running:?}");
    assert_eq!(s.wait(), 0, "{}", s.text());
    // By the end the note has run for real, and the ghost is gone.
    let raw = s.raw();
    let last_ghost = raw.rfind(ghost).unwrap();
    assert!(
        raw[last_ghost..].contains("\x1b[J"),
        "the ghost was never erased"
    );
    assert!(s.text().contains("still-to-come"));
    // The footer is erased once the stage ends, and the stage-end line closes the stage.
    let last_footer = raw.rfind(&format!("{dim}step 1.2\x1b[0m")).unwrap();
    assert!(
        raw[last_footer..].contains("\x1b[J"),
        "the footer was never erased"
    );
    assert!(
        s.text().contains("✓ stage 1 done · 1 command"),
        "{}",
        s.text()
    );
}

#[test]
fn check_shows_graphical_diagnostics_in_a_terminal() {
    let dir = setup("stage \"a\"\n  note \"{nope}\"\n", &[]);
    let mut s = start(dir.path(), &["check", "w.korin"]);
    assert_eq!(s.wait(), 1, "{}", s.text());
    let text = s.text();
    // Graphical diagnostics draw the source with a frame; narrated ones spell it out.
    assert!(text.contains("unknown name `nope`"), "{text}");
    assert!(!text.contains("Diagnostic severity"), "{text}");
    assert!(text.contains("╭─["), "{text}");
}

#[test]
fn one_key_answers_a_gate_and_keys_while_arming_are_ignored() {
    let dir = setup(
        "stage \"a\"\n  mutate touchit\n",
        &[("touchit", "touch \"$PWD/touched\"")],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    // Pressed as the prompt appears: ignored.
    s.send("y");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !marker(dir.path(), "touched").exists(),
        "an early key approved"
    );
    // Any key but y, n, Enter or Esc is ignored too, including an arrow key.
    s.send("x");
    s.send("\x1b[A");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker(dir.path(), "touched").exists());
    // One key, no Enter, approves.
    s.send("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(marker(dir.path(), "touched").exists());
    assert!(
        s.text()
            .contains("Run stage 1? [y/N · s skip · v values · f full · l legend] y"),
        "{}",
        s.text()
    );
}

#[test]
fn enter_alone_declines_and_the_terminal_modes_come_back() {
    let dir = setup(
        "stage \"a\"\n  mutate touchit\nstage \"b\"\n  mutate touchit\n",
        &[("touchit", "true")],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    std::thread::sleep(Duration::from_millis(400));
    // While the prompt waits for a key, korin has turned off line mode and echo.
    let waiting = s.modes();
    s.send("y");
    s.wait_for("Run stage 2?");
    std::thread::sleep(Duration::from_millis(400));
    s.send("\r");
    assert_eq!(s.wait(), 1, "{}", s.text());
    assert!(
        s.text()
            .contains("Run stage 2? [y/N · s skip · v values · f full · l legend] n"),
        "{}",
        s.text()
    );
    assert_ne!(
        s.modes(),
        waiting,
        "korin left the terminal in single-key mode"
    );
}

#[test]
fn ctrl_c_at_a_single_key_prompt_cancels_and_restores_the_terminal() {
    let dir = setup(
        "stage \"a\"\n  read slow\nstage \"b\"\n  mutate touchit\n",
        &[("slow", "echo started; sleep 0.5"), ("touchit", "true")],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    // While stage 1 runs, the terminal is back in its usual line mode.
    s.wait_for("started");
    let line_mode = s.modes();
    s.wait_for("Run stage 2?");
    std::thread::sleep(Duration::from_millis(400));
    assert_ne!(
        s.modes(),
        line_mode,
        "the prompt did not switch to single keys"
    );
    s.send("\x03");
    assert_eq!(s.wait(), 130, "{}", s.text());
    assert_eq!(
        s.modes(),
        line_mode,
        "the terminal's modes were not restored"
    );
}

#[test]
fn require_enter_needs_a_whole_line() {
    let dir = setup(
        "stage \"a\"\n  mutate touchit\n",
        &[("touchit", "touch \"$PWD/touched\"")],
    );
    let mut s = start(dir.path(), &["run", "w.korin", "--require-enter"]);
    s.wait_for("Run stage 1?");
    std::thread::sleep(Duration::from_millis(400));
    s.send("y");
    std::thread::sleep(Duration::from_millis(400));
    assert!(!marker(dir.path(), "touched").exists(), "y alone approved");
    s.send("\r");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(marker(dir.path(), "touched").exists());
}

// ---- the compact view ----

fn run_dir(dir: &Path) -> PathBuf {
    std::fs::read_dir(dir.join(".korin/runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
}

/// A wizard that lists a file under a directory param, and the directory: deep enough to
/// become `<target>`. The path is canonical, as the cwd korin sees is.
fn target_setup(body: &str) -> (tempfile::TempDir, String) {
    let dir = setup(
        &format!("param target required\nstage \"a\"\n{body}"),
        &[("lister", "echo \"listed $1\"")],
    );
    let target = dir.path().canonicalize().unwrap().join("data/deep");
    (dir, target.display().to_string())
}

#[test]
fn f_at_a_gate_redraws_the_preview_in_full() {
    let (dir, target) = target_setup("  read lister {target}/a.txt\n");
    let param = format!("target={target}");
    let mut s = start(dir.path(), &["run", "w.korin", "--param", &param]);
    s.wait_for("[y/N · s skip · v values · f full · l legend]");
    assert!(s.text().contains("lister <target>/a.txt"), "{}", s.text());
    std::thread::sleep(Duration::from_millis(400));
    s.send("f");
    s.wait_for("[y/N · s skip · v values · f compact]");
    let after = s.text();
    let toggled = after.find("v values · f compact]").unwrap();
    let full = format!("lister {target}/a.txt");
    assert!(after.find(&full).is_some_and(|i| i < toggled), "{after}");
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    // The rest of the run stays in the full view.
    assert!(
        s.text().contains(&format!("listed {target}/a.txt")),
        "{}",
        s.text()
    );
    let events = std::fs::read_to_string(run_dir(dir.path()).join("events.jsonl")).unwrap();
    assert!(events.contains(&format!("{target}/a.txt")), "{events}");
    assert!(!events.contains("<target>"), "{events}");
}

#[test]
fn f_and_enter_toggles_with_require_enter() {
    let (dir, target) = target_setup("  read lister {target}/a.txt\n");
    let param = format!("target={target}");
    let mut s = start(
        dir.path(),
        &["run", "w.korin", "--require-enter", "--param", &param],
    );
    s.wait_for("[y/N · s skip · v values · f full · l legend]");
    std::thread::sleep(Duration::from_millis(400));
    s.send("f\r");
    s.wait_for("[y/N · s skip · v values · f compact]");
    std::thread::sleep(Duration::from_millis(400));
    s.send("y\r");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(
        s.text().contains(&format!("listed {target}/a.txt")),
        "{}",
        s.text()
    );
}

#[test]
fn the_terminal_shortens_and_the_journal_keeps_full_text() {
    let (dir, target) = target_setup(concat!(
        "  read lister {target}/a.txt\n",
        "  record \"wrote {target}/a.txt\"\n",
    ));
    let param = format!("target={target}");
    let mut s = start(dir.path(), &["run", "w.korin", "--param", &param]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    let text = s.text();
    assert!(text.contains("1.1 ❯ lister <target>/a.txt"), "{text}");
    assert!(text.contains("listed <target>/a.txt"), "{text}");
    assert!(text.contains("✓ wrote <target>/a.txt"), "{text}");
    assert!(text.contains("journal: ./.korin/runs/"), "{text}");
    let run = run_dir(dir.path());
    let report = std::fs::read_to_string(run.join("report.md")).unwrap();
    assert!(
        report.contains(&format!("- wrote {target}/a.txt")),
        "{report}"
    );
    let events = std::fs::read_to_string(run.join("events.jsonl")).unwrap();
    assert!(
        !events.contains("<target>") && !events.contains("<pwd>"),
        "{events}"
    );
}

#[test]
fn a_huge_value_is_cut_at_a_gate_and_a_confirm() {
    let dir = setup(
        "param big required\nstage \"a\"\n  confirm \"go on?\"\n  read echo {big}\n",
        &[],
    );
    let big = "x".repeat(400);
    let param = format!("big={big}");
    let mut s = start(dir.path(), &["run", "w.korin", "--param", &param]);
    s.wait_for("Run stage 1?");
    // The intro echoes the command line; the preview starts at the stage rule.
    let text = s.text();
    let preview = &text[text.find("Stage 1/1").unwrap()..];
    assert!(preview.contains("read ❯ echo"), "{preview}");
    assert!(preview.contains("…(+"), "{preview}");
    assert!(!preview.contains(&big), "the gate showed the whole value");
    s.answer("y");
    s.wait_for("go on? [y/N");
    let at_confirm = s.text();
    let after_gate = at_confirm.find("Run stage 1?").unwrap();
    assert!(at_confirm[after_gate..].contains("…(+"), "{at_confirm}");
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    let events = std::fs::read_to_string(run_dir(dir.path()).join("events.jsonl")).unwrap();
    assert!(events.contains(&big), "the journal lost the full value");
}

#[test]
fn a_prefix_follows_the_current_value_of_its_name() {
    let dir = setup(
        concat!(
            "stage \"a\"\n",
            "  each d in [\"/a/b/c\", \"/x/y/z\"]\n",
            "    read pick {d} /a/b/c/q\n",
            "      capture p = text\n",
            "    read echo /a/b/c/old {p}/new\n",
        ),
        &[("pick", "echo \"$1\"")],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    let text = s.text();
    // While p is /a/b/c, both paths shorten.
    assert!(text.contains("│ <p>/old <p>/new"), "{text}");
    // The next command clears p before it shows, so the old prefix is gone.
    assert!(text.contains("❯ pick /x/y/z /a/b/c/q"), "{text}");
    assert!(!text.contains("<p>/q"), "{text}");
    // Once p is /x/y/z, only the new path shortens.
    assert!(text.contains("│ /a/b/c/old <p>/new"), "{text}");
}

// ---- guide mode ----

fn guide_setup(body: &str, scripts: &[(&str, &str)]) -> tempfile::TempDir {
    setup(&format!("mode guide\n{body}"), scripts)
}

#[test]
fn a_guide_pauses_before_each_stage_and_shows_no_preview() {
    let dir = guide_setup(
        concat!(
            "stage \"one\"\n  read echo first-ran\n",
            "stage \"two\"\n  note \"between\"\n  read echo second-ran\n",
        ),
        &[],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("↵ continue · s skip · v values · f full");
    // No preview: the step is not on screen before it runs.
    assert!(!s.text().contains("echo first-ran"), "{}", s.text());
    assert!(!s.text().contains("Run stage"), "{}", s.text());
    std::thread::sleep(Duration::from_millis(400));
    // A key that is not Enter, Space or f is ignored.
    s.send("x");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!s.text().contains("first-ran"), "x continued: {}", s.text());
    s.send("\r");
    s.wait_for("1.1 ❯ echo first-ran");
    s.wait_for("Stage 2/2");
    std::thread::sleep(Duration::from_millis(400));
    s.send(" ");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(s.text().contains("2.2 ❯ echo second-ran"), "{}", s.text());
    let events = events(dir.path());
    assert_eq!(events.matches("\"pause\"").count(), 2, "{events}");
    assert!(!events.contains("\"gate\""), "{events}");
}

#[test]
fn a_guide_pause_takes_a_line_with_require_enter() {
    let dir = guide_setup("stage \"one\"\n  read echo ran\n", &[]);
    let mut s = start(dir.path(), &["run", "w.korin", "--require-enter"]);
    s.wait_for("↵ continue · s skip · v values · f full");
    std::thread::sleep(Duration::from_millis(400));
    s.send("f\r");
    s.wait_for("↵ continue · s skip · v values · f compact");
    std::thread::sleep(Duration::from_millis(400));
    s.send("\r");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(s.text().contains("1.1 ❯ echo ran"), "{}", s.text());
}

#[test]
fn a_guide_pauses_before_an_interactive_step() {
    let dir = guide_setup(
        "stage \"one\"\n  interactive fakeui\n",
        &[("fakeui", "touch \"$PWD/opened\"")],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("↵ continue");
    s.answer("");
    s.wait_for("↵ open · Ctrl-C cancel");
    assert!(s.text().contains("1.1 ❯ fakeui"), "{}", s.text());
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        !marker(dir.path(), "opened").exists(),
        "opened before the pause"
    );
    s.send("\r");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(marker(dir.path(), "opened").exists());
}

// ---- hidden input ----

/// What the terminal shows once every byte so far is drawn, row by row.
fn screen(s: &Session) -> String {
    let rows = std::num::NonZero::new(40).unwrap();
    let cols = std::num::NonZero::new(200).unwrap();
    let mut parser = vt100::Parser::new(rows, cols, 0);
    parser.process(&s.bytes());
    parser.screen().contents()
}

#[test]
fn hidden_input_counts_characters_and_leaves_only_a_tick() {
    let dir = setup(
        "stage \"a\"\n  ask secret token \"Token?\"\n  read showlen {token}\n",
        &[("showlen", "echo \"len=${#1}\"")],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    std::thread::sleep(Duration::from_millis(400));
    s.send("y");
    s.wait_for("token (hidden):");
    s.send("ab");
    s.wait_for("· 2 chars");
    // A paste arrives as one read; é is two bytes but one character.
    s.send("cdé");
    s.wait_for("· 5 chars");
    s.send("\x7f");
    s.wait_for("· 4 chars");
    // An arrow key is ignored, and Ctrl-U clears the line.
    s.send("\x1b[D\x15");
    s.wait_for("· 0 chars");
    s.send("s3cret");
    s.wait_for("· 6 chars");
    s.send("\r");
    assert_eq!(s.wait(), 0, "{}", s.text());
    let shown = screen(&s);
    assert!(shown.contains("token (hidden): ✓"), "{shown}");
    assert!(
        !shown.contains("chars"),
        "the length stayed on screen:\n{shown}"
    );
    assert!(!s.text().contains("s3cret"), "{}", s.text());
    // The command got the whole value, and the journal holds no length.
    assert!(s.text().contains("len=6"), "{}", s.text());
    assert!(!events(dir.path()).contains("chars"));
}

// ---- the config file ----

#[test]
fn the_config_file_sets_the_starting_view_and_require_enter() {
    let dir = setup("stage \"a\"\n  read true\n", &[]);
    let config = dir.path().join("config.kdl");
    std::fs::write(&config, "view full\nrequire-enter #true\n").unwrap();
    let mut s = start_with_env(dir.path(), &["run", "w.korin"], &[("KORIN_CONFIG", config)]);
    s.wait_for("v values · f compact]");
    std::thread::sleep(Duration::from_millis(400));
    // With require-enter, y alone does not answer the gate.
    s.send("y");
    std::thread::sleep(Duration::from_millis(400));
    assert!(!s.text().contains("stage 1 done"), "{}", s.text());
    s.send("\r");
    assert_eq!(s.wait(), 0, "{}", s.text());
}

// ---- the legend ----

#[test]
fn the_legend_explains_each_prefix_and_l_hides_it() {
    let (dir, target) = target_setup("  read lister {target}/a.txt\n");
    let param = format!("target={target}");
    let mut s = start(dir.path(), &["run", "w.korin", "--param", &param]);
    s.wait_for("[y/N · s skip · v values · f full · l legend]");
    let entry = format!("<target> = {target}");
    let before = s.text();
    // Once when the param is set, and once under the preview that shows it.
    assert_eq!(before.matches(&entry).count(), 2, "{before}");
    assert!(before.contains("<pwd> = "), "{before}");
    std::thread::sleep(Duration::from_millis(400));
    s.send("l");
    s.wait_for_nth("[y/N · s skip · v values · f full · l legend]", 2);
    let after = s.text();
    let redrawn = &after[after.find("legend] l").unwrap()..];
    assert!(redrawn.contains("lister <target>/a.txt"), "{redrawn}");
    assert!(
        !redrawn.contains(&entry),
        "the legend is still shown:\n{redrawn}"
    );
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    let events = std::fs::read_to_string(run_dir(dir.path()).join("events.jsonl")).unwrap();
    assert!(!events.contains(" = "), "{events}");
}

#[test]
fn the_config_file_can_start_with_the_legend_hidden() {
    let (dir, target) = target_setup("  read lister {target}/a.txt\n");
    let config = dir.path().join("config.kdl");
    std::fs::write(&config, "legend #false\n").unwrap();
    let param = format!("target={target}");
    let mut s = start_with_env(
        dir.path(),
        &["run", "w.korin", "--param", &param],
        &[("KORIN_CONFIG", config)],
    );
    s.wait_for("[y/N · s skip · v values · f full · l legend]");
    assert!(!s.text().contains(" = "), "{}", s.text());
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
}

#[test]
fn the_footer_lists_each_name_while_a_stage_runs() {
    let (dir, target) = target_setup("  read lister {target}/a.txt\n  read sleep 0.3\n");
    let param = format!("target={target}");
    let mut s = start(dir.path(), &["run", "w.korin", "--param", &param]);
    s.wait_for("[y/N · s skip · v values · f full · l legend]");
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    let text = s.text();
    let running = &text[text.find("legend] y").unwrap()..];
    assert!(running.contains("Stage 1/1"), "{running}");
    assert!(
        running.contains(&format!("<target> = {target}")),
        "{running}"
    );
}

// ---- the values view ----

#[test]
fn v_at_a_gate_changes_a_value_and_redraws_the_preview() {
    let dir = setup(
        "param name = \"world\"\nstage \"a\"\n  read greet {name}\n",
        &[("greet", "echo \"hello $1\"")],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("[y/N · s skip · v values · f full · l legend]");
    std::thread::sleep(Duration::from_millis(400));
    s.send("v");
    s.wait_for("number to change");
    assert!(s.text().contains("1  name  world  param"), "{}", s.text());
    s.send("1\r");
    s.wait_for("r replace · Enter keep");
    std::thread::sleep(Duration::from_millis(400));
    s.send("r");
    s.wait_for_nth("? name:", 2);
    s.send("mars\r");
    s.wait_for_nth("number to change", 2);
    s.send("\r");
    s.wait_for_nth("[y/N · s skip · v values · f full · l legend]", 2);
    let text = s.text();
    let redrawn = &text[text.rfind("number to change").unwrap()..];
    assert!(redrawn.contains("greet mars"), "{redrawn}");
    std::thread::sleep(Duration::from_millis(400));
    s.send("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(s.text().contains("hello mars"), "{}", s.text());
    let events = events(dir.path());
    assert!(
        events.contains(r#""by":"operator","kind":"value_set","name":"name""#)
            && events.contains(r#""value":"mars""#),
        "{events}"
    );
}

#[test]
fn a_withheld_value_shows_only_its_placeholder_until_replaced() {
    const SECRET: &str = "hunter2-very-secret";
    let dir = setup(
        concat!(
            "stage \"a\"\n",
            "  read issue\n",
            "    expect o = exit 2\n",
            "    capture secret tok = jq .token\n",
            "    capture report = file {korin.out}/r.json jq json . on o\n",
            "stage \"b\"\n",
            "  note \"between\"\n",
            "stage \"c\"\n",
            "  when o\n",
            "    read show {report}\n",
        ),
        &[
            (
                "issue",
                &format!(
                    "echo '{{\"token\":\"{SECRET}\"}}' > \"$KORIN_OUT/r.json\"; echo 'raw {SECRET}'; exit 2"
                ),
            ),
            ("show", "echo \"got $1\""),
        ],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    s.wait_for("Run stage 2?");
    std::thread::sleep(Duration::from_millis(400));
    s.send("v");
    s.wait_for("number to change");
    let text = s.text();
    let row = text
        .lines()
        .find(|l| {
            let words: Vec<&str> = l.split_whitespace().collect();
            words.len() > 1 && words[0].parse::<usize>().is_ok() && words[1] == "report"
        })
        .unwrap_or_else(|| panic!("no report row:\n{text}"));
    assert!(row.contains("«withheld:report»"), "{row}");
    let number = row.split_whitespace().next().unwrap().to_string();
    s.send(&format!("{number}\r"));
    s.wait_for("report: r replace");
    // The edit prompt never offers the old value.
    assert!(!s.text().contains(SECRET), "{}", s.text());
    std::thread::sleep(Duration::from_millis(400));
    s.send("r");
    s.wait_for("report (JSON):");
    s.send("not json\r");
    s.wait_for("not a value for `report`");
    s.send("\"fixed\"\r");
    s.wait_for_nth("number to change", 2);
    s.send("\r");
    s.wait_for_nth("Run stage 2?", 2);
    s.answer("y");
    s.wait_for("Run stage 3?");
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(s.text().contains("got \"fixed\""), "{}", s.text());
    assert!(!s.text().contains(SECRET), "{}", s.text());
}

/// The number of a name's row in the values view, from the last listing shown.
fn row_number(text: &str, name: &str) -> String {
    let listing = &text[text.rfind("  values").unwrap()..];
    listing
        .lines()
        .find_map(|l| {
            let words: Vec<&str> = l.split_whitespace().collect();
            (words.len() > 1 && words[0].parse::<usize>().is_ok() && words[1] == name)
                .then(|| words[0].to_string())
        })
        .unwrap_or_else(|| panic!("no row for {name}:\n{listing}"))
}

#[test]
fn values_made_in_the_stage_can_be_edited_by_type_at_a_confirm() {
    let dir = setup(
        concat!(
            "stage \"a\"\n",
            "  read printf \"one\\ntwo\\n\"\n",
            "    capture items = lines\n",
            "  read echo tag=x\n",
            "    capture tag? = regex /tag=(\\w+)/\n",
            "  read echo hi\n",
            "    capture word = text\n",
            "  confirm \"Continue?\"\n",
            "  read showargs {items...}\n",
            "  each it in items\n",
            "    read showone {it}\n",
            "  when exists tag\n",
            "    note \"tag is {tag}\"\n",
            "  read showargs \"[{word}]\"\n",
        ),
        &[
            ("showargs", "echo \"$# args: $*\""),
            ("showone", "echo \"one:$1\""),
        ],
    );
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    s.wait_for("Continue?");
    std::thread::sleep(Duration::from_millis(400));
    s.send("v");
    s.wait_for("number to change");
    let text = s.text();
    assert!(text.contains(r#"["one","two"]"#), "{text}");
    // A list: one item per line, then an empty line.
    s.send(&format!("{}\r", row_number(&text, "items")));
    s.wait_for("items: r replace");
    std::thread::sleep(Duration::from_millis(400));
    s.send("r");
    s.wait_for("one item per line");
    s.send("a b\r");
    s.wait_for_nth("…", 1);
    s.send("c\r");
    s.wait_for_nth("…", 2);
    s.send("\r");
    // The empty string.
    s.wait_for_nth("number to change", 2);
    s.send(&format!("{}\r", row_number(&s.text(), "word")));
    s.wait_for("word: r replace");
    std::thread::sleep(Duration::from_millis(400));
    s.send("r");
    s.wait_for_nth("? word:", 2);
    s.send("\r");
    // Clearing an optional capture.
    s.wait_for_nth("number to change", 3);
    s.send(&format!("{}\r", row_number(&s.text(), "tag")));
    s.wait_for("tag: r replace · c clear");
    std::thread::sleep(Duration::from_millis(400));
    s.send("c");
    s.wait_for_nth("number to change", 4);
    let text = s.text();
    let listing = &text[text.rfind("  values").unwrap()..];
    assert!(listing.contains(r#"["a b","c"]"#), "{listing}");
    assert!(listing.contains("absent"), "{listing}");
    s.send("\r");
    s.wait_for_nth("Continue?", 2);
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    let text = s.text();
    assert!(text.contains("2 args: a b c"), "{text}");
    assert!(text.contains("one:a b") && text.contains("one:c"), "{text}");
    assert!(text.contains("1 args: []"), "{text}");
    assert!(!events(dir.path()).contains("tag is x"), "{text}");
}

// ---- skipping ----

/// Presses one key at a single-key prompt, after its arming delay.
fn key(s: &mut Session, key: &str) {
    std::thread::sleep(Duration::from_millis(400));
    s.send(key);
}

const SKIP_WIZARD: &str = concat!(
    "stage \"one\"\n  read true\n",
    "stage \"make\"\n  read make-id\n    capture id = text\n",
    "stage \"use\"\n  read use {id}\n",
    "stage \"last\"\n  read true\n",
);

fn skip_setup(body: &str) -> tempfile::TempDir {
    setup(
        body,
        &[
            ("make-id", "echo id-7"),
            ("use", "echo \"using $*\""),
            ("mark", "touch \"$PWD/marked-$1\""),
        ],
    )
}

#[test]
fn s_at_a_gate_skips_the_stage_and_its_dependents() {
    let dir = skip_setup(SKIP_WIZARD);
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    s.wait_for("Run stage 2? [y/N · s skip");
    key(&mut s, "s");
    s.wait_for("also skips:");
    assert!(
        s.text().contains("stage 3 \"use\"  reads `id` (2.1)"),
        "{}",
        s.text()
    );
    s.wait_for("a skip all · v set values by hand · Esc back");
    key(&mut s, "a");
    s.wait_for("Run stage 4?");
    let text = s.text();
    assert!(
        text.contains("↷ skipped: reads `id` from stage 2"),
        "{text}"
    );
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(!s.text().contains("using"), "{}", s.text());
    let events = events(dir.path());
    assert!(
        events.contains(r#""by":"operator","kind":"stage_skip","needs":[],"stage":2"#),
        "{events}"
    );
    assert!(
        events.contains(r#""by":"dependency","kind":"stage_skip","needs":["id"],"stage":3"#),
        "{events}"
    );
}

#[test]
fn a_value_set_by_hand_keeps_the_dependent_and_esc_throws_it_away() {
    let dir = skip_setup(SKIP_WIZARD);
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    s.answer("y");
    s.wait_for("Run stage 2?");
    // First set a value, then go back: nothing changes.
    key(&mut s, "s");
    s.wait_for("v set values by hand");
    key(&mut s, "v");
    s.wait_for("number to set");
    s.send("1\r");
    s.wait_for("? id:");
    s.send("thrown-away\r");
    s.wait_for_nth("number to set", 2);
    s.send("\r");
    s.wait_for("now skips nothing else");
    key(&mut s, "\x1b");
    s.wait_for_nth("Run stage 2?", 2);
    assert!(!events(dir.path()).contains("value_set"));
    // Then set it, and skip.
    key(&mut s, "s");
    s.wait_for_nth("v set values by hand", 2);
    key(&mut s, "v");
    s.wait_for_nth("number to set", 3);
    s.send("1\r");
    s.wait_for_nth("? id:", 2);
    s.send("hand-9\r");
    s.wait_for_nth("number to set", 4);
    s.send("\r");
    s.wait_for_nth("now skips nothing else", 2);
    key(&mut s, "s");
    s.wait_for("Run stage 3?");
    s.answer("y");
    s.wait_for("Run stage 4?");
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(s.text().contains("using hand-9"), "{}", s.text());
    assert!(!s.text().contains("using thrown-away"), "{}", s.text());
    let events = events(dir.path());
    assert_eq!(events.matches("value_set").count(), 1, "{events}");
}

#[test]
fn s_at_a_pause_and_at_a_confirm() {
    let dir = skip_setup(concat!(
        "gates mutating\n",
        "stage \"look\"\n  read mark look\n",
        "stage \"change\"\n  mutate mark a\n  confirm \"Go on?\"\n  mutate mark b\n",
        "stage \"after\"\n  read mark after\n",
    ));
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("↵ continue · s skip");
    key(&mut s, "s");
    s.wait_for("Run stage 2?");
    assert!(s.text().contains("↷ skipped"), "{}", s.text());
    s.answer("y");
    s.wait_for("Go on? [y/N · s skip");
    key(&mut s, "s");
    s.wait_for("the rest of stage 2 was skipped");
    s.wait_for_nth("↵ continue", 2);
    s.answer("");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(!dir.path().join("marked-look").exists());
    assert!(dir.path().join("marked-a").exists());
    assert!(!dir.path().join("marked-b").exists());
    assert!(dir.path().join("marked-after").exists());
    assert!(events(dir.path()).contains(r#""from_step":"2.2""#));
}

#[test]
fn skipping_a_skip_ends_stage_asks_and_ends_the_run() {
    let dir = skip_setup(concat!(
        "stage \"cutover\"\n  skip ends\n  mutate mark cutover\n",
        "stage \"after\"\n  mutate mark after\n",
    ));
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1?");
    key(&mut s, "s");
    s.wait_for("Skipping stage 1 ends the run. End now?");
    key(&mut s, "y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(
        s.text().contains("ended: stage 1 was skipped"),
        "{}",
        s.text()
    );
    assert!(!dir.path().join("marked-after").exists());
}

#[test]
fn a_skip_never_stage_offers_no_skip() {
    let dir = skip_setup("stage \"a\"\n  skip never\n  read true\n");
    let mut s = start(dir.path(), &["run", "w.korin"]);
    s.wait_for("Run stage 1? [y/N · v values");
    key(&mut s, "s");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!s.text().contains("skipped"), "{}", s.text());
    s.send("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
}

const ASK_WIZARD: &str = concat!(
    "stage \"make\"\n  read make-id\n    capture id = text\n",
    "stage \"asks\"\n  ask name \"Name?\"\n  read use {id}\n",
    "stage \"last\"\n  read use {name}\n",
);

#[test]
fn esc_leaves_answers_from_param_unstored() {
    let dir = skip_setup(ASK_WIZARD);
    let mut s = start(dir.path(), &["run", "w.korin", "--param", "name=ada"]);
    s.wait_for("Run stage 1?");
    key(&mut s, "s");
    s.wait_for("also skips:");
    key(&mut s, "\x1b");
    s.wait_for_nth("Run stage 1?", 2);
    assert!(!events(dir.path()).contains("value_set"));
    s.answer("y");
    s.wait_for("Run stage 2?");
    s.answer("y");
    s.wait_for("Run stage 3?");
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(s.text().contains("using ada"), "{}", s.text());
    assert!(!events(dir.path()).contains("value_set"));
}

#[test]
fn a_revived_stage_runs_its_ask_as_usual() {
    for supplied in [true, false] {
        let dir = skip_setup(ASK_WIZARD);
        let mut args = vec!["run", "w.korin"];
        if supplied {
            args.extend(["--param", "name=ada"]);
        }
        let mut s = start(dir.path(), &args);
        s.wait_for("Run stage 1?");
        key(&mut s, "s");
        s.wait_for("v set values by hand");
        key(&mut s, "v");
        s.wait_for("number to set");
        let text = s.text();
        let listing = &text[text.rfind("values to set by hand").unwrap()..];
        let number = listing
            .lines()
            .find(|l| l.split_whitespace().nth(1) == Some("id"))
            .and_then(|l| l.split_whitespace().next())
            .unwrap_or_else(|| panic!("{listing}"))
            .to_string();
        s.send(&format!("{number}\r"));
        s.wait_for("? id:");
        s.send("hand-1\r");
        s.wait_for_nth("number to set", 2);
        s.send("\r");
        s.wait_for("now skips nothing else");
        key(&mut s, "s");
        s.wait_for("Run stage 2?");
        s.answer("y");
        if !supplied {
            s.wait_for("Name?");
            s.wait_for("? name:");
            s.send("bo\r");
        }
        s.wait_for("Run stage 3?");
        s.answer("y");
        assert_eq!(s.wait(), 0, "{supplied}: {}", s.text());
        let who = if supplied { "ada" } else { "bo" };
        assert!(s.text().contains(&format!("using {who}")), "{}", s.text());
        assert!(s.text().contains("using hand-1"), "{}", s.text());
        let events = events(dir.path());
        assert_eq!(
            events.matches("value_set").count(),
            1,
            "{supplied}: {events}"
        );
        assert!(!events.contains(r#""by":"--param""#), "{events}");
    }
}

#[test]
fn from_in_a_terminal_asks_for_values_after_the_preflight() {
    let dir = skip_setup(&format!("preflight\n  read mark preflight\n{SKIP_WIZARD}"));
    let mut s = start(dir.path(), &["run", "w.korin", "--from", "use"]);
    s.wait_for("Run the preflight?");
    s.answer("y");
    s.wait_for("Skipping the stages before the run also skips:");
    assert!(dir.path().join("marked-preflight").exists());
    s.wait_for("v set values by hand · Esc stop");
    key(&mut s, "v");
    s.wait_for("number to set");
    s.send("1\r");
    s.wait_for("? id:");
    s.send("old-3\r");
    s.wait_for_nth("number to set", 2);
    s.send("\r");
    s.wait_for("now skips nothing else");
    key(&mut s, "s");
    s.wait_for("Run stage 3?");
    s.answer("y");
    s.wait_for("Run stage 4?");
    s.answer("y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    assert!(s.text().contains("using old-3"), "{}", s.text());
}

// ---- scripts ----

const TOKEN: &str = "tok-4815162342";

/// A wizard with two scripts beside it, and a fake pager that copies what it is given.
fn script_setup(body: &str) -> tempfile::TempDir {
    let dir = setup(
        &format!("param token secret\n{body}"),
        &[(
            "fake-pager",
            "cat \"$1\" >> \"$(dirname \"$0\")/../paged.txt\"; echo --- >> \"$(dirname \"$0\")/../paged.txt\"",
        )],
    );
    for (name, text) in [
        (
            "short.sh",
            format!("#!/bin/sh\necho short\n# token {TOKEN}\n"),
        ),
        (
            "long.sh",
            "#!/bin/sh\necho 1\necho 2\necho 3\necho 4\n".to_string(),
        ),
    ] {
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

fn script_env(dir: &Path, pager: &str) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("KORIN_SECRET_TOKEN", PathBuf::from(TOKEN)),
        ("PAGER", dir.join("bin").join(pager)),
    ]
}

#[test]
fn a_short_script_shows_in_full_and_a_long_one_opens_in_the_pager() {
    let dir = script_setup("stage \"a\"\n  script read ./short.sh\n  script read ./long.sh\n");
    let mut s = start_with_env(
        dir.path(),
        &["run", "w.korin", "--script-lines", "3"],
        &script_env(dir.path(), "fake-pager"),
    );
    s.wait_for("o open script]");
    let text = s.text();
    assert!(text.contains("│ echo short"), "{text}");
    assert!(text.contains("│ # token «secret:token»"), "{text}");
    assert!(text.contains("│ 5 lines · o to open"), "{text}");
    assert!(!text.contains(TOKEN), "{text}");
    // Two scripts: `o` lists them, and the pager gets a redacted copy of the one picked.
    key(&mut s, "o");
    s.wait_for("script to open");
    s.send("1\r");
    s.wait_for_nth("o open script]", 2);
    key(&mut s, "y");
    assert_eq!(s.wait(), 0, "{}", s.text());
    let paged = std::fs::read_to_string(dir.path().join("paged.txt")).unwrap();
    assert!(paged.contains("echo short"), "{paged}");
    assert!(
        paged.contains("«secret:token»") && !paged.contains(TOKEN),
        "{paged}"
    );
    let events = events(dir.path());
    assert!(events.contains("script_sha256"), "{events}");
}

#[test]
fn without_a_pager_the_script_prints_redacted() {
    let dir = script_setup("stage \"a\"\n  script read ./short.sh\n");
    let mut s = start_with_env(
        dir.path(),
        &["run", "w.korin", "--script-lines", "0"],
        &script_env(dir.path(), "no-such-pager"),
    );
    s.wait_for("o open script]");
    assert!(s.text().contains("│ 3 lines · o to open"), "{}", s.text());
    key(&mut s, "o");
    s.wait_for_nth("o open script]", 2);
    let text = s.text();
    assert!(text.contains("│ # token «secret:token»"), "{text}");
    assert!(!text.contains(TOKEN), "{text}");
    key(&mut s, "y");
    assert_eq!(s.wait(), 0, "{}", s.text());
}

#[test]
fn a_script_changed_after_the_gate_does_not_run() {
    let dir = script_setup(concat!(
        "stage \"a\"\n",
        "  mutate sh -c \"echo 'echo changed' >> short.sh\"\n",
        "  confirm \"Go on?\"\n",
        "  script read ./short.sh\n",
    ));
    let mut s = start_with_env(
        dir.path(),
        &["run", "w.korin"],
        &script_env(dir.path(), "fake-pager"),
    );
    s.wait_for("Run stage 1?");
    s.answer("y");
    s.wait_for("Go on? [y/N");
    let text = s.text();
    let confirm = &text[text.rfind("next:").unwrap()..];
    // The confirm shows the pinned text, and says the file changed.
    assert!(confirm.contains("│ echo short"), "{confirm}");
    assert!(!confirm.contains("echo changed"), "{confirm}");
    assert!(
        confirm.contains("changed on disk since the gate"),
        "{confirm}"
    );
    s.answer("y");
    assert_eq!(s.wait(), 1, "{}", s.text());
    assert!(
        s.text()
            .contains("changed after the gate; nothing in step 1.3 ran"),
        "{}",
        s.text()
    );
    assert!(!s.text().contains("\n      │ short"), "{}", s.text());
}
