//! PATH dispatch and the introspection interface used by shell completion.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::CommandFactory as _;

use crate::cli::Cli;
use crate::say;

pub(crate) fn run(directory: Option<&Path>, name: &OsStr, args: &[OsString]) -> Result<ExitCode> {
    if name.is_empty()
        || name
            .as_encoded_bytes()
            .iter()
            .any(|c| matches!(c, b'/' | b'\\'))
    {
        bail!("invalid extension name: {}", name.to_string_lossy());
    }
    let mut program = OsString::from("evidence-");
    program.push(name);
    let mut child = Command::new(program);
    child.args(args);
    if let Some(directory) = directory {
        child.current_dir(directory);
    }
    let failure = || {
        format!(
            "cannot run extension `{}` from PATH",
            name.to_string_lossy()
        )
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        Err(child.exec()).with_context(failure)
    }
    #[cfg(not(unix))]
    {
        let status = child.status().with_context(failure)?;
        Ok(ExitCode::from(
            status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .unwrap_or(1),
        ))
    }
}

pub(crate) fn introspect(args: &[OsString]) -> bool {
    let Some((flag, path)) = args.split_last() else {
        return false;
    };
    if !["--list-cmds", "--list-cmds=builtins", "--completion-helper"]
        .iter()
        .any(|known| flag == *known)
    {
        return false;
    }
    let mut root = Cli::command();
    root.build();
    let mut command = &root;
    for name in path {
        let Some(next) = name.to_str().and_then(|name| command.find_subcommand(name)) else {
            return false;
        };
        command = next;
    }
    if flag == "--list-cmds" || flag == "--list-cmds=builtins" {
        let mut names: BTreeSet<_> = command
            .get_subcommands()
            .filter(|cmd| !cmd.is_hide_set())
            .map(|cmd| cmd.get_name().to_owned())
            .collect();
        if path.is_empty() && flag == "--list-cmds" {
            names.extend(on_path());
        }
        for name in names {
            say!("{name}");
        }
    } else if flag == "--completion-helper" {
        let mut flags = BTreeSet::new();
        for arg in command.get_arguments().filter(|arg| !arg.is_hide_set()) {
            if let Some(short) = arg.get_short() {
                flags.insert(format!("-{short}"));
            }
            if let Some(long) = arg.get_long() {
                flags.insert(format!("--{long}"));
            }
        }
        for flag in flags {
            say!("{flag}");
        }
    } else {
        return false;
    }
    true
}

fn on_path() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let entries = std::fs::read_dir(if directory.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            directory
        });
        for entry in entries.into_iter().flatten().flatten() {
            let file = entry.file_name();
            let Some(name) = file.to_str().and_then(|s| s.strip_prefix("evidence-")) else {
                continue;
            };
            if let Some(name) = executable_name(&entry.path(), name)
                && !name.is_empty()
                && !name.chars().any(char::is_whitespace)
                && !name.contains('\\')
            {
                names.insert(name.to_owned());
            }
        }
    }
    names
}

#[cfg(unix)]
fn executable_name<'a>(path: &Path, name: &'a str) -> Option<&'a str> {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = path.metadata().ok()?;
    (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0).then_some(name)
}

#[cfg(windows)]
fn executable_name<'a>(path: &Path, name: &'a str) -> Option<&'a str> {
    let (stem, ext) = name.rsplit_once('.')?;
    (ext.eq_ignore_ascii_case("exe") && path.is_file()).then_some(stem)
}

#[cfg(not(any(unix, windows)))]
fn executable_name<'a>(path: &Path, name: &'a str) -> Option<&'a str> {
    path.is_file().then_some(name)
}
