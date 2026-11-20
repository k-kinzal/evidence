//! Configuration file types (`config.toml`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::format::evid::Interval;

/// Effective configuration (all layers merged).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    title = "Evidence configuration",
    description = "Configuration of the `evidence` command (global, project, or project-local layer)."
)]
pub struct Config {
    /// Identity recorded in reviews.
    #[serde(default)]
    pub user: UserConfig,
    /// How runs are executed.
    #[serde(default)]
    pub execution: ExecutionConfig,
    /// Snapshot object store.
    #[serde(default)]
    pub store: StoreConfig,
    /// Discovery of evidence files.
    #[serde(default)]
    pub discovery: DiscoveryConfig,
    /// Optional SQLite index.
    #[serde(default)]
    pub index: IndexConfig,
}

/// `[user]`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    /// Name recorded as `reviewed_by`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `[execution]`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionConfig {
    /// Maximum number of pairs updated in parallel. Defaults to the number of CPUs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jobs: Option<usize>,
    /// Default timeout of a single command (e.g. `10m`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<Interval>,
}

/// `[store]`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    /// Object store directory. Relative paths are resolved against the project root.
    /// Defaults to `.evidence/objects`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// `[discovery]`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryConfig {
    /// Additional gitignore-style patterns of paths to skip when looking for evidence files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<String>,
}

/// `[index]`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IndexConfig {
    /// Maintain `.evidence/index.db` automatically after changes. Defaults to `true`
    /// when the binary is built with index support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}
