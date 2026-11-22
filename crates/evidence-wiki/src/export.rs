//! `evidence-wiki build`: the whole site as static files.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::html::{Mode, Output, Site};
use crate::wiki::Wiki;
use crate::{assets, check};

/// What `build` wrote.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Built {
    /// Output directory.
    pub out: PathBuf,
    /// HTML pages written.
    pub pages: usize,
    /// Project files copied under `files/`.
    pub files: usize,
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// Writes the static site into `out`. Existing files there are overwritten, never removed.
pub fn build(wiki: &Wiki, out: &Path) -> Result<Built> {
    let site = Site {
        wiki,
        mode: Mode::Static,
    };
    let root = wiki.project().root();
    if std::fs::canonicalize(out).is_ok_and(|o| o == root) {
        anyhow::bail!("the output directory must not be the project root");
    }
    let mut built = Built {
        out: out.to_path_buf(),
        ..Built::default()
    };
    let mut files: BTreeSet<String> = BTreeSet::new();
    let mut emit = |name: &str, o: Output, built: &mut Built| -> Result<()> {
        write(&out.join(name), o.html.as_bytes())?;
        files.extend(o.files);
        built.pages += 1;
        Ok(())
    };
    emit("index.html", site.home(), &mut built)?;
    emit("all.html", site.all_pages(), &mut built)?;
    emit("categories.html", site.categories(), &mut built)?;
    emit("tags.html", site.tags(), &mut built)?;
    emit("search.html", site.search(), &mut built)?;
    emit(
        "maintenance.html",
        site.maintenance(&check::check(wiki)),
        &mut built,
    )?;
    for i in 0..wiki.pages().len() {
        let id = &wiki.page(i).id;
        emit(&format!("p/{id}.html"), site.page(i), &mut built)?;
        emit(&format!("history/{id}.html"), site.history(i), &mut built)?;
    }
    for a in assets::ASSETS {
        write(&out.join("assets").join(a.path), a.bytes)?;
    }
    write(&out.join("search-index.js"), site.search_index().as_bytes())?;
    for rel in &files {
        let Some(src) = wiki.servable_file(rel) else {
            continue;
        };
        let dst = out.join("files").join(rel);
        if let Some(dir) = dst.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::copy(&src, &dst).with_context(|| format!("copying {rel}"))?;
        built.files += 1;
    }
    Ok(built)
}
