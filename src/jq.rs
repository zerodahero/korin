//! jq filters, evaluated by jaq.

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, Vars, data, unwrap_valr};
use jaq_json::Val;

type Filter = jaq_core::Filter<data::JustLut<Val>>;

/// A compiled jq filter.
pub struct Jq {
    filter: Filter,
}

/// One result of a filter, reduced to what korin needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JqValue {
    Null,
    /// A string, number or boolean: its korin string form, and its JSON text.
    Scalar {
        text: String,
        json: String,
    },
    /// An array or object, as compact JSON text.
    Compound(String),
}

impl JqValue {
    /// Compact JSON text for any value.
    pub fn json_text(&self) -> String {
        match self {
            JqValue::Null => "null".into(),
            JqValue::Scalar { json: s, .. } | JqValue::Compound(s) => s.clone(),
        }
    }
}

/// Why a filter could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JqError {
    /// The input is not JSON.
    NotJson(String),
    /// The filter raised an error while running.
    Runtime(String),
}

impl Jq {
    /// Compiles a filter.
    ///
    /// # Errors
    ///
    /// Returns a description of the first parse or compile error.
    pub fn compile(code: &str) -> Result<Self, String> {
        let defs = jaq_core::defs()
            .chain(jaq_std::defs())
            .chain(jaq_json::defs());
        let funs = jaq_core::funs()
            .chain(jaq_std::funs())
            .chain(jaq_json::funs());
        let loader = Loader::new(defs);
        let arena = Arena::default();
        let modules = loader
            .load(&arena, File { code, path: () })
            .map_err(|errors| describe_load_errors(&errors))?;
        let filter = Compiler::default()
            .with_funs(funs)
            .compile(modules)
            .map_err(|errors| {
                let names: Vec<String> = errors
                    .iter()
                    .flat_map(|(_, errs)| errs.iter().map(|(name, _)| (*name).to_string()))
                    .collect();
                format!("invalid jq filter: undefined {}", names.join(", "))
            })?;
        Ok(Self { filter })
    }

    /// Runs the filter on JSON text and collects every result.
    ///
    /// # Errors
    ///
    /// Returns [`JqError::NotJson`] if the input does not parse, or [`JqError::Runtime`] if the
    /// filter raises an error.
    pub fn run(&self, input: &str) -> Result<Vec<JqValue>, JqError> {
        let value = jaq_json::read::parse_single(input.trim().as_bytes())
            .map_err(|error| JqError::NotJson(format!("{error:?}")))?;
        let ctx = Ctx::<data::JustLut<Val>>::new(&self.filter.lut, Vars::new([]));
        self.filter
            .id
            .run((ctx, value))
            .map(unwrap_valr)
            .map(|result| {
                result
                    .map(|val| convert(&val))
                    .map_err(|error| JqError::Runtime(error.to_string()))
            })
            .collect()
    }
}

fn describe_load_errors(errors: &[(File<&str, ()>, jaq_core::load::Error<&str>)]) -> String {
    use jaq_core::load::Error;
    let near = |rest: &str| {
        let rest = rest.trim();
        if rest.is_empty() {
            "at the end of the filter".to_string()
        } else {
            format!("near `{}`", rest.chars().take(20).collect::<String>())
        }
    };
    let detail = errors.iter().find_map(|(_, error)| match error {
        Error::Lex(errs) => errs
            .first()
            .map(|(expect, rest)| format!("expected {} {}", expect.as_str(), near(rest))),
        Error::Parse(errs) => errs
            .first()
            .map(|(expect, rest)| format!("expected {} {}", expect.as_str(), near(rest))),
        Error::Io(errs) => errs.first().map(|(_, message)| message.clone()),
    });
    format!(
        "invalid jq filter: {}",
        detail.unwrap_or_else(|| "cannot parse".into())
    )
}

fn convert(val: &Val) -> JqValue {
    match val {
        Val::Null => JqValue::Null,
        Val::Bool(_) | Val::Num(_) => JqValue::Scalar {
            text: val.to_string(),
            json: val.to_string(),
        },
        Val::TStr(bytes) | Val::BStr(bytes) => JqValue::Scalar {
            text: String::from_utf8_lossy(bytes).into_owned(),
            json: val.to_string(),
        },
        Val::Arr(_) | Val::Obj(_) => JqValue::Compound(val.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar(text: &str) -> JqValue {
        let json = serde_json::from_str::<serde_json::Value>(text)
            .map_or_else(|_| serde_json::to_string(text).unwrap(), |v| v.to_string());
        JqValue::Scalar {
            text: text.into(),
            json,
        }
    }

    #[test]
    fn json_text_keeps_string_quotes() {
        let values = run(".s, .n", r#"{"s":"hello","n":"1234"}"#).unwrap();
        let json: Vec<String> = values.iter().map(JqValue::json_text).collect();
        assert_eq!(
            json,
            vec![r#""hello""#.to_string(), r#""1234""#.to_string()]
        );
    }

    fn run(filter: &str, input: &str) -> Result<Vec<JqValue>, JqError> {
        Jq::compile(filter).unwrap().run(input)
    }

    #[test]
    fn scalars_and_compounds() {
        assert_eq!(
            run(
                ".a, .b, .c, .d",
                r#"{"a":"x","b":1.5,"c":true,"d":{"k":[1,2]}}"#
            )
            .unwrap(),
            vec![
                scalar("x"),
                scalar("1.5"),
                scalar("true"),
                JqValue::Compound(r#"{"k":[1,2]}"#.into()),
            ]
        );
    }

    #[test]
    fn missing_key_is_null_and_empty_is_empty() {
        assert_eq!(run(".missing", "{}").unwrap(), vec![JqValue::Null]);
        assert_eq!(run(".[]", "[]").unwrap(), vec![]);
    }

    #[test]
    fn not_json_and_bad_filter() {
        assert!(matches!(run(".", "not json"), Err(JqError::NotJson(_))));
        assert!(Jq::compile(".a |").is_err());
        assert!(Jq::compile("nosuchfn").is_err());
        assert!(matches!(
            run(".a.b", r#"{"a":1}"#),
            Err(JqError::Runtime(_))
        ));
    }
}
