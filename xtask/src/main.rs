//! Development tasks: `cargo xtask <task>`.
//!
//! - `schemas [--check]`: write (or verify) the published JSON Schemas in `schemas/`.
//! - `vocabulary`: fail on names that use banned words (see `vocabulary.rs`, AGENTS.md).
#![forbid(unsafe_code)]

mod vocabulary;

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("schemas") => schemas(args.iter().any(|a| a == "--check")),
        Some("vocabulary") => check_vocabulary(),
        _ => {
            let mut err = std::io::stderr().lock();
            writeln!(err, "usage: cargo xtask schemas [--check] | vocabulary")?;
            bail!("unknown or missing task")
        }
    }
}

fn check_vocabulary() -> Result<()> {
    let findings = vocabulary::check(&workspace_root())?;
    if findings.is_empty() {
        return Ok(());
    }
    let mut err = std::io::stderr().lock();
    for f in &findings {
        writeln!(err, "{f}")?;
    }
    bail!(
        "{} name(s) use banned words ({}); see AGENTS.md \"Vocabulary\"",
        findings.len(),
        vocabulary::BANNED.join(", ")
    )
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Every published schema: path under `schemas/` and its content.
fn all_schemas() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("evid.schema.json", evidence_core::format::evid_schema()),
        ("config.schema.json", evidence_core::format::config_schema()),
    ]
}

fn schemas(check: bool) -> Result<()> {
    let dir = workspace_root().join("schemas");
    let mut stale: Vec<String> = Vec::new();
    for (name, schema) in all_schemas() {
        let path = dir.join(name);
        let text = format!("{}\n", serde_json::to_string_pretty(&schema)?);
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current == text {
            continue;
        }
        if check {
            stale.push(name.to_owned());
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
            writeln!(std::io::stdout().lock(), "wrote schemas/{name}")?;
        }
    }
    // Schemas no longer generated are not published.
    let published: Vec<&str> = all_schemas().iter().map(|(name, _)| *name).collect();
    let mut orphans = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let file = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir()
            || (file.ends_with(".schema.json") && !published.contains(&file.as_str()))
        {
            orphans.push(file);
        }
    }
    orphans.sort();
    for name in orphans {
        if check {
            stale.push(name);
        } else {
            let path = dir.join(&name);
            if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            }
            .with_context(|| format!("removing schemas/{name}"))?;
            writeln!(std::io::stdout().lock(), "removed schemas/{name}")?;
        }
    }
    if !stale.is_empty() {
        bail!(
            "out of date: {}; run `cargo xtask schemas`",
            stale
                .iter()
                .map(|s| format!("schemas/{s}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}
