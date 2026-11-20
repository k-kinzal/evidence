//! Validation diagnostics.

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

/// Severity of a diagnostic.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Informational; never fails a command.
    Warning,
    /// Invalid input; fails validation.
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Warning => "warning",
            Self::Error => "error",
        })
    }
}

/// A problem found in an evidence file, configuration, or subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Diagnostic {
    /// Severity.
    pub severity: Severity,
    /// Stable machine-readable code (e.g. `schema`, `unknown-ref`, `cycle`).
    pub code: &'static str,
    /// Human-readable message.
    pub message: String,
    /// File the diagnostic refers to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    /// 1-based line in `file`, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// JSON Pointer to the offending value, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
}

impl Diagnostic {
    /// Creates an error.
    pub(crate) fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            message: message.into(),
            file: None,
            line: None,
            pointer: None,
        }
    }

    /// Creates a warning.
    pub(crate) fn warning(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(code, message)
        }
    }

    /// Sets the file.
    #[must_use]
    pub(crate) fn in_file(mut self, file: impl Into<PathBuf>) -> Self {
        self.file = Some(file.into());
        self
    }

    /// Sets the line.
    #[must_use]
    pub(crate) fn at_line(mut self, line: Option<usize>) -> Self {
        self.line = line;
        self
    }

    /// Sets the JSON Pointer.
    #[must_use]
    pub(crate) fn at_pointer(mut self, pointer: impl Into<String>) -> Self {
        let pointer = pointer.into();
        self.pointer = (!pointer.is_empty()).then_some(pointer);
        self
    }

    /// Whether this is an error.
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}[{}]", self.severity, self.code)?;
        if let Some(file) = &self.file {
            write!(f, " {}", file.display())?;
            if let Some(line) = self.line {
                write!(f, ":{line}")?;
            }
        }
        if let Some(pointer) = &self.pointer {
            write!(f, " ({pointer})")?;
        }
        write!(f, ": {}", self.message)
    }
}
