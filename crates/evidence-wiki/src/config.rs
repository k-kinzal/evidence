//! `.evidence/wiki.toml`: the front page, where pages are written, and scheduled agents.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use evidence_core::format::Interval;
use serde::{Deserialize, Serialize};

/// File name of the wiki configuration inside `.evidence/`.
pub const CONFIG_FILE: &str = "wiki.toml";

/// The wiki configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiConfig {
    /// Language of the pages (the `lang` of the HTML), e.g. `en` or `ja`.
    #[serde(default = "default_lang")]
    pub lang: String,
    /// The front page: a page name (title, alias, id, or path). Without one, an overview is
    /// generated.
    #[serde(default)]
    pub home: Option<String>,
    /// Directory (relative to the project root) where wiki pages are written. Maintenance
    /// looks for Markdown files without evidence files here, and new pages go here.
    #[serde(default = "default_pages")]
    pub pages: String,
    /// Subject paths (prefixes, relative to the root) left out of the wiki entirely.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// The dynamic server.
    #[serde(default)]
    pub server: ServerConfig,
    /// Maintenance checks.
    #[serde(default)]
    pub check: CheckConfig,
    /// Agents the server runs on a schedule (`[[agents]]`).
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
}

impl Default for WikiConfig {
    fn default() -> Self {
        Self {
            lang: default_lang(),
            home: None,
            pages: default_pages(),
            exclude: Vec::new(),
            server: ServerConfig::default(),
            check: CheckConfig::default(),
            agents: Vec::new(),
        }
    }
}

/// `[server]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address to listen on.
    #[serde(default = "default_addr")]
    pub addr: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: default_addr(),
        }
    }
}

/// `[check]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckConfig {
    /// A word used on at least this many pages without a page of its own is a term candidate.
    #[serde(default = "default_term_pages")]
    pub term_pages: usize,
    /// Names shorter than this (in characters) are not looked for as unlinked mentions.
    #[serde(default = "default_mention_chars")]
    pub mention_chars: usize,
    /// At most this many entries per list in the report.
    #[serde(default = "default_limit")]
    pub limit: usize,
}

impl Default for CheckConfig {
    fn default() -> Self {
        Self {
            term_pages: default_term_pages(),
            mention_chars: default_mention_chars(),
            limit: default_limit(),
        }
    }
}

/// An agent run on a schedule (`[[agents]]`), e.g. `claude -p /evidence-wiki`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Stable name, unique within the file.
    pub name: String,
    /// Program and arguments (no shell), run in the project root.
    pub run: Vec<String>,
    /// Time between runs. Without it the agent runs only when asked (button, CLI, API).
    #[serde(default)]
    pub interval: Option<Interval>,
    /// Kill the agent after this long. Defaults to one hour.
    #[serde(default)]
    pub timeout: Option<Interval>,
    /// Run once when the server starts if the agent never ran or is due.
    #[serde(default)]
    pub at_start: bool,
    /// Extra environment variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Set to `false` to keep the definition without scheduling it.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn default_lang() -> String {
    "en".into()
}
fn default_pages() -> String {
    "wiki".into()
}
fn default_addr() -> String {
    "127.0.0.1:4000".into()
}
fn default_term_pages() -> usize {
    3
}
fn default_mention_chars() -> usize {
    3
}
fn default_limit() -> usize {
    50
}
fn yes() -> bool {
    true
}

impl WikiConfig {
    /// Path of the configuration file for a project root.
    pub fn path(root: &Path) -> PathBuf {
        root.join(".evidence").join(CONFIG_FILE)
    }

    /// Reads the configuration of a project, or the defaults when the file does not exist.
    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::path(root);
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    /// Parses configuration text and checks agent names.
    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        let mut seen = std::collections::BTreeSet::new();
        for agent in &config.agents {
            anyhow::ensure!(
                !agent.run.is_empty(),
                "agent `{}`: `run` is empty",
                agent.name
            );
            anyhow::ensure!(
                !agent.name.is_empty()
                    && agent
                        .name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
                "agent name `{}`: use letters, digits, `-`, `_`, `.`",
                agent.name
            );
            anyhow::ensure!(
                seen.insert(&agent.name),
                "agent `{}` is defined twice",
                agent.name
            );
        }
        Ok(config)
    }

    /// The pages directory with a trailing `/` (empty for the whole project).
    pub fn pages_prefix(&self) -> String {
        let p = self.pages.trim_matches('/');
        if p.is_empty() || p == "." {
            String::new()
        } else {
            format!("{p}/")
        }
    }
}

/// The configuration written by `evidence-wiki init`.
pub fn template(pages: &str, home: &str) -> String {
    let pages = toml::Value::String(pages.to_owned());
    let home = toml::Value::String(home.to_owned());
    format!(
        r#"# evidence-wiki configuration. Every pair of the project is a page; `[wiki]` in an
# evidence file adds aliases, categories, and the kind of page.

lang = "en"            # language of the pages: "en", "ja", …
home = {home}          # the front page (a title, alias, id, or path)
pages = {pages}       # where wiki pages are written (maintenance looks here)
# exclude = ["vendor/"]  # subject path prefixes left out of the wiki

[server]
addr = "127.0.0.1:4000"

[check]
term_pages = 3       # a word on this many pages without its own page is a term candidate
mention_chars = 3    # shorter names are not reported as unlinked mentions
limit = 50           # entries per list in `evidence-wiki check`

# Agents the server (`evidence-wiki serve`) runs on a schedule, one at a time, in the
# project root. Each run gets EVIDENCE_WIKI_URL and EVIDENCE_WIKI_MCP (the server's MCP
# endpoint). Logs: .evidence/wiki/logs/<name>/.
#
# [[agents]]
# name = "maintain"
# run = ["claude", "-p", "/evidence-wiki", "--permission-mode", "acceptEdits",
#        "--allowedTools", "Bash(evidence:*) Bash(evidence-wiki:*) Read Write Edit Glob Grep"]
# interval = "6h"
# timeout = "1h"
#
# [[agents]]
# name = "categories"
# run = ["claude", "-p", "/evidence-wiki-update-category", "--permission-mode", "acceptEdits",
#        "--allowedTools", "Bash(evidence:*) Bash(evidence-wiki:*) Read Write Edit Glob Grep"]
# interval = "1d"
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_with_defaults() {
        let c = WikiConfig::parse(&template("wiki", "home")).unwrap();
        assert_eq!(c.home.as_deref(), Some("home"));
        assert_eq!(c.pages_prefix(), "wiki/");
        assert!(c.agents.is_empty());
    }

    #[test]
    fn agents_are_checked() {
        let ok = WikiConfig::parse(
            "[[agents]]\nname = \"m\"\nrun = [\"claude\", \"-p\", \"/evidence-wiki\"]\ninterval = \"6h\"\n",
        )
        .unwrap();
        assert_eq!(ok.agents[0].interval.unwrap().0.as_secs(), 6 * 3600);
        assert!(ok.agents[0].enabled);
        assert!(WikiConfig::parse("[[agents]]\nname = \"m\"\nrun = []\n").is_err());
        assert!(
            WikiConfig::parse(
                "[[agents]]\nname = \"m\"\nrun = [\"a\"]\n[[agents]]\nname = \"m\"\nrun = [\"b\"]\n"
            )
            .is_err()
        );
        assert!(WikiConfig::parse("[[agents]]\nname = \"a b\"\nrun = [\"a\"]\n").is_err());
    }
}
