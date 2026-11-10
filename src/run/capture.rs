//! Extracting values from command output and files, and checking whether a file is fresh.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::ast::CaptureMode;
use crate::jq::{Jq, JqError, JqValue};

/// A runtime value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Data {
    Str(String),
    List(Vec<String>),
    Bool(bool),
}

impl Data {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Data::Str(s) => serde_json::Value::String(s.clone()),
            Data::List(items) => serde_json::Value::from(items.clone()),
            Data::Bool(b) => serde_json::Value::Bool(*b),
        }
    }
}

/// The result of one capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extract {
    Value(Data),
    Absent,
    /// The capture failed, with a message that may quote the input.
    Fail(String),
}

/// Extracts a value from `input` according to `mode`. See the result-case table in
/// `docs-site/docs/reference/captures.md`.
pub fn extract(mode: &CaptureMode, input: &[u8], optional: bool) -> Extract {
    let Ok(text) = std::str::from_utf8(input) else {
        return Extract::Fail("the output is not valid UTF-8".into());
    };
    match mode {
        CaptureMode::Text => Extract::Value(Data::Str(text.trim_end().to_string())),
        CaptureMode::Lines => Extract::Value(Data::List(
            text.lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect(),
        )),
        CaptureMode::Regex(pattern) => match regex::Regex::new(pattern) {
            Err(error) => Extract::Fail(format!("invalid regex: {error}")),
            Ok(re) => match re.captures(text).and_then(|c| c.get(1)) {
                Some(m) => Extract::Value(Data::Str(m.as_str().to_string())),
                None if optional => Extract::Absent,
                None => Extract::Fail(format!("regex /{pattern}/ did not match")),
            },
        },
        CaptureMode::Jq(filter) | CaptureMode::JqList(filter) | CaptureMode::JqJson(filter) => {
            let jq = match Jq::compile(filter) {
                Ok(jq) => jq,
                Err(error) => return Extract::Fail(error),
            };
            let results = match jq.run(text) {
                Ok(results) => results,
                Err(JqError::NotJson(_)) => {
                    return Extract::Fail(format!(
                        "the output is not JSON, so `{filter}` cannot run"
                    ));
                }
                Err(JqError::Runtime(error)) => {
                    return Extract::Fail(format!("`{filter}` raised an error: {error}"));
                }
            };
            jq_result(mode, filter, results, optional)
        }
    }
}

fn jq_result(mode: &CaptureMode, filter: &str, results: Vec<JqValue>, optional: bool) -> Extract {
    if let CaptureMode::JqList(_) = mode {
        let mut items = Vec::with_capacity(results.len());
        for (index, value) in results.into_iter().enumerate() {
            match value {
                JqValue::Scalar { text, .. } => items.push(text),
                JqValue::Null => {
                    return Extract::Fail(format!("`{filter}` yielded null at item {}", index + 1));
                }
                JqValue::Compound(_) => {
                    return Extract::Fail(format!(
                        "`{filter}` yielded an object or array at item {}; list items must be scalars",
                        index + 1
                    ));
                }
            }
        }
        return Extract::Value(Data::List(items));
    }
    match results.len() {
        0 if optional => return Extract::Absent,
        0 => return Extract::Fail(format!("`{filter}` yielded no results")),
        1 => {}
        n => {
            return Extract::Fail(format!(
                "`{filter}` yielded {n} results; this capture needs exactly one"
            ));
        }
    }
    let value = results.into_iter().next().expect("one result");
    match (mode, value) {
        (_, JqValue::Null) if optional => Extract::Absent,
        (_, JqValue::Null) => Extract::Fail(format!("`{filter}` yielded null")),
        (CaptureMode::JqJson(_), value) => Extract::Value(Data::Str(value.json_text())),
        (_, JqValue::Scalar { text, .. }) => Extract::Value(Data::Str(text)),
        (_, JqValue::Compound(_)) => Extract::Fail(format!(
            "`{filter}` yielded an object or array; use `jq json` to capture JSON"
        )),
    }
}

/// What a file looked like before a command ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint(Option<(u64, u64, i64, i64, Vec<u8>)>);

impl Fingerprint {
    pub fn of(path: &Path) -> Self {
        let Ok(meta) = fs::metadata(path) else {
            return Self(None);
        };
        let digest = fs::read(path)
            .map(|bytes| Sha256::digest(&bytes).to_vec())
            .unwrap_or_default();
        Self(Some((
            meta.ino(),
            meta.size(),
            meta.mtime(),
            meta.mtime_nsec(),
            digest,
        )))
    }

    /// Whether the file now exists and differs from this fingerprint.
    pub fn fresh(&self, path: &Path) -> bool {
        let now = Self::of(path);
        now.0.is_some() && now != *self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jq(filter: &str, input: &str, optional: bool) -> Extract {
        extract(&CaptureMode::Jq(filter.into()), input.as_bytes(), optional)
    }

    #[test]
    fn jq_result_cases_are_distinct() {
        let doc = r#"{"a":"x","n":null,"o":{"k":1},"xs":[1,2]}"#;
        assert_eq!(jq(".a", doc, false), Extract::Value(Data::Str("x".into())));
        assert_eq!(
            jq(".missing", doc, false),
            Extract::Fail("`.missing` yielded null".into())
        );
        assert_eq!(
            jq(".n", doc, false),
            Extract::Fail("`.n` yielded null".into())
        );
        assert_eq!(jq(".missing", doc, true), Extract::Absent);
        assert_eq!(jq(".n", doc, true), Extract::Absent);
        assert_eq!(
            jq(".xs[] | select(. > 5)", doc, false),
            Extract::Fail("`.xs[] | select(. > 5)` yielded no results".into())
        );
        assert_eq!(jq(".xs[] | select(. > 5)", doc, true), Extract::Absent);
        assert!(
            matches!(jq(".xs[]", doc, false), Extract::Fail(m) if m.contains("yielded 2 results"))
        );
        assert!(
            matches!(jq(".xs[]", doc, true), Extract::Fail(m) if m.contains("yielded 2 results"))
        );
        assert!(matches!(jq(".o", doc, false), Extract::Fail(m) if m.contains("use `jq json`")));
        assert!(matches!(jq(".", "nope", true), Extract::Fail(m) if m.contains("not JSON")));
    }

    #[test]
    fn list_json_text_lines_regex() {
        let doc = r#"{"xs":["a","b"],"o":{"k":1}}"#;
        assert_eq!(
            extract(&CaptureMode::JqList(".xs[]".into()), doc.as_bytes(), false),
            Extract::Value(Data::List(vec!["a".into(), "b".into()]))
        );
        assert_eq!(
            extract(
                &CaptureMode::JqList(".none[]?".into()),
                doc.as_bytes(),
                false
            ),
            Extract::Value(Data::List(vec![]))
        );
        assert_eq!(
            extract(&CaptureMode::JqJson(".o".into()), doc.as_bytes(), false),
            Extract::Value(Data::Str(r#"{"k":1}"#.into()))
        );
        assert_eq!(
            extract(&CaptureMode::Lines, b"a\n\n b\n", false),
            Extract::Value(Data::List(vec!["a".into(), " b".into()]))
        );
        assert_eq!(
            extract(&CaptureMode::Regex(r"(\d+) rows".into()), b"12 rows", false),
            Extract::Value(Data::Str("12".into()))
        );
        assert_eq!(
            extract(&CaptureMode::Regex(r"(\d+) rows".into()), b"none", true),
            Extract::Absent
        );
    }

    #[test]
    fn fingerprint_detects_stale_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let absent = Fingerprint::of(&path);
        assert!(!absent.fresh(&path));
        std::fs::write(&path, "one").unwrap();
        assert!(absent.fresh(&path));
        let before = Fingerprint::of(&path);
        assert!(!before.fresh(&path), "unchanged file is stale");
        std::fs::write(&path, "two").unwrap();
        assert!(before.fresh(&path));
    }
}
