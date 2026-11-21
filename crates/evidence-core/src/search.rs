//! Full-text search over pairs without an index (the SQLite index offers the same API faster).

use serde::Serialize;

use crate::workspace::Workspace;

/// Maximum subject size scanned by the index-less search.
const MAX_SCAN: u64 = 4 * 1024 * 1024;

/// One search hit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hit {
    /// Evidence id.
    pub id: String,
    /// Content mode (`editable` or `produced`).
    pub content: String,
    /// Title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Subject path.
    pub subject: String,
    /// Where the match was found (`id`, `title`, `tags`, `description`, `path`, `content`).
    pub field: String,
    /// Matching line (content matches).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// Snippet.
    pub snippet: String,
    /// Relevance score (higher is better).
    pub score: f64,
}

/// Searches ids, titles, tags, descriptions, paths, and text subjects. Every whitespace
/// separated term must match (case-insensitive) somewhere in the pair.
pub(crate) fn search(ws: &Workspace, query: &str, limit: usize) -> Vec<Hit> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for pair in ws.pairs() {
        let f = &pair.file;
        let meta: Vec<(&str, String, f64)> = vec![
            ("id", pair.id.clone(), 5.0),
            ("title", f.title.clone().unwrap_or_default(), 4.0),
            ("tags", f.tags.join(" "), 3.0),
            (
                "description",
                f.description.clone().unwrap_or_default(),
                2.0,
            ),
            ("path", pair.subject_rel.clone(), 2.0),
        ];
        let content = if pair.format().is_text()
            && std::fs::metadata(&pair.subject_path)
                .is_ok_and(|m| m.is_file() && m.len() <= MAX_SCAN)
        {
            std::fs::read_to_string(&pair.subject_path).ok()
        } else {
            None
        };
        let haystack = format!(
            "{}\n{}",
            meta.iter()
                .map(|(_, v, _)| v.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            content.as_deref().unwrap_or_default()
        )
        .to_lowercase();
        if !terms.iter().all(|t| haystack.contains(t.as_str())) {
            continue;
        }
        let mut score = 0.0;
        let mut best: Option<(String, Option<usize>, String)> = None;
        for (field, value, weight) in &meta {
            let lower = value.to_lowercase();
            let n = terms.iter().filter(|t| lower.contains(t.as_str())).count();
            if n > 0 {
                #[allow(clippy::cast_precision_loss)]
                {
                    score += weight * n as f64;
                }
                if best.is_none() {
                    best = Some(((*field).to_owned(), None, value.clone()));
                }
            }
        }
        if let Some(text) = &content {
            for (i, line) in text.lines().enumerate() {
                let lower = line.to_lowercase();
                let n = terms.iter().filter(|t| lower.contains(t.as_str())).count();
                if n > 0 {
                    #[allow(clippy::cast_precision_loss)]
                    {
                        score += n as f64;
                    }
                    if best.as_ref().is_none_or(|(f, _, _)| f == "path") {
                        best = Some((
                            "content".into(),
                            Some(i + 1),
                            line.trim().chars().take(200).collect(),
                        ));
                    }
                }
            }
        }
        let (field, line, snippet) = best.unwrap_or_else(|| ("id".into(), None, pair.id.clone()));
        hits.push(Hit {
            id: pair.id.clone(),
            content: f.content_mode().as_str().to_owned(),
            title: f.title.clone(),
            subject: pair.subject_rel.clone(),
            field,
            line,
            snippet,
            score,
        });
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    hits.truncate(limit);
    hits
}
