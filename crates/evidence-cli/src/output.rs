//! Terminal output: colors, human-readable lines, and the JSON envelope.

use std::io::{IsTerminal, Write};

use evidence_core::LineStatus;
use evidence_core::{ClaimHealth, Health};
use serde::Serialize;

use crate::cli::ColorChoice;

pub(crate) use evidence_core::JSON_VERSION;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Style {
    Bold,
    Dim,
    Green,
    Yellow,
    Red,
    Cyan,
    Magenta,
}

impl Style {
    fn code(self) -> &'static str {
        match self {
            Self::Bold => "1",
            Self::Dim => "2",
            Self::Green => "32",
            Self::Yellow => "33",
            Self::Red => "31",
            Self::Cyan => "36",
            Self::Magenta => "35",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Out {
    pub(crate) json: bool,
    color: bool,
}

impl Out {
    pub(crate) fn new(json: bool, choice: ColorChoice) -> Self {
        let color = match choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => {
                std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
            }
        };
        Self { json, color }
    }

    pub(crate) fn paint(self, text: impl AsRef<str>, style: Style) -> String {
        if self.color {
            format!("\x1b[{}m{}\x1b[0m", style.code(), text.as_ref())
        } else {
            text.as_ref().to_owned()
        }
    }

    pub(crate) fn health(self, h: Health) -> String {
        let style = match h {
            Health::Ok => Style::Green,
            Health::Due | Health::Stale | Health::Modified => Style::Yellow,
            Health::Failed | Health::Missing | Health::Broken => Style::Red,
        };
        self.paint(format!("{:<8}", h.as_str()), style)
    }

    pub(crate) fn claim(self, h: ClaimHealth) -> String {
        let style = match h {
            ClaimHealth::Matched => Style::Green,
            ClaimHealth::Stale => Style::Yellow,
            ClaimHealth::Broken => Style::Red,
        };
        self.paint(format!("{:<8}", h.as_str()), style)
    }

    pub(crate) fn line_status(self, s: LineStatus) -> String {
        let style = match s {
            LineStatus::Matched => Style::Green,
            LineStatus::Primary => Style::Cyan,
            LineStatus::Stale => Style::Yellow,
            LineStatus::Broken => Style::Red,
            LineStatus::Uncovered => Style::Magenta,
        };
        self.paint(format!("{:<9}", s.as_str()), style)
    }

    /// Prints `{"version": 2, "<key>": value}`.
    #[allow(clippy::unused_self)] // keeps call sites uniform: `ctx.out.emit(..)`
    pub(crate) fn emit<T: Serialize>(self, key: &str, value: &T) -> anyhow::Result<()> {
        let mut obj = serde_json::Map::new();
        obj.insert("version".into(), JSON_VERSION.into());
        obj.insert(key.into(), serde_json::to_value(value)?);
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &serde_json::Value::Object(obj))?;
        writeln!(stdout)?;
        Ok(())
    }
}

/// Prints a line to stdout (ignoring broken pipes).
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout().lock(), $($arg)*);
    }};
}

/// Prints a line to stderr.
#[macro_export]
macro_rules! warn_line {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr().lock(), $($arg)*);
    }};
}
