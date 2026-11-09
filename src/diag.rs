//! Source files and diagnostics.
//!
//! Every error korin reports before running anything is a [`Diag`]. It renders through miette
//! for humans and as JSON (`korin check --format json`) for agents.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use miette::{LabeledSpan, SourceCode};
use serde::Serialize;

use crate::ast::Span;

/// One loaded `.korin` file.
#[derive(Debug)]
pub struct SourceFile {
    /// The path as given or resolved, used for relative imports and executables.
    pub path: PathBuf,
    /// The name shown in diagnostics.
    pub name: String,
    pub text: String,
}

impl SourceFile {
    pub fn new(
        path: impl Into<PathBuf>,
        name: impl Into<String>,
        text: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            path: path.into(),
            name: name.into(),
            text: text.into(),
        })
    }

    /// The directory that relative paths in this file resolve against.
    pub fn dir(&self) -> &Path {
        self.path.parent().unwrap_or_else(|| Path::new("."))
    }

    /// The 1-based line and column of a byte offset.
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.text.len());
        let before = &self.text[..offset];
        let line = before.matches('\n').count() + 1;
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        let column = self.text[line_start..offset].chars().count() + 1;
        (line, column)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// A diagnostic anchored to a span in a source file.
#[derive(Debug, Clone)]
pub struct Diag {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    pub label: String,
    pub span: Span,
    pub help: Option<String>,
    pub file: Arc<SourceFile>,
    /// Secondary spans, for example the earlier definition in a duplicate-name error.
    pub related: Vec<(Span, String)>,
}

impl Diag {
    pub fn error(
        file: &Arc<SourceFile>,
        code: &'static str,
        span: Span,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Error,
            code,
            message: message.into(),
            label: String::new(),
            span,
            help: None,
            file: Arc::clone(file),
            related: Vec::new(),
        }
    }

    pub fn warning(
        file: &Arc<SourceFile>,
        code: &'static str,
        span: Span,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(file, code, span, message)
        }
    }

    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    #[must_use]
    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    #[must_use]
    pub fn related(mut self, span: Span, label: impl Into<String>) -> Self {
        self.related.push((span, label.into()));
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    pub fn to_json(&self) -> JsonDiag {
        let (line, column) = self.file.line_col(self.span.start);
        JsonDiag {
            file: self.file.name.clone(),
            line,
            column,
            severity: self.severity,
            code: self.code,
            message: self.message.clone(),
            label: (!self.label.is_empty()).then(|| self.label.clone()),
            help: self.help.clone(),
        }
    }

    /// Plain one-line form, used in tests and logs: `file:line:col: code: message`.
    pub fn short(&self) -> String {
        let (line, column) = self.file.line_col(self.span.start);
        format!(
            "{}:{line}:{column}: {}: {}",
            self.file.name, self.code, self.message
        )
    }
}

#[derive(Debug, Serialize)]
pub struct JsonDiag {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Diag {}

impl miette::Diagnostic for Diag {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(format!("korin::{}", self.code)))
    }

    fn severity(&self) -> Option<miette::Severity> {
        Some(match self.severity {
            Severity::Error => miette::Severity::Error,
            Severity::Warning => miette::Severity::Warning,
        })
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.help
            .as_ref()
            .map(|h| Box::new(h) as Box<dyn fmt::Display>)
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        Some(self)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let primary = LabeledSpan::new_primary_with_span(
            (!self.label.is_empty()).then(|| self.label.clone()),
            self.span,
        );
        let related = self
            .related
            .iter()
            .map(|(span, label)| LabeledSpan::new_with_span(Some(label.clone()), *span));
        Some(Box::new(std::iter::once(primary).chain(related)))
    }
}

impl SourceCode for Diag {
    fn read_span<'a>(
        &'a self,
        span: &miette::SourceSpan,
        context_lines_before: usize,
        context_lines_after: usize,
    ) -> Result<Box<dyn miette::SpanContents<'a> + 'a>, miette::MietteError> {
        let contents = self
            .file
            .text
            .read_span(span, context_lines_before, context_lines_after)?;
        Ok(Box::new(miette::MietteSpanContents::new_named(
            self.file.name.clone(),
            contents.data(),
            *contents.span(),
            contents.line(),
            contents.column(),
            contents.line_count(),
        )))
    }
}
