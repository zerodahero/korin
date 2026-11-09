//! The run directory: `events.jsonl`, `report.md` and `out/`. Everything written here passes
//! through the redactor first.

use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::redact::Redactor;

pub struct Journal {
    dir: PathBuf,
    events: File,
    report: File,
    redactor: Redactor,
    /// How many writes failed, and the first error. A failure never stops the run, but the
    /// operator is told at once and again at the end.
    failures: RefCell<(usize, Option<String>)>,
}

impl Journal {
    /// Creates `<root>/<stamp>-<slug>/` with its files.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the directory or files cannot be created.
    pub fn create(root: &Path, slug: &str, redactor: Redactor) -> std::io::Result<Self> {
        let stamp = jiff::Timestamp::now()
            .strftime("%Y%m%dT%H%M%SZ")
            .to_string();
        let mut dir = root.join(format!("{stamp}-{slug}"));
        let mut n = 1;
        while dir.exists() {
            n += 1;
            dir = root.join(format!("{stamp}-{slug}-{n}"));
        }
        fs::create_dir_all(dir.join("out"))?;
        let events = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(dir.join("events.jsonl"))?;
        let report = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(dir.join("report.md"))?;
        Ok(Self {
            dir,
            events,
            report,
            redactor,
            failures: RefCell::new((0, None)),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Appends one event. Every string in `fields` is redacted.
    pub fn event(&mut self, kind: &str, fields: Value) {
        let mut event = json!({
            "ts": jiff::Timestamp::now().to_string(),
            "kind": kind,
        });
        if let (Value::Object(target), Value::Object(extra)) = (&mut event, fields) {
            target.extend(extra);
        }
        self.redactor.redact_json(&mut event);
        if let Err(error) = writeln!(self.events, "{event}") {
            self.failed("events.jsonl", &error);
        }
    }

    /// Appends a line to `report.md`.
    pub fn report(&mut self, line: &str) {
        if let Err(error) = writeln!(self.report, "{}", self.redactor.redact(line)) {
            self.failed("report.md", &error);
        }
    }

    /// Writes a redacted output file under `out/`.
    pub fn write_out(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.dir.join("out").join(relative);
        let written = path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(&path, self.redactor.redact_bytes(bytes)));
        if let Err(error) = written {
            self.failed(&format!("out/{relative}"), &error);
        }
        path
    }

    /// The number of failed writes and the first error, if any write failed.
    pub fn failures(&self) -> Option<(usize, String)> {
        let failures = self.failures.borrow();
        failures.1.clone().map(|first| (failures.0, first))
    }

    fn failed(&self, what: &str, error: &std::io::Error) {
        let mut failures = self.failures.borrow_mut();
        failures.0 += 1;
        if failures.1.is_none() {
            let message = format!("{what}: {error}");
            println!(
                "  ⚠ cannot write the journal ({message}); the run continues, but its record is incomplete"
            );
            failures.1 = Some(message);
        }
    }
}
