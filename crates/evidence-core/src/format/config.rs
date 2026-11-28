//! Configuration file types (`config.toml`).

use std::collections::BTreeMap;

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
    /// Coverage: when a claim or a reference is too broad to count as precise evidence.
    #[serde(default)]
    pub coverage: CoverageConfig,
    /// Named peers for P2P synchronization.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub remote: BTreeMap<String, RemoteConfig>,
    /// Optional upstream for each branch.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub branch: BTreeMap<String, BranchConfig>,
    /// TCP listener settings.
    #[serde(default, skip_serializing_if = "DaemonConfig::is_default")]
    pub daemon: DaemonConfig,
    /// Object maintenance settings.
    #[serde(default, skip_serializing_if = "GcConfig::is_default")]
    pub gc: GcConfig,
}

/// `[coverage]`
///
/// A claim whose range, or a reference whose selection, takes more than `broad_share` of a
/// file's non-blank lines is *broad*: one claim backing a whole document, or one reference
/// using a whole document, raises coverage without saying which part supports what. Broad
/// claims and references are reported, and coverage is also given without them (`narrow`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverageConfig {
    /// Share (0.0–1.0) of a file's non-blank lines above which a range is broad. Defaults to
    /// `0.5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub broad_share: Option<f64>,
    /// Files with fewer non-blank lines are never broad (selecting all of a short file is
    /// precise). Defaults to `40`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broad_min_lines: Option<usize>,
}

impl CoverageConfig {
    /// Default of `broad_share`.
    pub const DEFAULT_BROAD_SHARE: f64 = 0.5;
    /// Default of `broad_min_lines`.
    pub const DEFAULT_BROAD_MIN_LINES: usize = 40;

    /// Whether `lines` non-blank lines of a file of `total` non-blank lines are broad.
    #[allow(clippy::cast_precision_loss)]
    pub fn is_broad(&self, lines: usize, total: usize) -> bool {
        total
            >= self
                .broad_min_lines
                .unwrap_or(Self::DEFAULT_BROAD_MIN_LINES)
            && lines as f64 > self.broad_share.unwrap_or(Self::DEFAULT_BROAD_SHARE) * total as f64
    }
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
    /// Encrypt newly written snapshots and managed metadata. Defaults to `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<bool>,
}

impl StoreConfig {
    /// Whether new objects and metadata are encrypted.
    pub fn encryption_enabled(&self) -> bool {
        self.encryption.unwrap_or(true)
    }
}

/// `[discovery]`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryConfig {
    /// Additional gitignore-style patterns of paths to skip when looking for evidence files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<String>,
}

/// A peer configured under `[remote.NAME]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteConfig {
    /// A project-relative path, absolute path, or `tcp://HOST[:PORT]`.
    pub url: String,
}

/// Upstream policy under `[branch.NAME]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BranchConfig {
    /// Only adopt remote records observed through this named peer.
    pub remote: Option<String>,
}

/// TCP write policy. The listener is read-only by default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    /// Enable incoming pushes for the addresses in `write`.
    pub receive: bool,
    /// Per-branch IP addresses or CIDRs. `*` is the fallback.
    pub write: BTreeMap<String, Vec<String>>,
    /// Maximum bytes accepted per push (default: 256 MiB).
    pub push_bytes: Option<u64>,
    /// Maximum cumulative bytes accepted from one IP address (default: 4 GiB).
    pub peer_bytes: Option<u64>,
}

impl DaemonConfig {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// Local maintenance policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GcConfig {
    /// Verify and collect eligible objects periodically after updates.
    pub auto: bool,
}

impl GcConfig {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}
