//! Colors (light and dark, following the system appearance).

// Colors are conventional `0xRRGGBB` codes.
#![allow(clippy::unreadable_literal)]

use std::sync::atomic::{AtomicBool, Ordering};

use evidence_core::{ClaimHealth, Health, LineStatus};
use gpui::{Hsla, rgb};

static DARK: AtomicBool = AtomicBool::new(false);

/// Monospace font for subjects and evidence files.
#[cfg(target_os = "macos")]
pub(crate) const MONO: &str = "Menlo";
#[cfg(not(target_os = "macos"))]
pub(crate) const MONO: &str = "DejaVu Sans Mono";

pub(crate) fn set_dark(dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
}

fn pick(light: u32, dark: u32) -> Hsla {
    rgb(if DARK.load(Ordering::Relaxed) {
        dark
    } else {
        light
    })
    .into()
}

pub(crate) fn bg() -> Hsla {
    pick(0xffffff, 0x1e1f22)
}
pub(crate) fn sidebar() -> Hsla {
    pick(0xf6f7f9, 0x25262a)
}
pub(crate) fn surface() -> Hsla {
    pick(0xffffff, 0x2b2d31)
}
pub(crate) fn hover() -> Hsla {
    pick(0xeceef2, 0x33353a)
}
pub(crate) fn selected() -> Hsla {
    pick(0xdde7fb, 0x2f3d57)
}
pub(crate) fn border() -> Hsla {
    pick(0xe1e4e8, 0x3a3c42)
}
pub(crate) fn text() -> Hsla {
    pick(0x1f2328, 0xe6e6e6)
}
pub(crate) fn muted() -> Hsla {
    pick(0x6e7781, 0x9198a1)
}
pub(crate) fn accent() -> Hsla {
    pick(0x0969da, 0x4c9aff)
}
pub(crate) fn selection() -> Hsla {
    accent().opacity(0.25)
}
pub(crate) fn green() -> Hsla {
    pick(0x1a7f37, 0x3fb950)
}
pub(crate) fn amber() -> Hsla {
    pick(0x9a6700, 0xd29922)
}
pub(crate) fn red() -> Hsla {
    pick(0xcf222e, 0xf85149)
}
pub(crate) fn blue() -> Hsla {
    pick(0x0969da, 0x58a6ff)
}
pub(crate) fn purple() -> Hsla {
    pick(0x8250df, 0xbc8cff)
}

pub(crate) fn health(h: Health) -> Hsla {
    match h {
        Health::Ok => green(),
        Health::Due => blue(),
        Health::Stale | Health::Modified => amber(),
        Health::Failed | Health::Missing | Health::Broken => red(),
    }
}

pub(crate) fn claim(h: ClaimHealth) -> Hsla {
    match h {
        ClaimHealth::Covered => green(),
        ClaimHealth::Primary => blue(),
        ClaimHealth::Stale => amber(),
        ClaimHealth::Broken => red(),
    }
}

pub(crate) fn line(s: LineStatus) -> Hsla {
    match s {
        LineStatus::Covered => green(),
        LineStatus::Primary => blue(),
        LineStatus::Stale => amber(),
        LineStatus::Broken => red(),
        LineStatus::Uncovered => purple(),
    }
}
