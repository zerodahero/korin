//! The one redactor that every sink writes through: the terminal, the journal, the report and
//! output files.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

/// The shortest secret korin accepts. Replacing shorter strings would mangle unrelated text.
pub const MIN_SECRET_LEN: usize = 4;

/// Whether a registered string is a secret's own value or a form derived from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The value itself: a shell variable holding it reproduces exactly these bytes.
    Exact,
    /// A form `add_value` derives from a value, such as one JSON leaf or the escaped form.
    Derived,
}

struct Entry {
    value: String,
    name: String,
    kind: Kind,
    /// The shell variable that stands for this value in a `$` line; exact values only.
    var: Option<String>,
    /// Which value the variable holds, for a note: `secret token, value 2`.
    label: String,
}

#[derive(Default)]
struct Inner {
    /// Known secrets, longest first so overlapping secrets redact fully. Of two the same
    /// length, an exact value comes first.
    secrets: Vec<Entry>,
    /// Every variable given out so far, generated suffixes included.
    assigned: std::collections::HashSet<String>,
    /// How many exact values each origin (a name, and an item position) has registered.
    values_per_origin: std::collections::HashMap<(String, Option<usize>), usize>,
}

/// One secret found in a text by [`Redactor::matches`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretMatch {
    pub start: usize,
    pub end: usize,
    /// The secret's korin name.
    pub name: String,
    /// The shell variable for an exact match; `None` for a derived form.
    pub var: Option<String>,
    /// Which value the variable holds, such as `secret tokens, item 2`.
    pub label: String,
}

/// The base of a shell variable name: uppercase, with every character outside `[A-Z0-9_]`
/// replaced by `_`, and a `_` before a leading digit.
pub fn var_base(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            let c = c.to_ascii_uppercase();
            if c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out.push_str("SECRET");
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

impl Inner {
    /// Gives a new exact value its variable: the first of `BASE`, `BASE_2`, `BASE_3`, … that
    /// no earlier value holds.
    fn allocate(&mut self, name: &str, item: Option<usize>) -> (String, String) {
        let count = self
            .values_per_origin
            .entry((name.to_string(), item))
            .or_insert(0);
        *count += 1;
        let count = *count;
        let mut base = var_base(name);
        if let Some(item) = item {
            base = format!("{base}_{}", item + 1);
        }
        let mut var = base.clone();
        let mut n = 2;
        while self.assigned.contains(&var) {
            var = format!("{base}_{n}");
            n += 1;
        }
        self.assigned.insert(var.clone());
        let mut label = format!("secret {name}");
        if let Some(item) = item {
            let _ = write!(label, ", item {}", item + 1);
        }
        if count > 1 {
            let _ = write!(label, ", value {count}");
        }
        (var, label)
    }

    fn insert(&mut self, name: &str, item: Option<usize>, value: &str, kind: Kind) {
        if let Some(index) = self.secrets.iter().position(|e| e.value == value) {
            // A value first seen as another secret's derived form becomes exact.
            if kind == Kind::Exact && self.secrets[index].kind == Kind::Derived {
                let (var, label) = self.allocate(name, item);
                let entry = &mut self.secrets[index];
                entry.name = name.to_string();
                entry.kind = Kind::Exact;
                entry.var = Some(var);
                entry.label = label;
                self.sort();
            }
            return;
        }
        let (var, label) = match kind {
            Kind::Exact => {
                let (var, label) = self.allocate(name, item);
                (Some(var), label)
            }
            Kind::Derived => (None, format!("secret {name}")),
        };
        self.secrets.push(Entry {
            value: value.to_string(),
            name: name.to_string(),
            kind,
            var,
            label,
        });
        self.sort();
    }

    fn sort(&mut self) {
        self.secrets.sort_by_key(|e| {
            (
                std::cmp::Reverse(e.value.len()),
                matches!(e.kind, Kind::Derived),
            )
        });
    }
}

/// Replaces every known secret value with `«secret:name»`.
#[derive(Clone, Default)]
pub struct Redactor {
    inner: Arc<Mutex<Inner>>,
}

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a secret. From now on, every sink redacts it.
    ///
    /// # Errors
    ///
    /// Returns an error if the value is shorter than [`MIN_SECRET_LEN`].
    pub fn add(&self, name: &str, value: &str) -> Result<(), String> {
        self.add_kind(name, None, value, Kind::Exact)
    }

    fn add_kind(
        &self,
        name: &str,
        item: Option<usize>,
        value: &str,
        kind: Kind,
    ) -> Result<(), String> {
        if value.chars().count() < MIN_SECRET_LEN {
            return Err(format!(
                "secret `{name}` is shorter than {MIN_SECRET_LEN} characters, so korin cannot redact it safely"
            ));
        }
        let mut inner = self.inner.lock().expect("redactor lock");
        inner.insert(name, item, value, kind);
        Ok(())
    }

    /// Adds a captured secret and every form it can take in raw output. That covers each
    /// string inside it when it is JSON (so pretty-printed output still redacts), and the
    /// JSON-escaped form of each (so an escaped `SecretString` still redacts). Parts shorter
    /// than [`MIN_SECRET_LEN`] are skipped; only the value itself must be long enough.
    ///
    /// # Errors
    ///
    /// Returns an error if the value itself is shorter than [`MIN_SECRET_LEN`].
    pub fn add_value(&self, name: &str, value: &str) -> Result<(), String> {
        self.add_value_at(name, None, value)
    }

    /// [`Redactor::add_value`] for one item of a secret list, at position `item`, so the
    /// item gets a variable of its own.
    ///
    /// # Errors
    ///
    /// Returns an error if the value itself is shorter than [`MIN_SECRET_LEN`].
    pub fn add_value_at(&self, name: &str, item: Option<usize>, value: &str) -> Result<(), String> {
        self.add_kind(name, item, value, Kind::Exact)?;
        let mut forms = Vec::new();
        forms.push(escaped(value));
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(value) {
            collect_leaves(&json, &mut forms);
        }
        for form in forms {
            if form != value && form.chars().count() >= MIN_SECRET_LEN {
                self.add_kind(name, item, &form, Kind::Derived)?;
                let again = escaped(&form);
                if again != form {
                    self.add_kind(name, item, &again, Kind::Derived)?;
                }
            }
        }
        Ok(())
    }

    /// The length in bytes of the longest known secret.
    pub fn max_len(&self) -> usize {
        let inner = self.inner.lock().expect("redactor lock");
        inner.secrets.first().map_or(0, |e| e.value.len())
    }

    /// Byte ranges in `bytes` where a known secret occurs, overlaps included. Streaming
    /// redaction needs every range, to keep its cut out of each secret.
    fn occurrences(&self, bytes: &[u8]) -> Vec<(usize, usize)> {
        let inner = self.inner.lock().expect("redactor lock");
        let mut out = Vec::new();
        for entry in &inner.secrets {
            let needle = entry.value.as_bytes();
            if needle.is_empty() || needle.len() > bytes.len() {
                continue;
            }
            for start in 0..=bytes.len() - needle.len() {
                if &bytes[start..start + needle.len()] == needle {
                    out.push((start, start + needle.len()));
                }
            }
        }
        out
    }

    /// The secrets in `text` for a `$` line: sorted and never overlapping. Scanning left to
    /// right, each position takes the longest secret that starts there (an exact value wins a
    /// tie), and the scan goes on after it.
    pub fn matches(&self, text: &str) -> Vec<SecretMatch> {
        let inner = self.inner.lock().expect("redactor lock");
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let found = inner
                .secrets
                .iter()
                .find(|e| !e.value.is_empty() && bytes[at..].starts_with(e.value.as_bytes()));
            match found {
                Some(entry) => {
                    let end = at + entry.value.len();
                    out.push(SecretMatch {
                        start: at,
                        end,
                        name: entry.name.clone(),
                        var: entry.var.clone(),
                        label: entry.label.clone(),
                    });
                    at = end;
                }
                None => at += 1,
            }
        }
        out
    }

    pub fn redact(&self, text: &str) -> String {
        let inner = self.inner.lock().expect("redactor lock");
        let mut out = text.to_string();
        for entry in &inner.secrets {
            if out.contains(entry.value.as_str()) {
                out = out.replace(entry.value.as_str(), &format!("«secret:{}»", entry.name));
            }
        }
        out
    }

    /// Redacts bytes, treating them as UTF-8 text with lossy replacement.
    pub fn redact_bytes(&self, bytes: &[u8]) -> String {
        self.redact(&String::from_utf8_lossy(bytes))
    }

    /// Redacts every string inside a JSON value, before it is serialized.
    pub fn redact_json(&self, value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(s) => *s = self.redact(s),
            serde_json::Value::Array(items) => items.iter_mut().for_each(|v| self.redact_json(v)),
            serde_json::Value::Object(map) => map.values_mut().for_each(|v| self.redact_json(v)),
            _ => {}
        }
    }
}

/// The JSON string-escaped form of `text`, without the surrounding quotes.
fn escaped(text: &str) -> String {
    let quoted = serde_json::to_string(text).unwrap_or_default();
    quoted
        .strip_prefix('"')
        .and_then(|q| q.strip_suffix('"'))
        .unwrap_or(&quoted)
        .to_string()
}

fn collect_leaves(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => {
            out.push(s.clone());
            if let Ok(inner) = serde_json::from_str::<serde_json::Value>(s)
                && (inner.is_object() || inner.is_array())
            {
                collect_leaves(&inner, out);
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect_leaves(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| collect_leaves(v, out)),
        // A number prints the same in compact and pretty JSON, so its own text is a form to
        // redact. Short numbers fall under the length minimum in `add_value`.
        serde_json::Value::Number(n) => out.push(n.to_string()),
        // `true`, `false` and `null` are not secrets, and redacting them would mangle output.
        serde_json::Value::Bool(_) | serde_json::Value::Null => {}
    }
}

/// Redacts a stream that arrives in chunks. A secret may be split across two chunks, so the
/// end of the buffer is held back until it can no longer be the start of a secret.
#[derive(Default)]
pub struct StreamRedactor {
    pending: Vec<u8>,
}

impl StreamRedactor {
    /// Adds a chunk and returns the redacted text that is now safe to show.
    pub fn push(&mut self, redactor: &Redactor, chunk: &[u8]) -> String {
        self.pending.extend_from_slice(chunk);
        let hold = redactor.max_len().saturating_sub(1);
        let mut cut = self.pending.len().saturating_sub(hold);
        // Prefer a line boundary. This only moves the cut earlier.
        if let Some(newline) = self.pending[..cut].iter().rposition(|b| *b == b'\n') {
            cut = newline + 1;
        }
        // Never split a UTF-8 character.
        while cut > 0 && cut < self.pending.len() && (self.pending[cut] & 0xC0) == 0x80 {
            cut -= 1;
        }
        // Last, never cut through a secret, including one that spans lines. Moving the cut to
        // an occurrence's start can land inside another, so repeat until none straddles it.
        let occurrences = redactor.occurrences(&self.pending);
        loop {
            let straddled = occurrences
                .iter()
                .filter(|(start, end)| *start < cut && cut < *end)
                .map(|(start, _)| *start)
                .min();
            match straddled {
                Some(start) => cut = start,
                None => break,
            }
        }
        let ready: Vec<u8> = self.pending.drain(..cut).collect();
        redactor.redact_bytes(&ready)
    }

    /// Returns everything still held, redacted.
    pub fn finish(&mut self, redactor: &Redactor) -> String {
        let rest = std::mem::take(&mut self.pending);
        redactor.redact_bytes(&rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_longest_first_and_inside_json() {
        let r = Redactor::new();
        r.add("short", "abcd").unwrap();
        r.add("long", "abcdefgh").unwrap();
        assert_eq!(
            r.redact("x abcdefgh y abcd"),
            "x «secret:long» y «secret:short»"
        );
        let mut v = serde_json::json!({"a": ["tok abcd", {"b": "abcdefgh"}]});
        r.redact_json(&mut v);
        assert_eq!(
            v.to_string(),
            r#"{"a":["tok «secret:short»",{"b":"«secret:long»"}]}"#
        );
    }

    #[test]
    fn stream_never_splits_a_secret() {
        let r = Redactor::new();
        r.add("token", "SECRET-TOKEN-1234").unwrap();
        let mut stream = StreamRedactor::default();
        let mut shown = String::new();
        for chunk in [
            "start SECRET-",
            "TOKEN-",
            "1234 middle\n",
            "end SECRET-TOKEN-1234",
        ] {
            shown.push_str(&stream.push(&r, chunk.as_bytes()));
        }
        shown.push_str(&stream.finish(&r));
        assert_eq!(shown, "start «secret:token» middle\nend «secret:token»");
    }

    #[test]
    fn stream_never_splits_a_multiline_secret_at_a_newline() {
        let r = Redactor::new();
        r.add("key", "AAAA\nBBBB").unwrap();
        let mut stream = StreamRedactor::default();
        let mut shown = stream.push(&r, b"AAAA\nBBBBxxxxxxxxx");
        shown.push_str(&stream.finish(&r));
        assert_eq!(shown, "«secret:key»xxxxxxxxx");
    }

    #[test]
    fn add_value_covers_number_leaves_but_not_booleans() {
        let r = Redactor::new();
        r.add_value("creds", r#"{"pin":123456,"short":42,"ok":true}"#)
            .unwrap();
        let shown = r.redact("{\n  \"pin\": 123456,\n  \"short\": 42,\n  \"ok\": true\n}");
        assert!(!shown.contains("123456"), "{shown}");
        assert!(shown.contains("42") && shown.contains("true"), "{shown}");
    }

    #[test]
    fn add_value_covers_json_leaves_and_escaped_forms() {
        let r = Redactor::new();
        r.add_value(
            "creds",
            r#"{"AccessKeyId":"AKIAEXAMPLE","SecretAccessKey":"wJalrXUtnFEMI/K7MDENG"}"#,
        )
        .unwrap();
        let pretty = "{\n  \"AccessKeyId\": \"AKIAEXAMPLE\",\n  \"SecretAccessKey\": \"wJalrXUtnFEMI/K7MDENG\"\n}";
        let shown = r.redact(pretty);
        assert!(
            !shown.contains("AKIAEXAMPLE") && !shown.contains("wJalrXUtnFEMI"),
            "{shown}"
        );

        let r = Redactor::new();
        r.add_value("pw", r#"{"password":"hunter2-long"}"#).unwrap();
        let raw = r#"{"SecretString":"{\"password\":\"hunter2-long\"}"}"#;
        assert!(!r.redact(raw).contains("hunter2-long"), "{}", r.redact(raw));
    }

    #[test]
    fn rejects_short_secrets() {
        assert!(Redactor::new().add("pin", "123").is_err());
    }

    fn vars(r: &Redactor, text: &str) -> Vec<Option<String>> {
        r.matches(text).into_iter().map(|m| m.var).collect()
    }

    #[test]
    fn colliding_names_get_distinct_variables_in_any_order() {
        let names = ["foo.bar", "foo_bar", "foo_bar_2"];
        let orders = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        for order in orders {
            let r = Redactor::new();
            for (n, &i) in order.iter().enumerate() {
                r.add(names[i], &format!("value-number-{n}")).unwrap();
            }
            let found: Vec<String> = (0..3)
                .map(|n| {
                    r.matches(&format!("value-number-{n}"))[0]
                        .var
                        .clone()
                        .unwrap()
                })
                .collect();
            let mut unique = found.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(unique.len(), 3, "{order:?}: {found:?}");
        }
        let r = Redactor::new();
        r.add("foo.bar", "first-value").unwrap();
        r.add("foo_bar", "second-value").unwrap();
        assert_eq!(
            vars(&r, "first-value second-value"),
            [Some("FOO_BAR".into()), Some("FOO_BAR_2".into())]
        );
    }

    #[test]
    fn list_items_and_recaptures_get_their_own_variables() {
        let r = Redactor::new();
        r.add_value_at("tokens", Some(0), "item-one-value").unwrap();
        r.add_value_at("tokens", Some(1), "item-two-value").unwrap();
        let found = r.matches("item-one-value item-two-value");
        assert_eq!(found[0].var.as_deref(), Some("TOKENS_1"));
        assert_eq!(found[1].var.as_deref(), Some("TOKENS_2"));
        assert_eq!(found[1].label, "secret tokens, item 2");

        let r = Redactor::new();
        r.add_value("token", "old-token-value").unwrap();
        r.add_value("token", "new-token-value").unwrap();
        // The same value again keeps its variable.
        r.add_value("token", "old-token-value").unwrap();
        let found = r.matches("old-token-value new-token-value");
        assert_eq!(found[0].var.as_deref(), Some("TOKEN"));
        assert_eq!(found[1].var.as_deref(), Some("TOKEN_2"));
        assert_eq!(found[1].label, "secret token, value 2");
    }

    #[test]
    fn derived_forms_match_without_a_variable() {
        let r = Redactor::new();
        let creds = r#"{"key":"leaf-secret-value"}"#;
        r.add_value("creds", creds).unwrap();
        assert_eq!(vars(&r, creds), [Some("CREDS".into())]);
        let leaf = r.matches("x leaf-secret-value y");
        assert_eq!(leaf.len(), 1);
        assert_eq!(leaf[0].var, None);
        assert_eq!(leaf[0].name, "creds");
        // The escaped form of the whole value is derived too.
        let escaped = r#"{\"key\":\"leaf-secret-value\"}"#;
        assert_eq!(vars(&r, escaped), [None]);
    }

    #[test]
    fn matches_never_overlap_and_prefer_the_longest_then_exact() {
        let r = Redactor::new();
        r.add("short", "abcd").unwrap();
        r.add("long", "abcdefgh").unwrap();
        r.add("tail", "efghijkl").unwrap();
        let found = r.matches("abcdefghijkl abcd");
        let spans: Vec<(usize, usize, &str)> = found
            .iter()
            .map(|m| (m.start, m.end, m.name.as_str()))
            .collect();
        assert_eq!(spans, [(0, 8, "long"), (13, 17, "short")]);

        // A tie of length: the exact value wins over another secret's derived form.
        let r = Redactor::new();
        r.add_value("creds", r#"{"k":"same-length"}"#).unwrap();
        r.add("plain", "same-length").unwrap();
        let found = r.matches("same-length");
        assert_eq!(found[0].var.as_deref(), Some("PLAIN"));
    }

    #[test]
    fn stream_keeps_partly_overlapping_secrets_whole_at_every_split() {
        let r = Redactor::new();
        r.add("first", "abcdef").unwrap();
        r.add("second", "defghij").unwrap();
        let input = "abcdefghijXXXX";
        let whole = {
            let mut s = StreamRedactor::default();
            let mut out = s.push(&r, input.as_bytes());
            out.push_str(&s.finish(&r));
            out
        };
        for split in 0..=input.len() {
            let mut s = StreamRedactor::default();
            let mut out = s.push(&r, &input.as_bytes()[..split]);
            out.push_str(&s.push(&r, &input.as_bytes()[split..]));
            out.push_str(&s.finish(&r));
            assert_eq!(out, whole, "split at {split}");
            assert!(
                !out.contains("abcdef") && !out.contains("defghij"),
                "split at {split}: {out}"
            );
        }
    }

    #[test]
    fn var_base_normalizes_names() {
        assert_eq!(var_base("db.password"), "DB_PASSWORD");
        assert_eq!(var_base("2fa"), "_2FA");
        assert_eq!(var_base("tok-en"), "TOK_EN");
    }
}
