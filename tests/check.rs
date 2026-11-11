//! `korin check` rules. Each test compiles a small wizard and snapshots its diagnostics.

use std::path::Path;

use korin::compile::compile_path;
use korin::diag::Diag;

/// Writes `files` into a temp dir and compiles `main.korin`. Returns one line per diagnostic.
fn check(files: &[(&str, &str)]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    for (name, text) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let result = compile_path(&root.join("main.korin"));
    let diags = match &result {
        Ok((_, warnings)) => warnings.clone(),
        Err(diags) => diags.clone(),
    };
    let prefix = format!("{}/", root.display());
    let mut lines: Vec<String> = diags
        .iter()
        .map(|d: &Diag| {
            let kind = if d.is_error() { "error" } else { "warning" };
            format!("{kind} {}", d.short().replace(&prefix, ""))
        })
        .collect();
    if lines.is_empty() {
        lines.push("ok".into());
    }
    lines.join("\n")
}

fn wizard(body: &str) -> String {
    check(&[("main.korin", &format!("korin 1\ntitle \"t\"\n{body}"))])
}

fn from(body: &str, stage: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.korin");
    std::fs::write(&path, format!("korin 1\ntitle \"t\"\n{body}")).unwrap();
    let (program, _) = compile_path(&path).unwrap();
    match korin::run::resolve_from(&program, stage) {
        Ok(n) => format!("ok {n}"),
        Err(diag) => diag
            .short()
            .replace(&format!("{}/", dir.path().display()), ""),
    }
}

#[test]
fn intro_example_is_valid() {
    let (program, warnings) = compile_path(Path::new("examples/intro/intro.korin")).unwrap();
    assert!(warnings.is_empty());
    insta::assert_snapshot!(korin::render::plan(&program, korin::render::DEFAULT_WIDTH));
}

#[test]
fn guarded_expect_captures() {
    insta::assert_snapshot!(wizard(concat!(
        "use aws\n",
        "stage \"a\"\n",
        "  aws.ssm_get(/x) as prev\n",
        "  note \"unguarded {prev.value}\"\n",
        "  when not prev.missing\n",
        "    note \"guarded {prev.value}\"\n",
        "  when prev.missing\n",
        "    note \"wrong guard {prev.value}\"\n",
        "  assert not prev.missing and prev.value == \"x\" else \"short-circuit guard\"\n",
    )));
}

#[test]
fn on_expect_and_optional_captures() {
    insta::assert_snapshot!(wizard(concat!(
        "stage \"a\"\n",
        "  read tool\n",
        "    expect gone = exit 3\n",
        "    capture why = text on gone\n",
        "    capture maybe? = jq .x\n",
        "  note \"{why}\"\n",
        "  when gone\n",
        "    note \"{why}\"\n",
        "  when not gone\n",
        "    note \"{maybe}\"\n",
        "    when exists maybe\n",
        "      note \"{maybe}\"\n",
    )));
}

#[test]
fn type_rules() {
    insta::assert_snapshot!(wizard(concat!(
        "param p required\n",
        "stage \"a\"\n",
        "  read tool\n",
        "    capture xs = jq list .[]\n",
        "    capture doc = jq json .\n",
        "    expect o = exit 1\n",
        "  when not o\n",
        "    read tool {xs}\n",
        "    read tool {xs...}\n",
        "    read tool --ids={xs...}\n",
        "    read tool {p...}\n",
        "    note \"{xs}\"\n",
        "    read tool {o}\n",
        "    when p\n",
        "      note \"x\"\n",
        "    each x in p\n",
        "      note \"x\"\n",
        "    each x in xs\n",
        "      read tool {x} {doc}\n",
        "    assert xs == \"a\" else \"lists do not compare\"\n",
        "    assert exists p else \"p is not optional\"\n",
        "    read tool {doc.field}\n",
    )));
}

#[test]
fn names_scopes_and_redefinition() {
    insta::assert_snapshot!(wizard(concat!(
        "param p required\n",
        "stage \"a\"\n",
        "  read tool {nope}\n",
        "  read tool\n",
        "    capture p = text\n",
        "  each f in [\"a\", \"b\"]\n",
        "    read tool {f}\n",
        "      capture inner = text\n",
        "  note \"{inner}\"\n",
        "  note \"{korin.out}\"\n",
        "stage \"a\"\n",
        "  note \"duplicate title\"\n",
    )));
}

#[test]
fn secrets() {
    insta::assert_snapshot!(wizard(concat!(
        "param pw secret\n",
        "stage \"a\"\n",
        "  read tool\n",
        "    env PGPASSWORD={pw}\n",
        "    capture secret token = jq .token\n",
        "    capture other = jq .other\n",
        "    capture secret f = file ./out.json text\n",
        "    capture secret g = file {korin.out}/out.json text\n",
        "  note \"{token}\"\n",
        "  confirm \"use {pw}?\"\n",
        "  interactive nvim {pw}\n",
        "  read tool {other}\n",
    )));
}

#[test]
fn executables_and_allow() {
    insta::assert_snapshot!(check(&[
        (
            "main.korin",
            concat!(
                "korin 1\ntitle \"t\"\n",
                "allow git, ./scripts/ok.sh\n",
                "use \"./lib/m.korin\"\n",
                "param tool required\n",
                "stage \"a\"\n",
                "  read git status\n",
                "  read aws sts get-caller-identity\n",
                "  read {tool} --version\n",
                "  script read ./scripts/ok.sh\n",
                "  script read ./scripts/other.sh\n",
                "  m.run(git)\n",
                "  m.run(rm)\n",
            ),
        ),
        ("lib/m.korin", "define run(exe)\n  read {exe} --version\n"),
    ]));
}

#[test]
fn unattended_and_interactive_rules() {
    insta::assert_snapshot!(wizard(concat!(
        "unattended allowed\n",
        "stage \"a\"\n",
        "  confirm \"go?\"\n",
        "  interactive nvim x\n",
        "    timeout 5m\n",
    )));
}

#[test]
fn templates_hygiene_and_errors() {
    insta::assert_snapshot!(check(&[
        (
            "main.korin",
            concat!(
                "korin 1\ntitle \"t\"\n",
                "use rds\n",
                "use \"./lib/m.korin\"\n",
                "stage \"a\"\n",
                "  rds.cluster(one) as src\n",
                "  rds.cluster(two) as dst\n",
                "  assert src.status == dst.status else \"{src.status} vs {dst.status}\"\n",
                "  rds.cluster(three) as src\n",
                "  rds.cluster(four)\n",
                "  rds.nope(x)\n",
                "  rds.cluster(a, b) as c\n",
                "  nope.x()\n",
                "  note \"{src.private}\"\n",
                "  m.loop()\n",
                "  m.bad_export() as b\n",
            ),
        ),
        (
            "lib/m.korin",
            concat!(
                "define loop()\n  m2.back()\n",
                "define bad_export()\n  exports ghost\n  read tool\n",
                "use \"./m2.korin\"\n",
            ),
        ),
        (
            "lib/m2.korin",
            "use \"./m.korin\"\ndefine back()\n  m.loop()\n"
        ),
    ]));
}

#[test]
fn two_calls_do_not_collide() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.korin");
    std::fs::write(
        &path,
        "korin 1\ntitle \"t\"\nuse rds\nstage \"a\"\n  rds.cluster(one) as src\n  rds.cluster(two) as dst\n",
    )
    .unwrap();
    let (program, _) = compile_path(&path).unwrap();
    let names: Vec<&str> = program.syms.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"src.status") && names.contains(&"dst.status"),
        "{names:?}"
    );
    let src = names.iter().position(|n| *n == "src.status").unwrap();
    let dst = names.iter().position(|n| *n == "dst.status").unwrap();
    assert_ne!(src, dst);
}

#[test]
fn from_rejects_every_runtime_binding() {
    let expect_only = concat!(
        "stage \"probe\"\n",
        "  read tool\n",
        "    expect missing = exit 1\n",
        "stage \"fix\"\n",
        "  when missing\n",
        "    mutate tool create\n",
    );
    let ask_only = concat!(
        "stage \"ask\"\n",
        "  ask name \"Name?\"\n",
        "stage \"use\"\n",
        "  mutate tool {name}\n",
    );
    let exported_expect = concat!(
        "use aws\n",
        "stage \"probe\"\n",
        "  aws.ssm_get(/x) as prev\n",
        "stage \"use\"\n",
        "  when prev.missing\n",
        "    mutate tool create\n",
    );
    let params_only = concat!(
        "param p required\n",
        "stage \"one\"\n",
        "  read tool {p}\n",
        "stage \"two\"\n",
        "  mutate tool {p}\n",
    );
    insta::assert_snapshot!(
        [
            from(expect_only, "fix"),
            from(ask_only, "2"),
            from(exported_expect, "use"),
            from(params_only, "two"),
            from(params_only, "nope"),
        ]
        .join("\n")
    );
}

#[test]
fn bad_jq_and_regex() {
    insta::assert_snapshot!(wizard(concat!(
        "stage \"a\"\n",
        "  read tool\n",
        "    capture a = jq .a |\n",
        "    capture b = regex /no group/\n",
        "    capture c = regex /(unclosed/\n",
        "    expect d = stderr ~ /[/\n",
    )));
}

#[test]
fn review_fix_rules() {
    insta::assert_snapshot!(check(&[
        (
            "main.korin",
            concat!(
                "korin 1\ntitle \"t\"\n",
                "use \"./lib/m.korin\"\n",
                "param pw secret\n",
                "param conn = \"postgres://app:{pw}@db\"\n",
                "stage \"a\"\n",
                "  interactive psql {conn}\n",
                "  poll every 1s for 5s\n",
                "    mutate tool restart\n",
                "    confirm \"again?\"\n",
                "    read tool status\n",
                "      capture s = text\n",
                "    until s == \"ok\"\n",
                "  read tool\n",
                "    capture secret t = file {korin.out}/../escape text\n",
                "  m.show({korin.out})\n",
            ),
        ),
        ("lib/m.korin", "define show(p)\n  note \"path {p}\"\n"),
    ]));
}

#[test]
fn interactive_inside_when_makes_a_stage_mutating() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.korin");
    std::fs::write(
        &path,
        concat!(
            "korin 1\ntitle \"t\"\ngates mutating\n",
            "stage \"a\"\n",
            "  read tool\n",
            "    expect missing = exit 1\n",
            "  when missing\n",
            "    interactive nvim notes.md\n",
            "stage \"b\"\n",
            "  read tool\n",
        ),
    )
    .unwrap();
    let (program, _) = compile_path(&path).unwrap();
    assert!(program.stages[0].mutating);
    assert!(!program.stages[1].mutating);
    let plan = korin::render::plan(&program, korin::render::DEFAULT_WIDTH);
    let tags: Vec<&str> = plan
        .lines()
        .skip_while(|l| !l.starts_with("━━ Stage"))
        .filter(|l| l.ends_with("from start"))
        .collect();
    assert!(tags[0].starts_with("  mutating, gate;"), "{plan}");
    assert!(tags[1].starts_with("  read-only, no gate;"), "{plan}");
}

#[test]
fn secret_file_captures_stay_inside_the_scratch_directory() {
    insta::assert_snapshot!(wizard(concat!(
        "param name required\n",
        "stage \"a\"\n",
        "  read tool\n",
        "    capture secret a = file {korin.out}-outside text\n",
        "    capture secret b = file {korin.out}/{name} text\n",
        "    capture secret c = file {korin.out}/ text\n",
        "    capture secret d = file {korin.out}/ok.txt text\n",
    )));
}

#[test]
fn rendered_conditions_keep_their_grouping() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.korin");
    std::fs::write(
        &path,
        concat!(
            "korin 1\ntitle \"t\"\n",
            "stage \"a\"\n",
            "  read tool\n",
            "    expect a = exit 1\n",
            "    expect b = exit 2\n",
            "  when not (a or b)\n",
            "    note \"x\"\n",
            "  when (a or b) and not a\n",
            "    note \"y\"\n",
        ),
    )
    .unwrap();
    let (program, _) = compile_path(&path).unwrap();
    let plan = korin::render::plan(&program, korin::render::DEFAULT_WIDTH);
    assert!(plan.contains("when not (a or b)"), "{plan}");
    assert!(plan.contains("when (a or b) and not a"), "{plan}");
}

#[test]
fn a_guide_only_reads() {
    insta::assert_snapshot!(check(&[
        (
            "main.korin",
            concat!(
                "korin 1\ntitle \"t\"\n",
                "mode guide\n",
                "use \"./lib/m.korin\"\n",
                "stage \"a\"\n",
                "  read git status\n",
                "  script read ./scripts/look.sh\n",
                "  interactive nvim notes.md\n",
                "  note \"looked\"\n",
                "  mutate git push\n",
                "  each f in [\"a\"]\n",
                "    script mutate ./scripts/fix.sh\n",
                "  m.look()\n",
                "  m.change()\n",
            ),
        ),
        (
            "lib/m.korin",
            concat!(
                "define look()\n  read git log\n",
                "define change()\n  read git status\n  mutate git commit\n",
            ),
        ),
    ]));
}

#[test]
fn a_guide_has_no_gates_and_mode_takes_two_words() {
    insta::assert_snapshot!(wizard(concat!(
        "mode guide\n",
        "gates mutating\n",
        "stage \"a\"\n",
        "  read git status\n",
    )));
    insta::assert_snapshot!(wizard(concat!(
        "mode tour\n",
        "stage \"a\"\n",
        "  read git status\n",
    )));
    assert_eq!(
        wizard("mode stepwise\nstage \"a\"\n  mutate git push\n"),
        "ok"
    );
}
