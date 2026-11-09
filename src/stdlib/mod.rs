//! Modules built into korin, loaded with `use <name>`.

/// The names of every built-in module.
pub const NAMES: &[&str] = &["aws", "rds"];

/// The source of a built-in module.
pub fn source(name: &str) -> Option<&'static str> {
    match name {
        "aws" => Some(include_str!("aws.korin")),
        "rds" => Some(include_str!("rds.korin")),
        _ => None,
    }
}

/// A human-readable list of every built-in template, for `korin modules`.
pub fn listing() -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for name in NAMES {
        let file = crate::diag::SourceFile::new(
            format!("<built-in>/{name}.korin"),
            *name,
            source(name).unwrap_or_default(),
        );
        let Ok(module) = crate::parser::parse_module(&file) else {
            continue;
        };
        let _ = writeln!(out, "use {name}");
        for define in &module.defines {
            let params: Vec<&str> = define.params.iter().map(|p| p.value.as_str()).collect();
            let _ = write!(out, "  {name}.{}({})", define.name.value, params.join(", "));
            if !define.exports.is_empty() {
                let exports: Vec<&str> = define.exports.iter().map(|e| e.value.as_str()).collect();
                let _ = write!(out, " as <alias>  → {}", exports.join(", "));
            }
            out.push('\n');
        }
        out.push('\n');
    }
    out
}
