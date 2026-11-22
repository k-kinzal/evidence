//! `evidence-wiki serve`: the wiki over HTTP, re-read when files change, with a JSON API,
//! MCP at `/mcp`, and the agent scheduler.
//!
//! The server is meant for one machine (it listens on 127.0.0.1 by default). Requests whose
//! `Host` is not the server's are refused (DNS rebinding), and requests that change something
//! or run code must be `POST` with `Content-Type: application/json` from the same origin.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::agents::{self, Scheduler};
use crate::check::{self, Report};
use crate::html::{Mode, Output, Site};
use crate::mcp;
use crate::search::{self, Filter};
use crate::wiki::Wiki;

/// Largest request body accepted.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// Options of `serve`.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// Address to listen on (default from the configuration).
    pub addr: Option<String>,
    /// Run the scheduled agents.
    pub agents: bool,
}

struct Shared {
    wiki: RwLock<Wiki>,
    checked: Mutex<(Instant, Instant)>,
    report: Mutex<Option<(u64, Arc<Report>)>>,
    scheduler: Option<Scheduler>,
    loopback: bool,
    port: u16,
}

impl Shared {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Wiki> {
        self.wiki.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Re-reads the wiki when files changed (checked at most every second; everything is
    /// re-read at least every minute to see new directories).
    fn refresh(&self) {
        let now = Instant::now();
        let full = {
            let mut c = self.checked.lock().unwrap_or_else(PoisonError::into_inner);
            if now.duration_since(c.0) < Duration::from_secs(1) {
                return;
            }
            c.0 = now;
            let full = now.duration_since(c.1) >= Duration::from_secs(60);
            if full {
                c.1 = now;
            }
            full
        };
        if !full && !self.read().changed() {
            return;
        }
        let mut w = self.wiki.write().unwrap_or_else(PoisonError::into_inner);
        let before = w.fingerprint();
        if let Err(e) = w.reload() {
            log(&format!("reload failed: {e:#}"));
            return;
        }
        if let Some(s) = &self.scheduler
            && before != w.fingerprint()
        {
            s.set_agents(w.config().agents.clone());
        }
    }

    fn report(&self, wiki: &Wiki) -> Arc<Report> {
        let mut cache = self.report.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((fp, r)) = cache.as_ref()
            && *fp == wiki.fingerprint()
        {
            return r.clone();
        }
        let r = Arc::new(check::check(wiki));
        *cache = Some((wiki.fingerprint(), r.clone()));
        r
    }
}

fn log(msg: &str) {
    let _ = writeln!(std::io::stderr().lock(), "evidence-wiki: {msg}");
}

/// Serves the wiki until the process is stopped.
pub fn serve(dir: &Path, opts: &ServeOptions) -> Result<()> {
    let wiki = Wiki::open(dir)?;
    let addr = opts
        .addr
        .clone()
        .unwrap_or_else(|| wiki.config().server.addr.clone());
    let server = Server::http(&addr).map_err(|e| anyhow::anyhow!("listening on {addr}: {e}"))?;
    let local = server
        .server_addr()
        .to_ip()
        .context("the server has no IP address")?;
    let host = if local.ip().is_unspecified() {
        "127.0.0.1".to_owned()
    } else {
        local.ip().to_string()
    };
    let url = format!("http://{host}:{}/", local.port());
    let root = wiki.project().root().to_path_buf();
    let scheduler = opts.agents.then(|| {
        let env = BTreeMap::from([
            ("EVIDENCE_WIKI_URL".to_owned(), url.clone()),
            ("EVIDENCE_WIKI_MCP".to_owned(), format!("{url}mcp")),
        ]);
        Scheduler::new(&root, wiki.config().agents.clone(), env)
    });
    let agents_count = wiki.config().agents.iter().filter(|a| a.enabled).count();
    let shared = Arc::new(Shared {
        wiki: RwLock::new(wiki),
        checked: Mutex::new((Instant::now(), Instant::now())),
        report: Mutex::new(None),
        scheduler,
        loopback: local.ip().is_loopback(),
        port: local.port(),
    });
    if let Some(s) = &shared.scheduler {
        s.spawn();
    }
    log(&format!(
        "serving {} at {url} (MCP: {url}mcp)",
        root.display()
    ));
    if opts.agents && agents_count > 0 {
        log(&format!(
            "{agents_count} scheduled agent(s); see {url}agents.html"
        ));
    }
    let server = Arc::new(server);
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let server = server.clone();
            let shared = shared.clone();
            std::thread::spawn(move || {
                loop {
                    match server.recv() {
                        Ok(req) => answer(&shared, req),
                        Err(e) => {
                            log(&format!("accept failed: {e}"));
                            break;
                        }
                    }
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    Ok(())
}

fn header(name: &str, value: &str) -> Header {
    #[allow(clippy::expect_used)]
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

type Reply = Response<std::io::Cursor<Vec<u8>>>;

fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Reply {
    Response::from_data(body)
        .with_status_code(status)
        .with_header(header("Content-Type", content_type))
        .with_header(header("Cache-Control", "no-cache"))
        .with_header(header("X-Content-Type-Options", "nosniff"))
}

fn html(status: u16, o: Output) -> Reply {
    bytes(status, "text/html; charset=utf-8", o.html.into_bytes())
        .with_header(header("Content-Security-Policy", crate::html::CSP))
        .with_header(header("X-Frame-Options", "SAMEORIGIN"))
        .with_header(header("Referrer-Policy", "no-referrer"))
}

/// A file of [`crate::assets`]. Versioned library paths are cached for good.
fn asset(a: &crate::assets::Asset) -> Reply {
    let reply = Response::from_data(a.bytes.to_vec())
        .with_status_code(200)
        .with_header(header("Content-Type", a.content_type))
        .with_header(header("X-Content-Type-Options", "nosniff"));
    if a.immutable {
        reply.with_header(header(
            "Cache-Control",
            "public, max-age=31536000, immutable",
        ))
    } else {
        reply.with_header(header("Cache-Control", "no-cache"))
    }
}

fn json_reply(status: u16, v: &Value) -> Reply {
    bytes(
        status,
        "application/json",
        serde_json::to_vec(v).unwrap_or_default(),
    )
}

fn error(status: u16, msg: &str) -> Reply {
    json_reply(status, &json!({"error": msg}))
}

fn mime(path: &str) -> &'static str {
    match path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("bmp") => "image/bmp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("json") => "application/json",
        Some("csv") => "text/csv; charset=utf-8",
        // HTML is shown as text: it could run script on the wiki's origin.
        Some(
            "md" | "txt" | "toml" | "yaml" | "yml" | "tsv" | "jsonl" | "log" | "html" | "htm"
            | "xml",
        ) => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn query_params(q: &str) -> BTreeMap<String, String> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            let d = |s: &str| {
                percent_encoding::percent_decode_str(&s.replace('+', " "))
                    .decode_utf8_lossy()
                    .into_owned()
            };
            (d(k), d(v))
        })
        .collect()
}

fn header_value<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

/// The host part of `Host` or of an origin URL.
fn host_of(value: &str) -> &str {
    let v = value.split("://").nth(1).unwrap_or(value);
    let v = v.split('/').next().unwrap_or(v);
    if let Some(rest) = v.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    v.rsplit_once(':').map_or(v, |(h, _)| h)
}

fn allowed_host(shared: &Shared, req: &Request) -> bool {
    if !shared.loopback {
        return true;
    }
    match header_value(req, "Host") {
        Some(h) => matches!(host_of(h), "localhost" | "127.0.0.1" | "::1"),
        None => true,
    }
}

fn same_origin(shared: &Shared, req: &Request) -> bool {
    let Some(origin) = header_value(req, "Origin") else {
        return true; // not a browser, or a same-origin request that omits it
    };
    let port = origin
        .rsplit_once(':')
        .and_then(|(_, p)| p.trim_end_matches('/').parse::<u16>().ok());
    let host_ok = match header_value(req, "Host") {
        Some(h) => host_of(h) == host_of(origin),
        None => false,
    };
    host_ok && port == Some(shared.port)
}

fn answer(shared: &Shared, mut req: Request) {
    let reply = route(shared, &mut req);
    let _ = req.respond(reply);
}

fn route(shared: &Shared, req: &mut Request) -> Reply {
    if !allowed_host(shared, req) {
        return error(403, "unexpected Host header");
    }
    let url = req.url().to_owned();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let path = percent_encoding::percent_decode_str(path)
        .decode_utf8_lossy()
        .into_owned();
    let params = query_params(query);
    let method = req.method().clone();

    if method == Method::Post {
        let json_body =
            header_value(req, "Content-Type").is_some_and(|c| c.starts_with("application/json"));
        if !json_body || !same_origin(shared, req) {
            return error(
                403,
                "POST needs Content-Type: application/json from the same origin",
            );
        }
        let mut body = Vec::new();
        if req
            .as_reader()
            .take(MAX_BODY as u64 + 1)
            .read_to_end(&mut body)
            .is_err()
            || body.len() > MAX_BODY
        {
            return error(413, "request body too large or unreadable");
        }
        return post(shared, &path, &body);
    }
    if path == "/mcp" {
        return bytes(
            405,
            "text/plain",
            b"POST JSON-RPC messages to /mcp\n".to_vec(),
        )
        .with_header(header("Allow", "POST"));
    }
    if method != Method::Get && method != Method::Head {
        return error(405, "method not allowed");
    }

    shared.refresh();
    let wiki = shared.read();
    let site = Site {
        wiki: &wiki,
        mode: Mode::Dynamic,
    };
    match path.as_str() {
        "/" | "/index.html" => html(200, site.home()),
        "/all.html" => html(200, site.all_pages()),
        "/categories.html" => html(200, site.categories()),
        "/tags.html" => html(200, site.tags()),
        "/search.html" => html(200, site.search()),
        "/maintenance.html" => html(200, site.maintenance(&shared.report(&wiki))),
        "/agents.html" => html(200, site.agents()),
        "/search-index.js" => bytes(
            200,
            "text/javascript; charset=utf-8",
            site.search_index().into_bytes(),
        ),
        "/api/version" => json_reply(200, &json!({"version": wiki.fingerprint().to_string()})),
        "/api/search" => {
            let q = params.get("q").map_or("", String::as_str);
            let limit = params
                .get("limit")
                .and_then(|l| l.parse().ok())
                .unwrap_or(20usize)
                .min(200);
            let filter = Filter {
                category: params.get("category").cloned(),
                tag: params.get("tag").cloned(),
                under: params.get("under").cloned(),
                kind: None,
            };
            json_reply(
                200,
                &json!({"hits": search::search(&wiki, q, &filter, limit)}),
            )
        }
        "/api/check" => json_reply(
            200,
            &serde_json::to_value(&*shared.report(&wiki)).unwrap_or_default(),
        ),
        "/api/pages" => {
            let pages: Vec<Value> = wiki
                .visible()
                .map(|i| {
                    let p = wiki.page(i);
                    json!({"id": p.id, "title": p.title, "kind": p.kind, "path": p.path, "summary": p.summary})
                })
                .collect();
            json_reply(200, &json!({"pages": pages}))
        }
        "/api/agents" => match &shared.scheduler {
            Some(s) => json_reply(200, &json!({"agents": s.status()})),
            None => json_reply(200, &json!({"agents": [], "disabled": true})),
        },
        p => {
            if let Some(a) = p.strip_prefix("/assets/").and_then(crate::assets::get) {
                return asset(a);
            }
            for dir in ["/p/", "/history/"] {
                if let Some(id) = p.strip_prefix(dir).and_then(|r| r.strip_suffix(".html")) {
                    return match wiki.by_id(id) {
                        Some(i) => html(
                            200,
                            match dir {
                                "/history/" => site.history(i),
                                _ => site.page(i),
                            },
                        ),
                        None => html(404, site.not_found(id, "../")),
                    };
                }
            }
            if let Some(id) = p.strip_prefix("/api/page/") {
                return match wiki.resolve(id) {
                    Some(i) => {
                        let page = wiki.page(i);
                        let mut v = serde_json::to_value(page).unwrap_or_default();
                        v["text"] = page.text.clone().map_or(Value::Null, Value::String);
                        json_reply(200, &v)
                    }
                    None => error(404, "no such page"),
                };
            }
            if let Some(name) = p
                .strip_prefix("/api/agents/")
                .and_then(|r| r.strip_suffix("/log"))
            {
                let root = wiki.project().root();
                return json_reply(
                    200,
                    &json!({"log": agents::last_log(root, name, 64 * 1024)}),
                );
            }
            if let Some(rel) = p.strip_prefix("/files/") {
                return match wiki.servable_file(rel) {
                    Some(file) => match std::fs::read(&file) {
                        // Files never run script, even SVG opened directly.
                        Ok(b) => bytes(200, mime(rel), b)
                            .with_header(header("Content-Security-Policy", "default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; sandbox")),
                        Err(e) => error(500, &e.to_string()),
                    },
                    None => error(404, "no such file"),
                };
            }
            html(404, site.not_found(p.trim_start_matches('/'), ""))
        }
    }
}

fn post(shared: &Shared, path: &str, body: &[u8]) -> Reply {
    shared.refresh();
    if path == "/mcp" {
        let msg: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => return json_reply(400, &mcp::parse_error(&e)),
        };
        let wiki = shared.read();
        return match mcp::respond_any(&wiki, &msg) {
            Some(r) => json_reply(200, &r),
            None => bytes(202, "application/json", Vec::new()),
        };
    }
    if path == "/api/query" {
        let msg: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => return error(400, &e.to_string()),
        };
        let sql = msg.get("sql").and_then(Value::as_str).unwrap_or("");
        let pages: Vec<String> = msg
            .get("pages")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let wiki = shared.read();
        return match mcp::query(&wiki, &pages, sql, 5000) {
            Ok((t, truncated)) => json_reply(
                200,
                &json!({"columns": t.columns, "rows": t.rows, "total": t.rows.len(), "truncated": truncated}),
            ),
            Err(e) => error(400, &format!("{e:#}")),
        };
    }
    if let Some(name) = path
        .strip_prefix("/api/agents/")
        .and_then(|r| r.strip_suffix("/run"))
    {
        return match &shared.scheduler {
            Some(s) => match s.request(name) {
                Ok(()) => json_reply(202, &json!({"queued": name})),
                Err(e) => error(404, &format!("{e:#}")),
            },
            None => error(409, "agents are disabled (serve --no-agents)"),
        };
    }
    error(404, "not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hosts_and_queries() {
        assert_eq!(host_of("localhost:4000"), "localhost");
        assert_eq!(host_of("http://127.0.0.1:4000"), "127.0.0.1");
        assert_eq!(host_of("[::1]:4000"), "::1");
        assert_eq!(host_of("http://evil.example"), "evil.example");
        let q = query_params("q=supply+chain&limit=5&x=%E8%84%86");
        assert_eq!(q["q"], "supply chain");
        assert_eq!(q["x"], "脆");
        assert_eq!(mime("a/b.SVG"), "image/svg+xml");
        assert_eq!(mime("x.html"), "text/plain; charset=utf-8");
        assert_eq!(mime("a.png"), "image/png");
    }
}
