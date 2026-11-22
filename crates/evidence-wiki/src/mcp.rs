//! MCP (Model Context Protocol) server: tools for an agent searching the wiki.
//!
//! The tools are built for *agentic search*: an agent orients itself (`wiki_overview`),
//! finds candidates (`search`, `grep`, `list_pages`, `browse`), reads only what it needs
//! (`read_page` with a section and a character budget), follows the link graph (`links`,
//! `resolve`), analyzes tables (`query_table`), and checks where statements come from
//! (`evidence`). Every result names page ids, so the next call can build on it.
//!
//! Transports: JSON-RPC over stdio (`evidence-wiki mcp`) and HTTP (`POST /mcp` on the server).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use evidence_core::format::PageKind;
use serde_json::{Value, json};

use crate::search::{self, Filter};
use crate::table;
use crate::wiki::{CategoryRef, View, Wiki, normalize, parent_dir, truncate_chars};

/// Protocol versions this server speaks, newest first.
const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// How to use the tools, sent at initialization.
pub const INSTRUCTIONS: &str = "This server is a wiki built on Evidence: every page is a file with an evidence file \
recording where its content comes from. Search it like a researcher:\n\
1. `wiki_overview` once to see categories, tags, and directories.\n\
2. `search` with a few distinctive words (titles and aliases rank first); add `category`, `tag`, or `under` to narrow. \
Use `grep` for exact strings, numbers, or identifiers, and `list_pages`/`browse` to walk a category or directory.\n\
3. `read_page` the best hits. Start with `outline_only` or a `section` to save context; follow `next_offset` for more.\n\
4. Follow `links` (outgoing, incoming, category members) when the answer spans pages; `resolve` a name you saw in text.\n\
5. For tables, `query_table` with SQL (SQLite) instead of reading rows.\n\
6. Before answering, call `evidence` on the pages you rely on: it lists the sources behind each claim and whether they are \
fresh (`ok`) or `stale`. Cite page ids and say when evidence is stale.";

fn tools() -> Value {
    let kind = json!({"type": "string", "enum": ["article", "category", "term"], "description": "Only pages of this kind."});
    let category = json!({"type": "string", "description": "Only pages in this category (its title, alias, or id)."});
    let tag = json!({"type": "string", "description": "Only pages with this tag."});
    let under = json!({"type": "string", "description": "Only pages whose file path starts with this, e.g. `wiki/security/`."});
    let page = json!({"type": "string", "description": "Page id, title, alias, or file path."});
    json!([
        {
            "name": "wiki_overview",
            "title": "Wiki overview",
            "description": "Start here. Title, page counts, the home page, categories (with sizes), tags, and top directories.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "search",
            "title": "Search pages",
            "description": "Ranked page search. Every word must match (title, aliases, tags, categories, path, description, or text), ignoring case; \"quoted phrases\" stay together. Returns ids, titles, snippets with line numbers, and which fields matched.",
            "inputSchema": {"type": "object", "properties": {
                "query": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 10},
                "category": category, "tag": tag, "under": under, "kind": kind
            }, "required": ["query"]},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "read_page",
            "title": "Read a page",
            "description": "A page's metadata (title, aliases, categories, tags, outline, link counts, evidence health) and its content as Markdown or text. Tables show their columns and first rows. Use `section` (a heading's text or anchor) or `outline_only` to read less; long content is cut at `max_chars` with a `next_offset`.",
            "inputSchema": {"type": "object", "properties": {
                "page": page,
                "section": {"type": "string", "description": "Only this section: a heading's text or anchor."},
                "outline_only": {"type": "boolean", "default": false},
                "offset": {"type": "integer", "minimum": 0, "default": 0, "description": "Start at this character."},
                "max_chars": {"type": "integer", "minimum": 200, "maximum": 100_000, "default": 12_000}
            }, "required": ["page"]},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "list_pages",
            "title": "List pages",
            "description": "Pages with one-line summaries, sorted by title, filtered by category, tag, path prefix, or kind.",
            "inputSchema": {"type": "object", "properties": {
                "category": category, "tag": tag, "under": under, "kind": kind,
                "offset": {"type": "integer", "minimum": 0, "default": 0},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50}
            }},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "browse",
            "title": "Browse a directory",
            "description": "Subdirectories (with page counts) and pages directly inside a directory of the project. Empty path is the root.",
            "inputSchema": {"type": "object", "properties": {"path": {"type": "string", "default": ""}}},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "links",
            "title": "Page links",
            "description": "The link graph around a page: pages it links to (and missing targets), pages linking to it, its categories, and, for a category, its members.",
            "inputSchema": {"type": "object", "properties": {
                "page": page,
                "direction": {"type": "string", "enum": ["out", "in", "both"], "default": "both"}
            }, "required": ["page"]},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "resolve",
            "title": "Resolve a name",
            "description": "Which page a name (as in [[Name]]) means: by id, title, alias, or path, best first. Several candidates mean the name is ambiguous.",
            "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "grep",
            "title": "Grep pages",
            "description": "Lines matching a literal string (or a regular expression with `regex`), ignoring case, across all text pages including tables. Returns page ids and line numbers.",
            "inputSchema": {"type": "object", "properties": {
                "pattern": {"type": "string"},
                "regex": {"type": "boolean", "default": false},
                "under": under,
                "context": {"type": "integer", "minimum": 0, "maximum": 5, "default": 0},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50}
            }, "required": ["pattern"]},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "query_table",
            "title": "SQL over table pages",
            "description": "Runs one read-only SQLite statement over table pages (CSV, TSV, JSON, JSONL, TOML). The first page is table `t`; every page is also a table named by its id with `.` and `-` replaced by `_`. At most 200 rows are returned.",
            "inputSchema": {"type": "object", "properties": {
                "pages": {"type": "array", "items": {"type": "string"}, "minItems": 1, "description": "Table pages (id, title, alias, or path)."},
                "sql": {"type": "string", "description": "e.g. SELECT severity, count(*) FROM t GROUP BY 1"}
            }, "required": ["pages", "sql"]},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "evidence",
            "title": "Evidence behind a page",
            "description": "Where a page's content comes from: its health (ok, stale, broken, …) with reasons, every claim (lines, the sources it cites, status, note), how the page is produced (command, inputs, locator such as a URL), and pages that cite it.",
            "inputSchema": {"type": "object", "properties": {"page": page}, "required": ["page"]},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "maintenance",
            "title": "Maintenance report",
            "description": "What the wiki lacks: wanted pages, stale evidence, missing categories, term candidates, unlinked mentions, orphans, and the skill that fixes each.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true}
        }
    ])
}

/// Answers one JSON-RPC message; `None` for notifications.
pub fn respond(wiki: &Wiki, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let id = id?; // notifications (and responses) get no answer
    let result = match method {
        "initialize" => {
            let asked = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(VERSIONS[0]);
            let version = VERSIONS
                .iter()
                .find(|v| **v == asked)
                .copied()
                .unwrap_or(VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "evidence-wiki", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            Ok(match call(wiki, name, &args) {
                Ok(r) => r.into_value(false),
                Err(e) => Reply::text(format!("Error: {e:#}")).into_value(true),
            })
        }
        "resources/list" => Ok(json!({"resources": []})),
        "prompts/list" => Ok(json!({"prompts": []})),
        other => Err((-32601, format!("method not found: {other}"))),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    })
}

/// Answers a message or a batch; `None` when nothing needs an answer.
pub fn respond_any(wiki: &Wiki, msg: &Value) -> Option<Value> {
    match msg {
        Value::Array(items) => {
            let out: Vec<Value> = items.iter().filter_map(|m| respond(wiki, m)).collect();
            (!out.is_empty()).then_some(Value::Array(out))
        }
        m => respond(wiki, m),
    }
}

/// A parse error response.
pub fn parse_error(e: &serde_json::Error) -> Value {
    json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}})
}

/// A tool result: text for the model, and the same as structured content when it is JSON.
struct Reply {
    text: String,
    structured: Option<Value>,
}

impl Reply {
    fn text(text: String) -> Self {
        Self {
            text,
            structured: None,
        }
    }
    fn json(v: Value) -> Self {
        Self {
            text: serde_json::to_string_pretty(&v).unwrap_or_default(),
            structured: Some(v),
        }
    }
    fn into_value(self, error: bool) -> Value {
        let mut v = json!({"content": [{"type": "text", "text": self.text}], "isError": error});
        if let Some(s) = self.structured.filter(Value::is_object) {
            v["structuredContent"] = s;
        }
        v
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn arg_usize(args: &Value, key: &str, default: usize, max: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(default)
        .min(max)
}

fn filter(args: &Value) -> anyhow::Result<Filter> {
    let kind = match arg_str(args, "kind") {
        None => None,
        Some("article") => Some(PageKind::Article),
        Some("category") => Some(PageKind::Category),
        Some("term") => Some(PageKind::Term),
        Some(k) => anyhow::bail!("unknown kind `{k}` (article, category, term)"),
    };
    Ok(Filter {
        category: arg_str(args, "category").map(str::to_owned),
        tag: arg_str(args, "tag").map(str::to_owned),
        under: arg_str(args, "under").map(str::to_owned),
        kind,
    })
}

fn find_page(wiki: &Wiki, name: &str) -> anyhow::Result<usize> {
    if let Some(i) = wiki.resolve(name) {
        return Ok(i);
    }
    let hits = search::search(wiki, name, &Filter::default(), 5);
    if hits.is_empty() {
        anyhow::bail!("no page `{name}`; try `search`");
    }
    anyhow::bail!(
        "no page `{name}`. Similar: {}",
        hits.iter()
            .map(|h| format!("{} ({})", h.id, h.title))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn page_brief(wiki: &Wiki, i: usize) -> Value {
    let p = wiki.page(i);
    json!({"id": p.id, "title": p.title, "kind": p.kind, "path": p.path, "summary": p.summary})
}

fn category_names(wiki: &Wiki, i: usize) -> Vec<String> {
    wiki.categories_of(i)
        .iter()
        .map(|c| match c {
            CategoryRef::Page(c) => wiki.page(*c).title.clone(),
            CategoryRef::Wanted(n) => n.clone(),
        })
        .collect()
}

fn call(wiki: &Wiki, name: &str, args: &Value) -> anyhow::Result<Reply> {
    match name {
        "wiki_overview" => Ok(Reply::json(overview(wiki))),
        "search" => {
            let query =
                arg_str(args, "query").ok_or_else(|| anyhow::anyhow!("`query` is required"))?;
            let hits = search::search(
                wiki,
                query,
                &filter(args)?,
                arg_usize(args, "limit", 10, 100),
            );
            Ok(Reply::json(
                json!({"query": query, "count": hits.len(), "hits": hits}),
            ))
        }
        "read_page" => {
            let i = find_page(wiki, arg_str(args, "page").unwrap_or(""))?;
            Ok(Reply::text(read_page(wiki, i, args)))
        }
        "list_pages" => {
            let f = filter(args)?;
            let mut pages: Vec<usize> = wiki.visible().filter(|&i| f.accepts(wiki, i)).collect();
            pages.sort_by_key(|&i| wiki.page(i).title.to_lowercase());
            let total = pages.len();
            let offset = arg_usize(args, "offset", 0, usize::MAX);
            let limit = arg_usize(args, "limit", 50, 500);
            let items: Vec<Value> = pages
                .iter()
                .skip(offset)
                .take(limit)
                .map(|&i| page_brief(wiki, i))
                .collect();
            let next = (offset + items.len() < total).then_some(offset + items.len());
            Ok(Reply::json(
                json!({"total": total, "offset": offset, "next_offset": next, "pages": items}),
            ))
        }
        "browse" => Ok(Reply::json(browse(
            wiki,
            arg_str(args, "path").unwrap_or(""),
        ))),
        "links" => {
            let i = find_page(wiki, arg_str(args, "page").unwrap_or(""))?;
            let dir = arg_str(args, "direction").unwrap_or("both");
            Ok(Reply::json(links(wiki, i, dir)))
        }
        "resolve" => {
            let n = arg_str(args, "name").ok_or_else(|| anyhow::anyhow!("`name` is required"))?;
            let c: Vec<Value> = wiki
                .candidates(n)
                .iter()
                .map(|&i| page_brief(wiki, i))
                .collect();
            Ok(Reply::json(
                json!({"name": n, "ambiguous": wiki.is_ambiguous(n), "pages": c}),
            ))
        }
        "grep" => {
            let pattern =
                arg_str(args, "pattern").ok_or_else(|| anyhow::anyhow!("`pattern` is required"))?;
            let regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
            let lines = search::grep(
                wiki,
                pattern,
                regex,
                &filter(args)?,
                arg_usize(args, "context", 0, 5),
                arg_usize(args, "limit", 50, 500),
            )?;
            Ok(Reply::json(
                json!({"pattern": pattern, "count": lines.len(), "lines": lines}),
            ))
        }
        "query_table" => {
            let sql = arg_str(args, "sql").ok_or_else(|| anyhow::anyhow!("`sql` is required"))?;
            let names: Vec<String> = match args.get("pages") {
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                Some(Value::String(s)) => vec![s.clone()],
                _ => Vec::new(),
            };
            let (t, truncated) = query(wiki, &names, sql, 200)?;
            Ok(Reply::json(
                json!({"columns": t.columns, "rows": t.rows, "truncated": truncated}),
            ))
        }
        "evidence" => {
            let i = find_page(wiki, arg_str(args, "page").unwrap_or(""))?;
            Ok(Reply::json(evidence(wiki, i)))
        }
        "maintenance" => {
            let r = crate::check::check(wiki);
            Ok(Reply::json(serde_json::to_value(r)?))
        }
        other => anyhow::bail!("unknown tool `{other}`"),
    }
}

/// Runs a tool (as `tools/call` does): its text for a person or model, and its JSON when
/// the tool returns JSON.
pub fn tool(wiki: &Wiki, name: &str, args: &Value) -> anyhow::Result<(String, Option<Value>)> {
    call(wiki, name, args).map(|r| (r.text, r.structured))
}

/// Names of the tools.
pub fn tool_names() -> Vec<String> {
    tools()
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Runs SQL over table pages (the first is also `t`).
pub fn query(
    wiki: &Wiki,
    names: &[String],
    sql: &str,
    max_rows: usize,
) -> anyhow::Result<(table::Table, bool)> {
    anyhow::ensure!(!names.is_empty(), "name at least one table page in `pages`");
    let mut tables = Vec::new();
    for (k, n) in names.iter().enumerate() {
        let i = find_page(wiki, n)?;
        let t = wiki.table(i)?;
        tables.push((k, wiki.page(i).id.clone(), t));
    }
    let mut named: Vec<(String, &table::Table)> = Vec::new();
    for (k, id, t) in &tables {
        if *k == 0 {
            named.push(("t".to_owned(), t));
        }
        let name = table::sql_name(id);
        if name != "t" {
            named.push((name, t));
        }
    }
    table::query(&named, sql, max_rows)
}

fn overview(wiki: &Wiki) -> Value {
    let cats: Vec<Value> = wiki
        .category_pages()
        .into_iter()
        .filter(|&c| !wiki.page(c).hidden)
        .map(|c| json!({"id": wiki.page(c).id, "title": wiki.page(c).title, "pages": wiki.members(c).len(), "summary": wiki.page(c).summary}))
        .collect();
    let wanted: Vec<Value> = wiki
        .wanted_categories()
        .values()
        .map(|(n, m)| json!({"name": n, "pages": m.len()}))
        .collect();
    let mut tags: Vec<(String, usize)> =
        wiki.tags().into_iter().map(|(t, p)| (t, p.len())).collect();
    tags.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut dirs: BTreeMap<String, usize> = BTreeMap::new();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for i in wiki.visible() {
        let p = wiki.page(i);
        let segs: Vec<&str> = parent_dir(&p.path)
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        for depth in 1..=segs.len().min(2) {
            *dirs.entry(segs[..depth].join("/")).or_default() += 1;
        }
        let k = match (p.kind, p.view) {
            (PageKind::Article, View::Markdown) => "article".to_owned(),
            (k, View::Markdown) => k.as_str().to_owned(),
            (_, View::Table(_)) => "table".to_owned(),
            (_, v) => serde_json::to_value(v)
                .ok()
                .and_then(|v| v["type"].as_str().map(str::to_owned))
                .unwrap_or_default(),
        };
        *kinds.entry(k).or_default() += 1;
    }
    json!({
        "pages": wiki.visible().count(),
        "by_kind": kinds,
        "home": wiki.home().map(|h| page_brief(wiki, h)),
        "categories": cats,
        "categories_without_page": wanted,
        "tags": tags.into_iter().take(100).map(|(t, n)| json!({"tag": t, "pages": n})).collect::<Vec<_>>(),
        "directories": dirs.into_iter().map(|(d, n)| json!({"path": format!("{d}/"), "pages": n})).collect::<Vec<_>>(),
    })
}

fn read_page(wiki: &Wiki, i: usize, args: &Value) -> String {
    let p = wiki.page(i);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "id: {}\ntitle: {}\nkind: {}\npath: {}",
        p.id, p.title, p.kind, p.path
    );
    if !p.aliases.is_empty() {
        let _ = writeln!(out, "aliases: {}", p.aliases.join(", "));
    }
    let cats = category_names(wiki, i);
    if !cats.is_empty() {
        let _ = writeln!(out, "categories: {}", cats.join(", "));
    }
    if !p.tags.is_empty() {
        let _ = writeln!(out, "tags: {}", p.tags.join(", "));
    }
    if let Some(d) = &p.description {
        let _ = writeln!(out, "description: {d}");
    }
    if let Some(pair) = wiki.project().pair(&p.id) {
        let s = wiki.project().status(pair);
        let _ = writeln!(
            out,
            "evidence: {} ({} claims; call `evidence` for sources)",
            s.health,
            s.claims.len()
        );
    }
    let outgoing = wiki
        .links(i)
        .iter()
        .filter(|l| l.target.is_some_and(|t| t != i))
        .count();
    let _ = writeln!(out, "links: {outgoing} out, {} in", wiki.backlinks(i).len());
    if wiki.is_category(i) {
        let _ = writeln!(
            out,
            "members: {} (call `links` to list them)",
            wiki.members(i).len()
        );
    }
    if !p.headings.is_empty() {
        out.push_str("outline:\n");
        for h in &p.headings {
            let _ = writeln!(
                out,
                "  {}{} (#{}, line {})",
                "  ".repeat(usize::from(h.level.saturating_sub(1))),
                h.text,
                h.anchor,
                h.line
            );
        }
    }
    if args
        .get("outline_only")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return out;
    }

    let body = match p.view {
        View::Table(_) => match wiki.table(i) {
            Ok(t) => format!(
                "table: {} rows × {} columns\ncolumns: {}\n(first rows as CSV; use `query_table` with pages=[\"{}\"] for more)\n\n{}",
                t.rows.len(),
                t.columns.len(),
                t.columns.join(", "),
                p.id,
                t.to_csv(30)
            ),
            Err(e) => format!(
                "(cannot read as a table: {e:#})\n\n{}",
                p.text.as_deref().unwrap_or("")
            ),
        },
        View::Markdown | View::Text => {
            let text = p.text.as_deref().unwrap_or("");
            if let Some(s) = arg_str(args, "section") {
                let Some(t) = section(wiki, i, text, s) else {
                    let _ = writeln!(out, "\n(no section `{s}`; see the outline)");
                    return out;
                };
                t
            } else {
                text.to_owned()
            }
        }
        View::Image => "(an image)".to_owned(),
        View::Directory => "(a directory; use `browse`)".to_owned(),
        View::Binary => "(binary or too large to show)".to_owned(),
        View::Missing => "(the file does not exist)".to_owned(),
    };
    let offset = arg_usize(args, "offset", 0, usize::MAX);
    let max = arg_usize(args, "max_chars", 12_000, 100_000).max(200);
    let total = body.chars().count();
    let piece: String = body.chars().skip(offset).take(max).collect();
    let _ = writeln!(out, "\n---\n{piece}");
    if offset + max < total {
        let _ = writeln!(
            out,
            "\n--- cut at character {} of {total}; continue with offset={} ---",
            offset + max,
            offset + max
        );
    }
    out
}

fn section(wiki: &Wiki, i: usize, text: &str, name: &str) -> Option<String> {
    let p = wiki.page(i);
    let want = normalize(name.trim_start_matches('#'));
    let k = p
        .headings
        .iter()
        .position(|h| h.anchor == name.trim_start_matches('#') || normalize(&h.text) == want)?;
    let h = &p.headings[k];
    let end = p.headings[k + 1..]
        .iter()
        .find(|n| n.level <= h.level)
        .map_or(usize::MAX, |n| n.line - 1);
    let lines: Vec<&str> = text.lines().collect();
    let from = h.line.saturating_sub(1);
    let to = end.min(lines.len());
    Some(lines.get(from..to)?.join("\n"))
}

fn browse(wiki: &Wiki, path: &str) -> Value {
    let path = path.trim_matches('/');
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}/")
    };
    let mut dirs: BTreeMap<String, usize> = BTreeMap::new();
    let mut pages = Vec::new();
    for i in wiki.visible() {
        let p = wiki.page(i);
        let Some(rest) = p.path.strip_prefix(&prefix) else {
            continue;
        };
        match rest.split_once('/') {
            Some((d, _)) => *dirs.entry(d.to_owned()).or_default() += 1,
            None => pages.push(page_brief(wiki, i)),
        }
    }
    json!({
        "path": prefix,
        "directories": dirs.into_iter().map(|(d, n)| json!({"path": format!("{prefix}{d}/"), "pages": n})).collect::<Vec<_>>(),
        "pages": pages,
    })
}

fn links(wiki: &Wiki, i: usize, dir: &str) -> Value {
    let mut v = json!({"page": page_brief(wiki, i), "categories": category_names(wiki, i)});
    if dir != "in" {
        let mut seen = Vec::new();
        let mut out = Vec::new();
        let mut missing = Vec::new();
        for l in wiki.links(i) {
            match l.target {
                Some(t) if t != i => {
                    if !seen.contains(&t) {
                        seen.push(t);
                        out.push(json!({"id": wiki.page(t).id, "title": wiki.page(t).title, "line": l.line}));
                    }
                }
                None if !l.file => missing.push(json!({"name": l.written, "line": l.line})),
                _ => {}
            }
        }
        v["outgoing"] = Value::Array(out);
        v["missing"] = Value::Array(missing);
    }
    if dir != "out" {
        v["incoming"] = Value::Array(
            wiki.backlinks(i)
                .iter()
                .map(|&b| page_brief(wiki, b))
                .collect(),
        );
        let cited_by: Vec<Value> = wiki
            .project()
            .dependents(&wiki.page(i).id)
            .iter()
            .filter_map(|d| wiki.by_id(&d.id))
            .map(|d| page_brief(wiki, d))
            .collect();
        v["cited_by"] = Value::Array(cited_by);
    }
    if wiki.is_category(i) {
        v["members"] = Value::Array(
            wiki.members(i)
                .iter()
                .map(|&m| page_brief(wiki, m))
                .collect(),
        );
    }
    v
}

fn evidence(wiki: &Wiki, i: usize) -> Value {
    let p = wiki.page(i);
    let Some(pair) = wiki.project().pair(&p.id) else {
        return json!({"id": p.id, "error": "no evidence file"});
    };
    let s = wiki.project().status(pair);
    let title = |id: &str| wiki.by_id(id).map(|t| wiki.page(t).title.clone());
    let claims: Vec<Value> = s
        .claims
        .iter()
        .map(|c| {
            let excerpt = c.lines.and_then(|(a, b)| {
                let text = p.text.as_deref()?;
                let lines: Vec<&str> = text.lines().collect();
                Some(truncate_chars(&lines.get(a.saturating_sub(1)..b.min(lines.len()))?.join("\n"), 400))
            });
            json!({
                "id": c.id,
                "health": c.health.as_str(),
                "lines": c.lines.map(|(a, b)| format!("{a}-{b}")),
                "text": excerpt,
                "sources": c.references.iter().map(|r| json!({"id": r.id, "title": title(&r.id), "ref": r.key, "revision": r.revision})).collect::<Vec<_>>(),
                "note": c.note,
                "reasons": c.reasons.iter().map(|r| r.message.clone()).collect::<Vec<_>>(),
                "pinned_at": c.pinned_at.map(|t| t.to_string()),
            })
        })
        .collect();
    let productions: Vec<Value> = s
        .productions
        .iter()
        .map(|prod| {
            let def = pair.file.production(&prod.name);
            let locator = pair.file.state.as_ref().and_then(|st| st.runs.get(&prod.name)).map(|r| r.locator.clone());
            json!({
                "name": prod.name,
                "run": def.map(|d| d.run.clone()),
                "inputs": def.map(|d| d.inputs.iter().map(|r| r.id().to_owned()).collect::<Vec<_>>()),
                "locator": locator,
                "last_success": prod.succeeded_at.map(|t| t.to_string()),
                "reasons": prod.reasons.iter().map(|r| r.message.clone()).collect::<Vec<_>>(),
            })
        })
        .collect();
    let cited_by: Vec<String> = wiki
        .project()
        .dependents(&p.id)
        .iter()
        .map(|d| d.id.clone())
        .collect();
    json!({
        "id": p.id,
        "title": p.title,
        "health": s.health,
        "content": s.content,
        "reasons": s.reasons.iter().map(|r| r.message.clone()).collect::<Vec<_>>(),
        "claims": claims,
        "primary": p.primary,
        "productions": productions,
        "cited_by": cited_by,
        "author": p.author,
    })
}
