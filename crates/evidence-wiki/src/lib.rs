//! A wiki over an Evidence project.
//!
//! Every pair of the project is a page: Markdown subjects are articles, CSV/JSON/TOML
//! subjects are tables, images are images. The evidence file gives the title, description,
//! and tags; `[wiki]` adds aliases, categories, and the kind of page (`article`, `category`,
//! `term`). Pages link with `[[Name]]`, and every statement can be traced to its sources
//! through the evidence file's claims.
//!
//! - [`Wiki`]: pages, name resolution, links, backlinks, categories, tags.
//! - [`search`]: ranked search and grep; [`check`]: what the wiki lacks, and the skill that
//!   fixes each finding.
//! - [`html`] and [`export`]: the site, served ([`server`]) or written as static files.
//! - [`mcp`]: tools for agentic search, over stdio or HTTP.
//! - [`agents`]: scheduled agent runs (`claude -p /evidence-wiki`).
#![forbid(unsafe_code)]

pub mod agents;
pub mod assets;
pub mod check;
pub mod config;
pub mod export;
pub mod html;
pub mod markdown;
pub mod mcp;
pub mod scaffold;
pub mod search;
pub mod server;
pub mod table;
pub mod wiki;

pub use config::WikiConfig;
pub use wiki::{CategoryRef, Link, Page, View, Wiki};
