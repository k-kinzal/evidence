//! Optional SQLite full-text index (feature `index`), used only by [`crate::Project::search`].
//!
//! The index is a cache of evidence files and text subjects: deleting it loses nothing, and
//! search gives the same answers without it. FTS5 with the trigram tokenizer makes substring
//! search work, also for CJK text. [`Index::sync`] is incremental: only pairs whose evidence
//! file or subject changed (by size and modification time) are re-indexed.

use std::path::Path;
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Result;
use crate::project::IndexStats;
use crate::search::Hit;
use crate::workspace::{Pair, Workspace};

const SCHEMA_VERSION: i64 = 3;
const MAX_CONTENT: u64 = 4 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct Index {
    conn: Connection,
}

impl Index {
    /// Opens (creating or migrating if needed) the index at `path`.
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let index = Self { conn };
        index.migrate()?;
        Ok(index)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        let version: Option<i64> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|v| v.parse().ok());
        if version == Some(SCHEMA_VERSION) {
            return Ok(());
        }
        // A cache: on any schema change, start over.
        self.conn.execute_batch(
            "DROP TABLE IF EXISTS pairs; DROP TABLE IF EXISTS edges; DROP TABLE IF EXISTS history;
             DROP TABLE IF EXISTS fts;
             CREATE TABLE pairs (id TEXT PRIMARY KEY, content TEXT NOT NULL, title TEXT,
                 subject TEXT NOT NULL, fingerprint TEXT NOT NULL);
             CREATE VIRTUAL TABLE fts USING fts5(
                 id UNINDEXED, title, tags, description, path, content, tokenize = 'trigram');",
        )?;
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// Brings the index up to date with the workspace (incremental).
    pub(crate) fn sync(&mut self, ws: &Workspace) -> Result<IndexStats> {
        let tx = self.conn.transaction()?;
        let mut stats = IndexStats::default();
        let existing: Vec<(String, String)> = {
            let mut stmt = tx.prepare("SELECT id, fingerprint FROM pairs")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (id, _) in &existing {
            if ws.get(id).is_none() {
                tx.execute("DELETE FROM pairs WHERE id = ?1", params![id])?;
                tx.execute("DELETE FROM fts WHERE id = ?1", params![id])?;
                stats.removed += 1;
            }
        }
        for pair in ws.pairs() {
            let fp = fingerprint(pair);
            let unchanged = existing.iter().any(|(id, f)| id == &pair.id && f == &fp);
            if unchanged {
                stats.unchanged += 1;
            } else {
                index_pair(&tx, pair, &fp)?;
                stats.indexed += 1;
            }
        }
        tx.commit()?;
        Ok(stats)
    }

    /// Drops everything and re-indexes the workspace.
    pub(crate) fn rebuild(&mut self, ws: &Workspace) -> Result<IndexStats> {
        self.conn
            .execute_batch("DELETE FROM pairs; DELETE FROM fts;")?;
        self.sync(ws)
    }

    /// Every whitespace-separated term must match. Terms shorter than three characters cannot
    /// use the trigram index; such queries return `None` (the caller scans instead).
    pub(crate) fn search(&self, query: &str, limit: usize) -> Result<Option<Vec<Hit>>> {
        let terms: Vec<&str> = query.split_whitespace().collect();
        if terms.is_empty() || terms.iter().any(|t| t.chars().count() < 3) {
            return Ok(None);
        }
        let fts_query = terms
            .iter()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.content, p.title, p.subject,
                    snippet(fts, 5, '', '', '…', 48), snippet(fts, -1, '', '', '…', 48), bm25(fts)
             FROM fts JOIN pairs p ON p.id = fts.id
             WHERE fts MATCH ?1 ORDER BY bm25(fts) LIMIT ?2",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![fts_query, limit], |r| {
            let content: String = r.get(4)?;
            let any: String = r.get(5)?;
            let field = if content.is_empty() {
                "metadata"
            } else {
                "content"
            };
            Ok(Hit {
                id: r.get(0)?,
                content: r.get(1)?,
                title: r.get(2)?,
                subject: r.get(3)?,
                field: field.into(),
                line: None,
                snippet: if content.is_empty() { any } else { content },
                score: -r.get::<_, f64>(6)?,
            })
        })?;
        Ok(Some(rows.collect::<rusqlite::Result<_>>()?))
    }
}

fn mtime(path: &Path) -> String {
    std::fs::metadata(path)
        .ok()
        .map(|m| {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{}:{t}", m.len())
        })
        .unwrap_or_default()
}

fn fingerprint(pair: &Pair) -> String {
    format!("{}|{}", mtime(&pair.evid_path), mtime(&pair.subject_path))
}

fn index_pair(tx: &rusqlite::Transaction<'_>, pair: &Pair, fp: &str) -> Result<()> {
    let f = &pair.file;
    tx.execute("DELETE FROM fts WHERE id = ?1", params![pair.id])?;
    tx.execute(
        "INSERT OR REPLACE INTO pairs (id, content, title, subject, fingerprint)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            pair.id,
            f.content_mode().as_str(),
            f.title,
            pair.subject_rel,
            fp
        ],
    )?;
    let content = if pair.format().is_text()
        && std::fs::metadata(&pair.subject_path)
            .is_ok_and(|m| m.is_file() && m.len() <= MAX_CONTENT)
    {
        std::fs::read_to_string(&pair.subject_path).unwrap_or_default()
    } else {
        String::new()
    };
    tx.execute(
        "INSERT INTO fts (id, title, tags, description, path, content) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            pair.id,
            f.title.clone().unwrap_or_default(),
            f.tags.join(" "),
            f.description.clone().unwrap_or_default(),
            format!("{} {}", pair.id, pair.subject_rel),
            content
        ],
    )?;
    Ok(())
}
