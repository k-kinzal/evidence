//! Project discovery, initialization, layout, configuration, and locking.

use std::fs;
use std::path::{Path, PathBuf};

use crate::format::Config;
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};

use crate::error::{Error, Result};

/// Name of the project metadata directory.
pub(crate) const DIR: &str = ".evidence";

const LOCAL_IGNORE: &[u8] = b"/tmp/\n/cache/\n/private/\n/journal/\n/objects/tmp/\n/objects/blob/\n/objects/tree/\n/objects/sealed/\n/objects/replay/blob/\n/history/\n/lock\n/config.local.toml\n/distributed/local/\n";
const CHUNK_ATTRIBUTES: &[u8] = b"objects/plain/v2/** binary -filter -ident\nobjects/replay/v2/** binary -filter -ident -delta\nobjects/pins/v1/** binary -filter -ident -delta\ndistributed/records/** binary -filter -ident -delta\ndistributed/objects/** binary -filter -ident -delta\n";

/// Configuration layers, lowest precedence first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// `~/.config/evidence/config.toml`
    Global,
    /// `.evidence/config.toml`
    Project,
    /// `.evidence/config.local.toml`
    Local,
}

/// The effective configuration plus where each value came from.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// Merged configuration.
    pub config: Config,
    figment: Figment,
    files: Vec<(Layer, PathBuf)>,
}

impl LoadedConfig {
    /// Configuration outside any project: built-in defaults, the global file, and `EVIDENCE_*`.
    pub fn global() -> Result<Self> {
        Self::load(None)
    }

    /// Loads configuration for a project root (or only global + env when `root` is `None`).
    pub(crate) fn load(root: Option<&Path>) -> Result<Self> {
        let files = Self::paths(root);
        let mut figment = Figment::from(Serialized::defaults(Config::default()));
        for (_, path) in &files {
            if path.is_file() {
                figment = figment.merge(Toml::file_exact(path));
            }
        }
        let sections = [
            "user",
            "execution",
            "store",
            "discovery",
            "coverage",
            "remote",
            "branch",
            "daemon",
            "gc",
        ];
        figment = figment.merge(Env::prefixed("EVIDENCE_").split("__").filter(move |k| {
            let first = k
                .as_str()
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            sections.contains(&first.as_str())
        }));
        let config: Config = figment
            .extract()
            .map_err(|e| Error::Config(e.to_string()))?;
        Ok(Self {
            config,
            figment,
            files,
        })
    }

    fn paths(root: Option<&Path>) -> Vec<(Layer, PathBuf)> {
        let mut files = vec![(Layer::Global, global_config_path())];
        if let Some(root) = root {
            files.push((Layer::Project, root.join(DIR).join("config.toml")));
            files.push((Layer::Local, root.join(DIR).join("config.local.toml")));
        }
        files
    }

    fn defaults(root: &Path) -> Self {
        let config = Config::default();
        Self {
            figment: Figment::from(Serialized::defaults(&config)),
            config,
            files: Self::paths(Some(root)),
        }
    }

    /// Sets `key` to `value` in the file of `layer`, preserving its formatting, and returns
    /// that file. `value` is parsed as TOML when possible (`4`, `true`, `["a"]`), else taken as
    /// a string. The result is validated before it is written.
    pub fn set(&self, layer: Layer, key: &str, value: &str) -> Result<PathBuf> {
        let path = self.path(layer).ok_or_else(|| {
            Error::Invalid("not inside an evidence project; use the global layer".into())
        })?;
        set_config_value(path, key, value)?;
        Ok(path.to_path_buf())
    }

    /// Path of a layer's file.
    pub fn path(&self, layer: Layer) -> Option<&Path> {
        self.files
            .iter()
            .find(|(l, _)| *l == layer)
            .map(|(_, p)| p.as_path())
    }

    /// Configuration files and their layers.
    pub fn files(&self) -> &[(Layer, PathBuf)] {
        &self.files
    }

    /// Every set value as `(dotted.key, value, origin)`.
    pub fn entries(&self) -> Vec<(String, serde_json::Value, String)> {
        let value = serde_json::to_value(&self.config).unwrap_or_default();
        let mut out = Vec::new();
        flatten("", &value, &mut out);
        out.into_iter()
            .map(|(k, v)| {
                let origin = self.origin(&k);
                (k, v, origin)
            })
            .collect()
    }

    /// Describes where `key` came from.
    pub fn origin(&self, key: &str) -> String {
        let Some(meta) = self.figment.find_metadata(key) else {
            return "default".into();
        };
        match &meta.source {
            Some(figment::Source::File(path)) => {
                let layer = self
                    .files
                    .iter()
                    .find(|(_, p)| p == path)
                    .map_or("file", |(l, _)| match l {
                        Layer::Global => "global",
                        Layer::Project => "project",
                        Layer::Local => "local",
                    });
                format!("{layer} ({})", path.display())
            }
            Some(figment::Source::Code(_)) | None => "default".into(),
            Some(figment::Source::Custom(c)) => c.clone(),
            Some(_) => meta.name.to_string(),
        }
    }

    /// Looks up a dotted key in the merged configuration.
    pub fn get(&self, key: &str) -> Option<serde_json::Value> {
        let value = serde_json::to_value(&self.config).ok()?;
        key.split('.').try_fold(&value, |v, k| v.get(k)).cloned()
    }
}

fn flatten(prefix: &str, value: &serde_json::Value, out: &mut Vec<(String, serde_json::Value)>) {
    match value {
        serde_json::Value::Object(map) if !map.is_empty() => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&key, v, out);
            }
        }
        serde_json::Value::Object(_) => {}
        other => out.push((prefix.to_owned(), other.clone())),
    }
}

/// `$EVIDENCE_CONFIG_DIR/config.toml`, `$XDG_CONFIG_HOME/evidence/config.toml`, or
/// `~/.config/evidence/config.toml`.
pub(crate) fn global_config_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("EVIDENCE_CONFIG_DIR") {
        return PathBuf::from(dir).join("config.toml");
    }
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    config_home.join("evidence").join("config.toml")
}

/// Sets `key` to `value` (parsed as TOML when possible, else a string) in a config file,
/// preserving its formatting.
fn set_config_value(path: &Path, key: &str, value: &str) -> Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Error::io(path, e)),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| Error::Config(format!("{}: {e}", path.display())))?;
    let parsed = parse_toml_value(value);
    let parts: Vec<&str> = key.split('.').collect();
    let Some((last, parents)) = parts.split_last() else {
        return Err(Error::Invalid("empty key".into()));
    };
    let mut table = doc.as_table_mut();
    for part in parents {
        let entry = table
            .entry(part)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        table = entry.as_table_mut().ok_or_else(|| {
            Error::Invalid(format!("`{part}` is not a table in {}", path.display()))
        })?;
    }
    table[*last] = toml_edit::value(parsed);
    // Validate the result before writing.
    let candidate = doc.to_string();
    toml::from_str::<Config>(&candidate).map_err(|e| Error::Config(e.to_string()))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    crate::format::document::atomic_write(path, candidate.as_bytes())?;
    Ok(())
}

/// Parses a command-line value as a TOML value, falling back to a string.
fn parse_toml_value(value: &str) -> toml_edit::Value {
    format!("v = {value}")
        .parse::<toml_edit::DocumentMut>()
        .ok()
        .and_then(|d| d.get("v").and_then(|i| i.as_value().cloned()))
        .map_or_else(
            || toml_edit::Value::from(value),
            |mut v| {
                v.decor_mut().clear();
                v
            },
        )
}

/// An evidence project.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    root: PathBuf,
    config: LoadedConfig,
    key: Option<cas_fs::EncryptionKey>,
}

impl Layout {
    /// Finds the project root containing `start`.
    pub(crate) fn discover(start: &Path) -> Result<PathBuf> {
        start
            .ancestors()
            .find(|dir| dir.join(DIR).is_dir())
            .map(Path::to_path_buf)
            .ok_or_else(|| Error::NoProject(start.to_path_buf()))
    }

    /// Opens the project rooted at `root`.
    pub(crate) fn open(root: &Path) -> Result<Self> {
        let root = fs::canonicalize(root).map_err(|e| Error::io(root, e))?;
        let config = LoadedConfig::load(Some(&root))?;
        Self::with_config(root, config)
    }

    pub(crate) fn with_defaults(root: &Path) -> Result<Self> {
        Self::with_config(root.to_path_buf(), LoadedConfig::defaults(root))
    }

    fn with_config(root: PathBuf, config: LoadedConfig) -> Result<Self> {
        if !root.join(DIR).is_dir() {
            return Err(Error::NoProject(root));
        }
        let key = crate::protection::project_key(&root.join(DIR), false)?;
        Ok(Self { root, config, key })
    }

    /// Creates `.evidence/` in `root`.
    pub(crate) fn create(root: &Path) -> Result<PathBuf> {
        let root = fs::canonicalize(root).map_err(|e| Error::io(root, e))?;
        let dir = root.join(DIR);
        match fs::create_dir(&dir) {
            Ok(()) => Ok(root),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => {
                Err(Error::AlreadyInitialized(root))
            }
            Err(e) => Err(Error::io(dir, e)),
        }
    }

    /// Prepares Git rules and the local key under the caller's project lock.
    pub(crate) fn writable(&self) -> Result<Self> {
        let meta = self.meta_dir();
        crate::protection::ignore_local(&meta, LOCAL_IGNORE)?;
        crate::protection::ensure_rules(&meta.join(".gitattributes"), CHUNK_ATTRIBUTES)?;
        if self.config.config.store.path.is_some() {
            let root = self.store_dir();
            fs::create_dir_all(&root).map_err(|e| Error::io(&root, e))?;
            crate::protection::ignore_local(&root, b"/blob/\n/tree/\n/sealed/\n/tmp/\n")?;
        }
        let key =
            crate::protection::project_key(&meta, self.config.config.store.encryption_enabled())?;
        Ok(Self {
            key,
            ..self.clone()
        })
    }

    /// Project root (canonical).
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// `.evidence` directory.
    pub(crate) fn meta_dir(&self) -> PathBuf {
        self.root.join(DIR)
    }

    /// Loaded configuration.
    pub(crate) fn config(&self) -> &LoadedConfig {
        &self.config
    }

    /// Object store directory.
    pub(crate) fn store_dir(&self) -> PathBuf {
        match &self.config.config.store.path {
            Some(p) => self.root.join(p),
            None => self.meta_dir().join("objects"),
        }
    }

    /// Opens the object store.
    pub(crate) fn store(&self) -> Result<cas_fs::Store> {
        let root = self.store_dir();
        let store = cas_fs::Store::at(&root);
        if let Some(key) = &self.key {
            let canonical = match fs::canonicalize(&root) {
                Ok(path) => path,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => root,
                Err(error) => return Err(Error::io(root, error)),
            };
            let store_identity = canonical.strip_prefix(&self.root).unwrap_or(&canonical);
            let namespace =
                cas_fs::ObjectId::of(store_identity.as_os_str().as_encoded_bytes()).to_hex();
            let enabled = self.config.config.store.encryption_enabled();
            return Ok(store.with_encryption(
                key.clone(),
                self.meta_dir().join("private/lookup").join(namespace),
                enabled,
            )?);
        }
        Ok(store)
    }

    pub(crate) fn key(&self) -> Option<&cas_fs::EncryptionKey> {
        self.key.as_ref()
    }

    /// History directory.
    pub(crate) fn history_dir(&self) -> PathBuf {
        self.meta_dir().join("history")
    }

    /// Persistent cache directory of one evidence (`{cache}`).
    pub(crate) fn cache_dir(&self, id: &str) -> PathBuf {
        self.meta_dir().join("cache").join(id)
    }

    /// Temporary directory for runs.
    pub(crate) fn tmp_dir(&self) -> PathBuf {
        self.meta_dir().join("tmp")
    }

    /// Path relative to the root, with `/` separators.
    pub(crate) fn relative(&self, path: &Path) -> String {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Takes the exclusive project lock (released on drop).
    pub(crate) fn lock(&self) -> Result<LayoutLock> {
        let path = self.meta_dir().join("lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| Error::io(&path, e))?;
        match file.try_lock() {
            Ok(()) => Ok(LayoutLock { file }),
            Err(fs::TryLockError::WouldBlock) => Err(Error::Locked(path)),
            Err(fs::TryLockError::Error(e)) => Err(Error::io(&path, e)),
        }
    }
}

/// Exclusive project lock; released when dropped.
#[derive(Debug)]
pub(crate) struct LayoutLock {
    file: fs::File,
}

impl Drop for LayoutLock {
    fn drop(&mut self) {
        // Unlock explicitly instead of relying on `close`: a process spawned by another
        // thread may still hold a copy of the descriptor between `fork` and `exec`, which
        // would keep the lock alive after this one is closed.
        let _ = self.file.unlock();
    }
}
