//! `cargo xtask vocabulary`: names in this repository must say what a thing is.
//!
//! Words that name nothing in particular (`Manager`, `Service`, `Data`, …) or that describe an
//! architecture instead of the domain (`Engine`, `Model`, `Outcome`) are not allowed in names
//! we define: package names, file and directory names, and Rust items, fields, variables, and
//! renamed imports. Calls into other crates' APIs are not checked (we do not choose those
//! names), and neither is prose in comments and documentation.
//!
//! The list and the reasoning live in AGENTS.md ("Vocabulary").

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use syn::visit::{self, Visit};

/// Banned words (singular, lowercase). Plurals are banned too.
pub(crate) const BANNED: &[&str] = &[
    // Architecture words instead of domain words.
    "engine",
    "model",
    "outcome",
    // Words that fit anything.
    "manager",
    "manage",
    "management",
    "service",
    "handler",
    "helper",
    "util",
    "utility",
    "common",
    "core",
    "misc",
    "base",
    "data",
    "info",
    "processor",
    "controller",
];

/// The one exception: the domain crate may be called `core` (AGENTS.md "Vocabulary"). It is
/// allowed only as this exact name, so `core` stays banned in every name inside it.
pub(crate) const ALLOWED: &[&str] = &["evidence-core", "evidence_core"];

/// One offending name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    /// File the name is defined in.
    pub(crate) path: PathBuf,
    /// 1-based line, when known.
    pub(crate) line: Option<usize>,
    /// What kind of name it is (`fn`, `struct`, `package`, …).
    pub(crate) what: &'static str,
    /// The name.
    pub(crate) name: String,
    /// The banned word it contains.
    pub(crate) word: &'static str,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
        }
        write!(f, ": {} `{}` uses `{}`", self.what, self.name, self.word)
    }
}

/// Splits `HTTPServerData`, `base_dir`, `evidence-model`, `x.schema.json` into lowercase words.
pub(crate) fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in name.split(|c: char| !c.is_alphanumeric()) {
        let chars: Vec<char> = part.chars().collect();
        let mut word = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let boundary = i > 0
                && c.is_uppercase()
                && (chars[i - 1].is_lowercase()
                    || chars.get(i + 1).is_some_and(|n| n.is_lowercase()));
            if boundary && !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            word.extend(c.to_lowercase());
        }
        if !word.is_empty() {
            out.push(word);
        }
    }
    out
}

/// The banned word `name` contains, if any.
pub(crate) fn banned_word(name: &str) -> Option<&'static str> {
    if ALLOWED.contains(&name) {
        return None;
    }
    words(name).iter().find_map(|w| {
        BANNED
            .iter()
            .copied()
            .find(|b| w == b || w.strip_suffix('s') == Some(b) || w.strip_suffix("es") == Some(b))
    })
}

/// Checks the repository at `root`.
pub(crate) fn check(root: &Path) -> Result<Vec<Finding>> {
    let mut out = Vec::new();
    for path in files(root)? {
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        for component in rel.components() {
            let name = component.as_os_str().to_string_lossy();
            let stem = name.split('.').next().unwrap_or_default();
            if let Some(word) = banned_word(stem) {
                out.push(Finding {
                    path: rel.clone(),
                    line: None,
                    what: "path",
                    name: name.into_owned(),
                    word,
                });
            }
        }
        match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => check_rust(&path, &rel, &mut out)?,
            _ if path.file_name().is_some_and(|n| n == "Cargo.toml") => {
                check_manifest(&path, &rel, &mut out)?;
            }
            _ => {}
        }
    }
    out.dedup();
    Ok(out)
}

/// Files we own: everything but build output, fuzz corpora, and VCS data.
fn files(root: &Path) -> Result<Vec<PathBuf>> {
    const SKIP: &[&str] = &[
        "target",
        ".git",
        "corpus",
        "artifacts",
        "coverage",
        "snapshots",
    ];
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).with_context(|| dir.display().to_string())? {
            let entry = entry?;
            let name = entry.file_name();
            if SKIP.iter().any(|s| name == *s) {
                continue;
            }
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn check_manifest(path: &Path, rel: &Path, out: &mut Vec<Finding>) -> Result<()> {
    let text = std::fs::read_to_string(path)?;
    let manifest: toml::Value = toml::from_str(&text).with_context(|| rel.display().to_string())?;
    let names = manifest
        .get("package")
        .and_then(|p| p.get("name"))
        .into_iter()
        .chain(
            manifest
                .get("bin")
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|b| b.get("name")),
        )
        .filter_map(toml::Value::as_str);
    for name in names {
        if let Some(word) = banned_word(name) {
            out.push(Finding {
                path: rel.to_path_buf(),
                line: line_of(&text, name),
                what: "package",
                name: name.to_owned(),
                word,
            });
        }
    }
    Ok(())
}

fn check_rust(path: &Path, rel: &Path, out: &mut Vec<Finding>) -> Result<()> {
    let text = std::fs::read_to_string(path)?;
    let file = syn::parse_file(&text).with_context(|| format!("parsing {}", rel.display()))?;
    let mut names = Names::default();
    names.visit_file(&file);
    for (what, name) in names.0 {
        if let Some(word) = banned_word(&name) {
            out.push(Finding {
                path: rel.to_path_buf(),
                line: line_of(&text, &name),
                what,
                name,
                word,
            });
        }
    }
    Ok(())
}

/// First line where `name` appears as a whole word.
fn line_of(text: &str, name: &str) -> Option<usize> {
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    text.lines()
        .position(|line| {
            !line.trim_start().starts_with("//")
                && line.match_indices(name).any(|(i, _)| {
                    !line[..i].chars().next_back().is_some_and(ident)
                        && !line[i + name.len()..].chars().next().is_some_and(ident)
                })
        })
        .map(|i| i + 1)
}

/// Names defined in a Rust file.
#[derive(Default)]
struct Names(Vec<(&'static str, String)>);

impl Names {
    fn add(&mut self, what: &'static str, ident: &syn::Ident) {
        self.0
            .push((what, ident.to_string().trim_start_matches("r#").to_owned()));
    }
}

impl<'ast> Visit<'ast> for Names {
    fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
        self.add("fn", &i.sig.ident);
        visit::visit_item_fn(self, i);
    }
    fn visit_impl_item_fn(&mut self, i: &'ast syn::ImplItemFn) {
        self.add("fn", &i.sig.ident);
        visit::visit_impl_item_fn(self, i);
    }
    fn visit_trait_item_fn(&mut self, i: &'ast syn::TraitItemFn) {
        self.add("fn", &i.sig.ident);
        visit::visit_trait_item_fn(self, i);
    }
    fn visit_item_struct(&mut self, i: &'ast syn::ItemStruct) {
        self.add("struct", &i.ident);
        visit::visit_item_struct(self, i);
    }
    fn visit_item_enum(&mut self, i: &'ast syn::ItemEnum) {
        self.add("enum", &i.ident);
        visit::visit_item_enum(self, i);
    }
    fn visit_variant(&mut self, i: &'ast syn::Variant) {
        self.add("variant", &i.ident);
        visit::visit_variant(self, i);
    }
    fn visit_field(&mut self, i: &'ast syn::Field) {
        if let Some(ident) = &i.ident {
            self.add("field", ident);
        }
        visit::visit_field(self, i);
    }
    fn visit_item_trait(&mut self, i: &'ast syn::ItemTrait) {
        self.add("trait", &i.ident);
        visit::visit_item_trait(self, i);
    }
    fn visit_item_type(&mut self, i: &'ast syn::ItemType) {
        self.add("type", &i.ident);
        visit::visit_item_type(self, i);
    }
    fn visit_item_const(&mut self, i: &'ast syn::ItemConst) {
        self.add("const", &i.ident);
        visit::visit_item_const(self, i);
    }
    fn visit_item_static(&mut self, i: &'ast syn::ItemStatic) {
        self.add("static", &i.ident);
        visit::visit_item_static(self, i);
    }
    fn visit_item_mod(&mut self, i: &'ast syn::ItemMod) {
        self.add("mod", &i.ident);
        visit::visit_item_mod(self, i);
    }
    fn visit_item_macro(&mut self, i: &'ast syn::ItemMacro) {
        if let Some(ident) = &i.ident {
            self.add("macro", ident);
        }
        visit::visit_item_macro(self, i);
    }
    fn visit_use_rename(&mut self, i: &'ast syn::UseRename) {
        self.add("import alias", &i.rename);
        visit::visit_use_rename(self, i);
    }
    fn visit_pat_ident(&mut self, i: &'ast syn::PatIdent) {
        self.add("binding", &i.ident);
        visit::visit_pat_ident(self, i);
    }
    fn visit_generic_param(&mut self, i: &'ast syn::GenericParam) {
        match i {
            syn::GenericParam::Type(t) => self.add("type parameter", &t.ident),
            syn::GenericParam::Const(c) => self.add("const parameter", &c.ident),
            syn::GenericParam::Lifetime(_) => {}
        }
        visit::visit_generic_param(self, i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_names_into_words() {
        assert_eq!(words("HTTPServerData"), ["http", "server", "data"]);
        assert_eq!(words("base_dir"), ["base", "dir"]);
        assert_eq!(words("evidence-model"), ["evidence", "model"]);
        assert_eq!(words("RunContext"), ["run", "context"]);
    }

    #[test]
    fn finds_banned_words_and_plurals() {
        assert_eq!(banned_word("UserManager"), Some("manager"));
        assert_eq!(banned_word("evidence-engine"), Some("engine"));
        assert_eq!(banned_word("helpers"), Some("helper"));
        assert_eq!(banned_word("Services"), Some("service"));
        assert_eq!(banned_word("database"), None, "whole words only");
        assert_eq!(banned_word("RunReport"), None);
        assert_eq!(banned_word("evidence-core"), None, "the one allowed name");
        assert_eq!(banned_word("evidence_core"), None);
        assert_eq!(banned_word("core_utils"), Some("core"));
        assert_eq!(banned_word("evidence-core-ext"), Some("core"));
    }

    #[test]
    fn finds_definitions_not_uses() {
        let file: syn::File = syn::parse_str(
            "use x::Model as M; use y::{Thing as DataThing};
             struct Pair { base_dir: u8 }
             fn run() { let info = other::core::thing(); handler(); }",
        )
        .unwrap();
        let mut names = Names::default();
        names.visit_file(&file);
        let banned: Vec<_> = names
            .0
            .iter()
            .filter_map(|(_, n)| banned_word(n).map(|w| (n.as_str(), w)))
            .collect();
        assert_eq!(
            banned,
            [
                ("DataThing", "data"),
                ("base_dir", "base"),
                ("info", "info")
            ]
        );
    }
}
