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
    for stage in [3, 4, 6] {
        s.wait_for(&format!("Run stage {stage}?"));
        s.answer("y");
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
    s.wait_for("Continue? [y/N]");
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
    s.wait_for("Continue? [y/N]");
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
    assert!(s.text().contains("Run stage 1? [y/N] y"), "{}", s.text());
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
    assert!(s.text().contains("Run stage 2? [y/N] n"), "{}", s.text());
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
