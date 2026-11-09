//! Tests for `scripts/denylist-check.sh`, the hook that keeps private names out of the repo.

use std::path::Path;
use std::process::Command;

struct Out {
    code: i32,
    stderr: String,
}

fn run(list: Option<&str>, files: &[(&str, &str)], extra_args: &[&str]) -> Out {
    let dir = tempfile::tempdir().unwrap();
    let mut args: Vec<String> = Vec::new();
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).unwrap();
        args.push(dir.path().join(name).display().to_string());
    }
    args.extend(extra_args.iter().map(|a| (*a).to_string()));
    let list_path = dir.path().join("denylist");
    if let Some(list) = list {
        std::fs::write(&list_path, list).unwrap();
    }
    let out = Command::new(Path::new("scripts/denylist-check.sh"))
        .args(&args)
        .env("KORIN_DENYLIST", &list_path)
        .output()
        .unwrap();
    Out {
        code: out.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn a_match_fails_without_echoing_the_pattern_or_line() {
    let out = run(
        Some("# comment\nsecretcorp\n"),
        &[("a.md", "fine\nworks at SecretCorp now\n")],
        &[],
    );
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("a.md:2"), "{}", out.stderr);
    assert!(
        !out.stderr.to_lowercase().contains("secretcorp"),
        "{}",
        out.stderr
    );
}

#[test]
fn no_match_passes() {
    let out = run(Some("secretcorp\n"), &[("a.md", "nothing private\n")], &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
}

#[test]
fn an_absent_list_warns_and_passes() {
    let out = run(None, &[("a.md", "anything\n")], &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stderr.contains("not found; skipping"), "{}", out.stderr);
}

#[test]
fn an_empty_list_passes() {
    let out = run(Some("# only comments\n\n"), &[("a.md", "anything\n")], &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
}

#[test]
fn an_invalid_pattern_fails_closed() {
    let out = run(Some("(unclosed\n"), &[("a.md", "anything\n")], &[]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("grep failed"), "{}", out.stderr);
}

#[test]
fn an_unreadable_input_fails_closed() {
    let out = run(
        Some("secretcorp\n"),
        &[("a.md", "fine\n")],
        &["/nonexistent/file.md"],
    );
    assert_eq!(out.code, 2, "{}", out.stderr);
}
