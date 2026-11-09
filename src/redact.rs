//! The one redactor that every sink writes through: the terminal, the journal, the report and
//! output files.

use std::sync::{Arc, Mutex};

/// The shortest secret korin accepts. Replacing shorter strings would mangle unrelated text.
pub const MIN_SECRET_LEN: usize = 4;

#[derive(Default)]
struct Inner {
    /// Known secrets with their names, longest first so overlapping secrets redact fully.
    secrets: Vec<(String, String)>,
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
        if value.chars().count() < MIN_SECRET_LEN {
            return Err(format!(
                "secret `{name}` is shorter than {MIN_SECRET_LEN} characters, so korin cannot redact it safely"
            ));
        }
        let mut inner = self.inner.lock().expect("redactor lock");
        if !inner.secrets.iter().any(|(v, _)| v == value) {
            inner.secrets.push((value.to_string(), name.to_string()));
            inner
                .secrets
                .sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
        }
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
        self.add(name, value)?;
        let mut forms = Vec::new();
        forms.push(escaped(value));
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(value) {
            collect_leaves(&json, &mut forms);
        }
        for form in forms {
            if form != value && form.chars().count() >= MIN_SECRET_LEN {
                self.add(name, &form)?;
                let again = escaped(&form);
                if again != form {
                    self.add(name, &again)?;
                }
            }
        }
        Ok(())
    }

    /// The length in bytes of the longest known secret.
    pub fn max_len(&self) -> usize {
        let inner = self.inner.lock().expect("redactor lock");
        inner.secrets.first().map_or(0, |(v, _)| v.len())
    }

    /// Byte ranges in `bytes` where a known secret occurs.
    fn occurrences(&self, bytes: &[u8]) -> Vec<(usize, usize)> {
        let inner = self.inner.lock().expect("redactor lock");
        let mut out = Vec::new();
        for (value, _) in &inner.secrets {
            let needle = value.as_bytes();
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

    pub fn redact(&self, text: &str) -> String {
        let inner = self.inner.lock().expect("redactor lock");
        let mut out = text.to_string();
        for (value, name) in &inner.secrets {
            if out.contains(value.as_str()) {
                out = out.replace(value.as_str(), &format!("«secret:{name}»"));
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
}
