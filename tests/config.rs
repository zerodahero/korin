//! The user's config file, through `korin config` and `korin run`.

use std::path::Path;
use std::process::{Command, Output};

fn korin(config: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_korin"));
    cmd.args(args)
        .env("KORIN_CONFIG", config)
        .env_remove("KORIN_REQUIRE_ENTER")
        .env("NO_COLOR", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn with_config(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.kdl");
    std::fs::write(&path, text).unwrap();
    (dir, path)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The `key value source` line for one key.
fn line<'a>(text: &'a str, key: &str) -> Vec<&'a str> {
    text.lines()
        .find(|l| l.split_whitespace().next() == Some(key))
        .unwrap_or_else(|| panic!("no `{key}` line in:\n{text}"))
        .split_whitespace()
        .collect()
}

#[test]
fn a_missing_file_gives_the_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("none.kdl");
    let out = korin(&path, &["config"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.lines().next().unwrap().ends_with("(not found)"),
        "{text}"
    );
    assert_eq!(line(&text, "view"), ["view", "compact", "default"]);
    assert_eq!(
        line(&text, "require-enter"),
        ["require-enter", "#false", "default"]
    );
    assert_eq!(line(&text, "color"), ["color", "auto", "default"]);
}

#[test]
fn the_file_sets_each_key() {
    let (_dir, path) =
        with_config("// my preferences\nview full\nrequire-enter #true\ncolor never\n");
    let out = korin(&path, &["config"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains("not found"), "{text}");
    assert_eq!(line(&text, "view"), ["view", "full", "file"]);
    assert_eq!(
        line(&text, "require-enter"),
        ["require-enter", "#true", "file"]
    );
    assert_eq!(line(&text, "color"), ["color", "never", "file"]);
}

#[test]
fn a_flag_wins_over_the_env_and_the_env_over_the_file() {
    let (_dir, path) = with_config("require-enter #false\ncolor never\n");
    let out = korin(&path, &["config"], &[("KORIN_REQUIRE_ENTER", "1")]);
    assert_eq!(
        line(&stdout(&out), "require-enter"),
        ["require-enter", "#true", "env"]
    );
    let out = korin(&path, &["--color", "always", "config"], &[]);
    assert_eq!(line(&stdout(&out), "color"), ["color", "always", "flag"]);
    // `0` in the environment turns the key off, even when the file turns it on.
    let (_dir, path) = with_config("require-enter #true\n");
    let out = korin(&path, &["config"], &[("KORIN_REQUIRE_ENTER", "0")]);
    assert_eq!(
        line(&stdout(&out), "require-enter"),
        ["require-enter", "#false", "env"]
    );
}

#[test]
fn errors_name_the_key_and_stop_before_anything_runs() {
    let cases = [
        ("pager-ish \"x\"\n", "unknown config key `pager-ish`"),
        ("view full\nview compact\n", "`view` appears more than once"),
        ("require-enter yes\n", "`require-enter` cannot be"),
        ("view wide\n", "`view` cannot be"),
        ("color\n", "`color` takes exactly one value"),
        ("view {\n  full\n}\n", "`view` takes no children"),
        ("view \"unclosed\n", "config"),
    ];
    for (text, expected) in cases {
        let (dir, path) = with_config(text);
        // A wizard that would leave a mark if it ran.
        let wizard = dir.path().join("w.korin");
        std::fs::write(
            &wizard,
            "korin 1\ntitle \"t\"\nunattended allowed\nstage \"a\"\n  mutate touch marked\n",
        )
        .unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_korin"))
            .args(["run", "w.korin", "--yes"])
            .current_dir(dir.path())
            .env("KORIN_CONFIG", &path)
            .env("NO_COLOR", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{text:?} was accepted");
        assert!(
            stderr(&out).contains(expected),
            "{text:?}: expected {expected:?} in:\n{}",
            stderr(&out)
        );
        assert!(stderr(&out).contains("config.kdl"), "{}", stderr(&out));
        assert!(
            !dir.path().join("marked").exists(),
            "{text:?}: the wizard ran"
        );
    }
}

#[test]
fn the_default_path_follows_xdg_config_home() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("xdg/korin")).unwrap();
    std::fs::write(dir.path().join("xdg/korin/config.kdl"), "view full\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_korin"))
        .arg("config")
        .env_remove("KORIN_CONFIG")
        .env("XDG_CONFIG_HOME", dir.path().join("xdg"))
        .output()
        .unwrap();
    let text = stdout(&out);
    assert!(text.contains("xdg/korin/config.kdl"), "{text}");
    assert_eq!(line(&text, "view"), ["view", "full", "file"]);
}

#[test]
fn every_docs_example_config_is_valid() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs-site/examples/reference/config");
    let mut count = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "kdl") {
            let out = korin(&path, &["config"], &[]);
            assert!(out.status.success(), "{}: {}", path.display(), stderr(&out));
            count += 1;
        }
    }
    assert_ne!(count, 0);
}
