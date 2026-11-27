//! Project-local key and authenticated revision metadata.

use std::fs::{self, DirBuilder, File};
use std::io::{Read, Write};
use std::path::Path;

use cas_fs::EncryptionKey;
use zeroize::Zeroizing;

use crate::error::{Error, Result};

/// Loads the local secret, or creates it in an owner-only, Git-ignored directory.
pub(crate) fn project_key(meta: &Path, create: bool) -> Result<Option<EncryptionKey>> {
    let dir = meta.join("private");
    let key = dir.join("key");
    match fs::symlink_metadata(&dir) {
        Ok(info) if !info.is_dir() => {
            return Err(Error::Invalid(
                "private key directory is not a real directory".into(),
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !create => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            match builder.create(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !fs::symlink_metadata(&dir)
                        .map_err(|e| Error::io(&dir, e))?
                        .is_dir()
                    {
                        return Err(Error::Invalid(
                            "private key directory is not a real directory".into(),
                        ));
                    }
                }
                Err(e) => return Err(Error::io(&dir, e)),
            }
        }
        Err(e) => return Err(Error::io(&dir, e)),
    }
    // This also protects old projects with the earlier .evidence/.gitignore template.
    ignore_local(&dir, b"*\n")?;
    if let Some(bytes) = read_key(&key)? {
        return Ok(Some(EncryptionKey::new(bytes)));
    }
    if dir.join("lookup").is_dir() {
        if let Some(bytes) = read_key(&key)? {
            return Ok(Some(EncryptionKey::new(bytes)));
        }
        return Err(Error::Invalid(
            "encrypted snapshots exist but .evidence/private/key is missing".into(),
        ));
    }
    if !create {
        return Ok(None);
    }
    let mut bytes = Zeroizing::new([0; 32]);
    getrandom::fill(bytes.as_mut())
        .map_err(|_| Error::Invalid("system randomness unavailable".into()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(&dir).map_err(|e| Error::io(&dir, e))?;
    temporary
        .as_file_mut()
        .write_all(bytes.as_ref())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|e| Error::io(&key, e))?;
    match temporary.persist_noclobber(&key) {
        Ok(_) => Ok(Some(EncryptionKey::new(bytes))),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(read_key(&key)?.map(EncryptionKey::new))
        }
        Err(e) => Err(Error::io(&key, e.error)),
    }
}

/// Install local-only rules in a directory managed by Evidence, preserving an existing file.
pub(crate) fn ignore_local(dir: &Path, patterns: &[u8]) -> Result<()> {
    ensure_rules(&dir.join(".gitignore"), patterns)
}

/// Installs Git rules without replacing project-specific ones already in the file.
pub(crate) fn ensure_rules(path: &Path, patterns: &[u8]) -> Result<()> {
    let current = match fs::symlink_metadata(path) {
        Ok(info) if info.is_file() => fs::read(path).map_err(|e| Error::io(path, e))?,
        Ok(_) => {
            return Err(Error::Invalid(format!(
                "{} must be a regular file",
                path.display()
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(Error::io(path, e)),
    };
    let has_rules = current.ends_with(patterns)
        || (current.contains(&b'\r')
            && String::from_utf8_lossy(&current)
                .replace("\r\n", "\n")
                .as_bytes()
                .ends_with(patterns));
    if has_rules {
        return Ok(());
    }
    let mut rule = current;
    if !rule.is_empty() && !rule.ends_with(b"\n") {
        rule.push(b'\n');
    }
    rule.extend_from_slice(patterns);
    crate::format::document::atomic_write(path, &rule)?;
    Ok(())
}

fn read_key(path: &Path) -> Result<Option<Zeroizing<[u8; 32]>>> {
    let info = match fs::symlink_metadata(path) {
        Ok(info) => info,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(path, e)),
    };
    if !info.is_file() || info.len() != 32 {
        return Err(Error::Invalid("invalid .evidence/private/key".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if info.permissions().mode() & 0o077 != 0 {
            return Err(Error::Invalid(
                ".evidence/private/key must be readable only by its owner".into(),
            ));
        }
    }
    let mut key = Zeroizing::new([0; 32]);
    File::open(path)
        .and_then(|mut file| file.read_exact(key.as_mut()))
        .map_err(|e| Error::io(path, e))?;
    Ok(Some(key))
}

use crate::format::{self, PlainSnapshot, ProtectedState, ReplayState, State, document};
use crate::layout::Layout;

pub(crate) fn state_in(layout: &Layout, text: &str) -> Result<State, String> {
    let state = document::state_in(text)?;
    decode_state(layout, text, state)
}

pub(crate) fn writable_state(layout: &Layout, text: &str) -> Result<State, String> {
    let mut state = state_in(layout, text)?;
    if let Some(protected) = state.protected.take() {
        if protected.plain.is_some() {
            return Err(
                "the accepted snapshot is locked; restore its plaintext chunks from Git first"
                    .into(),
            );
        }
        if protected.replay.is_some() {
            return Err(
                "the accepted snapshot is locked; retrieve its exact input versions first".into(),
            );
        }
    }
    Ok(state)
}

pub(crate) fn previous_protected(text: &str) -> Option<ProtectedState> {
    document::state_in(text).ok()?.protected
}

#[derive(Default)]
pub(crate) struct SharedSnapshots {
    pub(crate) replay: Option<ReplayState>,
    pub(crate) plain: Option<PlainSnapshot>,
}

pub(crate) fn previous_shared(text: &str) -> SharedSnapshots {
    previous_protected(text).map_or_else(SharedSnapshots::default, |p| SharedSnapshots {
        replay: p.replay,
        plain: p.plain,
    })
}

fn decode_state(layout: &Layout, text: &str, mut state: State) -> Result<State, String> {
    let Some(protected) = state.protected.take() else {
        return Ok(state);
    };
    if !state.is_empty()
        || protected.version != 1
        || (protected.replay.is_some() && protected.plain.is_some())
    {
        return Err("invalid protected evidence state".into());
    }
    if let Some(key) = layout.key() {
        let context = state_context(text)?;
        if key.id() == protected.key {
            return open_state(key, &context, &protected.ciphertext);
        }
        if let Some(state) = crate::replay::cached_state(layout, &context, &protected)? {
            return Ok(state);
        }
    }
    state.protected = Some(protected);
    Ok(state)
}

pub(crate) fn replace_state(
    layout: &Layout,
    text: &str,
    state: &State,
    shared: SharedSnapshots,
) -> Result<String, String> {
    if !layout.config().config.store.encryption_enabled()
        || (state.is_empty() && shared.plain.is_none() && shared.replay.is_none())
    {
        return document::replace_state(text, state);
    }
    let key = layout.key().ok_or("project key unavailable")?;
    let context = state_context(text)?;
    let state = Zeroizing::new(serde_json::to_vec(state).map_err(|e| e.to_string())?);
    let ciphertext = key.seal(&context, &state).map_err(|e| e.to_string())?;
    document::replace_state(
        text,
        &State {
            protected: Some(ProtectedState {
                version: 1,
                key: key.id(),
                ciphertext: hex::encode(ciphertext),
                replay: shared.replay,
                plain: shared.plain,
            }),
            ..State::default()
        },
    )
}

pub(crate) fn state_context(text: &str) -> Result<Vec<u8>, String> {
    let definition = document::definition_in(text).ok_or("invalid evidence identity")?;
    let id = |key: &str| definition.get(key).and_then(toml::Value::as_str);
    serde_json::to_vec(&("evidence-state-v1", id("id"), id("member_of"), id("member")))
        .map_err(|e| e.to_string())
}

pub(crate) fn open_state(
    key: &EncryptionKey,
    context: &[u8],
    ciphertext: &str,
) -> Result<State, String> {
    let ciphertext =
        hex::decode(ciphertext).map_err(|_| "invalid protected evidence ciphertext")?;
    let plain = key.open(context, &ciphertext).map_err(|e| e.to_string())?;
    let state: State =
        serde_json::from_slice(&plain).map_err(|_| "invalid protected evidence state")?;
    if state.protected.is_some() {
        return Err("nested protected evidence state".into());
    }
    Ok(state)
}

/// Parses the public definition and unlocks locally available metadata for normal readers.
pub(crate) fn read(layout: &Layout, path: &Path) -> format::Parsed {
    let Ok(text) = fs::read_to_string(path) else {
        return document::read(path);
    };
    let mut parsed = format::parse(&text, path);
    let mut unlock = || {
        state_in(layout, &text).map_err(|e| {
            parsed
                .diagnostics
                .push(format::Diagnostic::error("protected-state", e).in_file(path));
        })
    };
    if let Some(file) = &mut parsed.file {
        if file
            .state
            .as_ref()
            .is_some_and(|state| state.protected.is_some())
            && let Ok(state) = unlock()
        {
            file.state = Some(state);
        }
    } else if let Some(member) = &mut parsed.member
        && member
            .state
            .as_ref()
            .is_some_and(|state| state.get("protected").is_some())
        && let Ok(state) = unlock()
    {
        member.state = serde_json::to_value(state).ok();
    }
    parsed
}
