//! Parser tests: successful parses snapshot the AST, failures snapshot every diagnostic.

use korin::diag::SourceFile;
use korin::parser::{parse_module, parse_wizard};

fn wizard(src: &str) -> String {
    let file = SourceFile::new("test.korin", "test.korin", src);
    match parse_wizard(&file) {
        Ok(wizard) => format!("{wizard:#?}"),
        Err(diags) => diags
            .iter()
            .map(korin::diag::Diag::short)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn errors(src: &str) -> String {
    let file = SourceFile::new("test.korin", "test.korin", src);
    match parse_wizard(&file) {
        Ok(_) => panic!("expected parse errors"),
        Err(diags) => diags
            .iter()
            .map(korin::diag::Diag::short)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

const HEADER: &str = "korin 1\ntitle \"t\"\n";

fn with_header(body: &str) -> String {
    format!("{HEADER}{body}")
}

#[test]
fn intro_example_parses() {
    let src = std::fs::read_to_string("examples/intro/intro.korin").unwrap();
    insta::assert_snapshot!(wizard(&src));
}

#[test]
fn intro_example_matches_docs() {
    let example = std::fs::read_to_string("examples/intro/intro.korin").unwrap();
    let docs = std::fs::read_to_string("docs/language.md").unwrap();
    let start = docs.find("<!-- intro:start -->\n```text\n").unwrap()
        + "<!-- intro:start -->\n```text\n".len();
    let end = docs.find("```\n<!-- intro:end -->").unwrap();
    assert_eq!(
        &docs[start..end],
        example,
        "docs/language.md and examples/intro/intro.korin differ"
    );
}

#[test]
fn intro_module_parses() {
    let src = std::fs::read_to_string("examples/intro/lib/local.korin").unwrap();
    let file = SourceFile::new("local.korin", "local.korin", src);
    let module = parse_module(&file).unwrap();
    assert_eq!(module.defines.len(), 1);
}

#[test]
fn argv_words_quotes_and_interpolation() {
    insta::assert_snapshot!(wizard(&with_header(
        "stage \"s\"\n  read tool \"two words\" a{x}b {xs...} --flag=\"{y} z\" \\{literal\\}\n"
    )));
}

#[test]
fn captures_outcomes_env_timeout() {
    insta::assert_snapshot!(wizard(&with_header(concat!(
        "stage \"s\"\n",
        "  read aws ssm get-parameter --name n --output json\n",
        "    expect missing = exit 254 and stderr ~ /ParameterNotFound/\n",
        "    expect gone = exit != 0\n",
        "    expect stopped = signal TERM\n",
        "    capture value = jq .Parameter.Value\n",
        "    capture secret tok? = jq .Parameter | .Token on missing\n",
        "    capture all = jq list .Items[].Id\n",
        "    capture doc = jq json .Doc\n",
        "    capture raw = text\n",
        "    capture rows = lines on gone\n",
        "    capture n = regex /(\\d+) rows/\n",
        "    capture notes = file {korin.out}/notes.md text\n",
        "    env PGPASSWORD={password}\n",
        "    timeout 90s\n",
    ))));
}

#[test]
fn control_flow() {
    insta::assert_snapshot!(wizard(&with_header(concat!(
        "stage \"s\"\n",
        "  when not a.missing and (b == \"x\" or c in [\"y\", \"z\"]) and d ~ /^ok/\n",
        "    note \"guarded {b}\"\n",
        "  each f in [\"a.rs\", \"b.rs\"]\n",
        "    interactive nvim {f}\n",
        "  poll every 30s for 2h\n",
        "    read aws rds describe-db-clusters --output json\n",
        "      capture status = jq .DBClusters[0].Status\n",
        "    until status == \"available\"\n",
        "    fail status in [\"failed\", \"deleting\"]\n",
        "  assert exists x and n >= 3 else \"bad {n}\"\n",
        "  ask secret pw \"Password?\"\n",
        "  confirm \"Go?\"\n",
        "  rds.cluster({id}-x, other) as c\n",
        "  script mutate ./fix.sh {c.arn}\n",
    ))));
}

#[test]
fn header_options() {
    insta::assert_snapshot!(wizard(concat!(
        "korin 1\n",
        "title \"t\"\n",
        "gates mutating\n",
        "unattended allowed\n",
        "allow aws, ./scripts/fix.sh\n",
        "use aws\n",
        "use \"./lib/x.korin\" as x\n",
        "param a required\n",
        "param b secret\n",
        "param c = \"{a}-c\"\n",
        "stage \"s\"\n",
        "  note \"hi\"\n",
    )));
}

#[test]
fn error_tab_indentation() {
    insta::assert_snapshot!(errors(&with_header("stage \"s\"\n\tread x\n")));
}

#[test]
fn error_inconsistent_indentation() {
    insta::assert_snapshot!(errors(&with_header("stage \"s\"\n    read x\n  read y\n")));
}

#[test]
fn error_missing_header() {
    insta::assert_snapshot!(errors("stage \"s\"\n  read x\n"));
}

#[test]
fn error_recovers_and_reports_many() {
    insta::assert_snapshot!(errors(&with_header(concat!(
        "stage \"s\"\n",
        "  frobnicate x\n",
        "  read\n",
        "  read x\n",
        "    capture = jq .x\n",
        "    retry 3\n",
        "  assert x == \"y\"\n",
        "  poll every 30s for 2h\n",
        "    read x\n",
        "  read \"unterminated\n",
        "  note \"{bad name}\"\n",
        "  capture y = text\n",
    ))));
}

#[test]
fn error_bad_durations_and_predicates() {
    insta::assert_snapshot!(errors(&with_header(concat!(
        "stage \"s\"\n",
        "  read x\n",
        "    timeout 10\n",
        "    expect a = exit 999\n",
        "    expect b = signal NOPE\n",
        "    expect c = exit != 2\n",
        "    capture d = yaml .x\n",
    ))));
}

#[test]
fn error_define_in_wizard_and_stage_in_module() {
    insta::assert_snapshot!(errors(&with_header(
        "define f()\n  read x\nstage \"s\"\n  read x\n"
    )));
    let file = SourceFile::new("m.korin", "m.korin", "stage \"s\"\n  read x\n");
    let diags = parse_module(&file).unwrap_err();
    insta::assert_snapshot!(
        diags
            .iter()
            .map(korin::diag::Diag::short)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn comments_respect_quotes() {
    let out = wizard(&with_header(
        "stage \"s\"  # trailing\n  note \"a # not a comment\" # comment\n",
    ));
    assert!(out.contains("a # not a comment"), "{out}");
}

#[test]
fn huge_durations_are_diagnostics_not_panics() {
    insta::assert_snapshot!(errors(&with_header(concat!(
        "stage \"s\"\n",
        "  read x\n",
        "    timeout 18446744073709551615h\n",
        "  read y\n",
        "    timeout 99999999999999999999999s\n",
        "  read z\n",
        "    timeout 721h\n",
        "  read w\n",
        "    timeout 0s\n",
    ))));
}
