//! End to end: a small project with articles, a category, a term, a table source, and an
//! image, through `Wiki`, the check, the static site, and MCP.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

use std::path::Path;

use evidence_wiki::html::{Mode, Site};
use evidence_wiki::{Wiki, check, export, mcp, search};
use serde_json::{Value, json};

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn fixture() -> (tempfile::TempDir, Wiki) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    evidence_core::Project::init(root).unwrap();
    write(
        root,
        ".evidence/wiki.toml",
        "home = \"home\"\n[check]\nterm_pages = 2\n",
    );
    write(
        root,
        "wiki/home.md",
        "# Home\n\nStart at [[Supply chain]] or [[SBOM]].\n",
    );
    write(
        root,
        "wiki/home.md.evid",
        "version = 2\nid = \"home\"\ntitle = \"Home\"\n[[primary]]\nselect = { whole = true }\n",
    );
    write(
        root,
        "wiki/security/sbom.md",
        "# SBOM\n\nA **Software Bill of Materials** lists components. See [[Dependency Confusion]] and [[Missing page]].\n\n## Data\n\n![[vulns]]\n\n```mermaid\ngraph TD; A-->B\n```\n\nThe VEX format and CVE data matter.\n",
    );
    write(
        root,
        "wiki/security/sbom.md.evid",
        "version = 2\nid = \"security.sbom\"\ntitle = \"SBOM\"\ntags = [\"supply-chain\"]\n\n[wiki]\nkind = \"term\"\naliases = [\"Software Bill of Materials\"]\ncategories = [\"Supply chain\"]\n\n[[claims]]\nid = \"data\"\nselect = { heading = \"## Data\" }\nevidence = [\"vulns\"]\n",
    );
    write(
        root,
        "wiki/security/confusion.md",
        "# Dependency confusion\n\nAttacks on package names. An SBOM helps; so does the **Software Bill of Materials** and VEX and CVE.\n\n![diagram](img/flow.png)\n",
    );
    write(
        root,
        "wiki/security/confusion.md.evid",
        "version = 2\nid = \"security.confusion\"\n\n[wiki]\ncategories = [\"supply_chain\", \"Attacks\"]\n",
    );
    write(root, "wiki/security/img/flow.png", "not really a png");
    write(
        root,
        "wiki/categories/supply-chain.md",
        "# Supply chain\n\nPages about the software supply chain.\n\n- [[SBOM]]\n",
    );
    write(
        root,
        "wiki/categories/supply-chain.md.evid",
        "version = 2\nid = \"categories.supply-chain\"\ntitle = \"Supply chain\"\n[wiki]\nkind = \"category\"\n[[primary]]\nselect = { whole = true }\n",
    );
    write(root, "wiki/draft.md", "# Draft\n\nNo evidence file yet.\n");
    write(
        root,
        "sources/vulns.csv",
        "id,severity,score\nCVE-1,high,9.1\nCVE-2,low,2.0\nCVE-3,high,7.5\n",
    );
    write(
        root,
        "sources/vulns.csv.evid",
        "version = 2\nid = \"vulns\"\ntitle = \"Vulnerabilities\"\n",
    );
    write(root, "sources/unused.json", "[{\"a\": 1}]");
    write(
        root,
        "sources/unused.json.evid",
        "version = 2\nid = \"unused\"\n",
    );
    let wiki = Wiki::open(root).unwrap();
    (dir, wiki)
}

#[test]
fn pages_names_links_and_categories() {
    let (_dir, w) = fixture();
    let id = |name: &str| w.resolve(name).map(|i| w.page(i).id.clone());
    assert_eq!(id("sbom").as_deref(), Some("security.sbom"));
    assert_eq!(
        id("software bill of materials").as_deref(),
        Some("security.sbom")
    );
    assert_eq!(
        id("Dependency_Confusion").as_deref(),
        Some("security.confusion")
    );
    assert_eq!(
        id("wiki/security/sbom.md").as_deref(),
        Some("security.sbom")
    );
    assert_eq!(id("Missing page"), None);

    let sbom = w.by_id("security.sbom").unwrap();
    let confusion = w.by_id("security.confusion").unwrap();
    let cat = w.by_id("categories.supply-chain").unwrap();
    assert_eq!(
        w.page(confusion).title,
        "Dependency confusion",
        "title from the first heading"
    );
    assert!(w.backlinks(sbom).contains(&w.by_id("home").unwrap()));
    assert!(w.backlinks(confusion).contains(&sbom));
    assert!(w.is_category(cat));
    let mut members = w.members(cat).to_vec();
    members.sort_unstable();
    let mut want = vec![sbom, confusion];
    want.sort_unstable();
    assert_eq!(members, want, "\"supply_chain\" names the same category");
    assert!(w.wanted_categories().contains_key("attacks"));
    assert_eq!(w.home(), w.by_id("home"));
    assert!(matches!(
        w.page(w.by_id("vulns").unwrap()).view,
        evidence_wiki::View::Table(_)
    ));
    assert_eq!(w.tags()["supply-chain"], vec![sbom]);
}

#[test]
fn search_and_grep() {
    let (_dir, w) = fixture();
    let hits = search::search(&w, "software bill", &search::Filter::default(), 10);
    assert_eq!(hits[0].id, "security.sbom", "{hits:?}");
    assert!(hits[0].matched.contains(&"alias"));
    let in_cat = search::search(
        &w,
        "package",
        &search::Filter {
            category: Some("Supply chain".into()),
            ..Default::default()
        },
        10,
    );
    assert_eq!(
        in_cat.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
        vec!["security.confusion"]
    );
    assert!(
        search::search(
            &w,
            "\"bill of\" nonexistentword",
            &search::Filter::default(),
            10
        )
        .is_empty()
    );
    let lines = search::grep(&w, "cve-[0-9]", true, &search::Filter::default(), 0, 10).unwrap();
    assert!(lines.iter().any(|l| l.id == "vulns" && l.line == 2));
}

#[test]
fn check_reports_what_is_missing() {
    let (_dir, w) = fixture();
    let r = check::check(&w);
    let names = |v: &[check::Wanted]| v.iter().map(|x| x.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&r.wanted_pages), vec!["Missing page"]);
    assert_eq!(names(&r.wanted_categories), vec!["Attacks"]);
    assert_eq!(r.unpaired, vec!["wiki/draft.md"]);
    assert_eq!(r.outdated_categories.len(), 1);
    assert_eq!(r.outdated_categories[0].missing, vec!["security.confusion"]);
    assert!(r.unused_sources.iter().any(|s| s.id == "unused"));
    assert!(
        !r.unused_sources.iter().any(|s| s.id == "vulns"),
        "cited by a claim"
    );
    assert!(
        r.term_candidates.iter().any(|t| t.term == "VEX"),
        "{:?}",
        r.term_candidates
    );
    assert!(
        !r.term_candidates.iter().any(|t| t.term == "SBOM"),
        "SBOM has a page"
    );
    assert!(
        r.unlinked_mentions
            .iter()
            .any(|m| m.page == "security.confusion" && m.target == "security.sbom"),
        "{:?}",
        r.unlinked_mentions
    );
    assert!(r.uncited.iter().any(|p| p.id == "security.confusion"));
    let skills: Vec<&str> = r.actions.iter().map(|a| a.skill).collect();
    assert!(skills.contains(&"evidence-wiki-update-category"));
    assert!(skills.contains(&"evidence-wiki-page"));
    assert_eq!(
        r.summary.findings,
        r.actions.iter().map(|a| a.count).sum::<usize>()
    );
}

#[test]
fn renders_pages() {
    let (_dir, w) = fixture();
    let site = Site {
        wiki: &w,
        mode: Mode::Static,
    };
    let sbom = site.page(w.by_id("security.sbom").unwrap());
    assert!(
        sbom.html.contains("href=\"../p/security.confusion.html\""),
        "wiki link"
    );
    assert!(sbom.html.contains("wikilink missing"), "missing link");
    assert!(sbom.html.contains("class=\"datagrid\""), "embedded table");
    assert!(
        sbom.html.contains("pre class=\"mermaid\"")
            && sbom
                .html
                .contains("src=\"../assets/vendor/mermaid-11.4.1/mermaid.min.js\"")
    );
    // Nothing is loaded from another origin, and the policy says so.
    assert!(sbom.html.contains("http-equiv=\"Content-Security-Policy\""));
    for attr in ["src=\"http", "src=\"//", "href=\"//"] {
        assert!(!sbom.html.contains(attr), "{attr}");
    }
    assert!(!sbom.html.contains("<link rel=\"stylesheet\" href=\"http"));
    assert!(
        sbom.html
            .contains("Also known as <b>Software Bill of Materials</b>")
    );
    // A wiki page: nothing about the files it is made of.
    assert!(!sbom.html.contains(".md.evid"), "no file paths");
    assert!(!sbom.html.contains("info/"), "no page information");
    // Categories at the end of the page.
    assert!(sbom.html.contains(
        "<div class=\"catlinks\"><a class=\"catlinks-label\" href=\"../categories.html\">Category</a><ul><li><a href=\"../p/categories.supply-chain.html\""
    ));
    // The evidence of each statement: the section, then what it rests on.
    assert!(
        sbom.html
            .contains("<h2 class=\"section-heading\" id=\"evidence\">Evidence</h2>")
    );
    assert!(
        sbom.html
            .contains("<span class=\"evidence-claim\">Data</span>")
    );
    assert!(
        sbom.html
            .contains("<p class=\"evidence-source\">— <a href=\"../p/vulns.html\">")
    );
    let sbom_id = w.by_id("security.sbom").unwrap();
    let history = site.history(sbom_id);
    assert!(history.html.contains("aria-current=\"page\">History</a>"));
    let confusion = site.page(w.by_id("security.confusion").unwrap());
    assert!(
        confusion
            .html
            .contains("src=\"../files/wiki/security/img/flow.png\"")
    );
    assert_eq!(confusion.files, vec!["wiki/security/img/flow.png"]);
    let cat = site.page(w.by_id("categories.supply-chain").unwrap());
    assert!(!cat.html.contains("card"), "members are a list of titles");
    assert!(
        cat.html
            .contains("Pages in this category <span class=\"count\">2</span>")
    );
    let vulns = w.by_id("vulns").unwrap();
    let table = site.page(vulns);
    // A source says which statements rest on it.
    assert!(table.html.contains("id=\"evidence-for\""));
    assert!(
        table
            .html
            .contains("<a href=\"../p/security.sbom.html#data\">Data</a>"),
        "the statements that rest on it"
    );
    assert!(table.html.contains("id=\"table-main\""));
    assert!(
        !table.html.contains("data-sql"),
        "no SQL in the static site"
    );
    let dynamic = Site {
        wiki: &w,
        mode: Mode::Dynamic,
    }
    .page(w.by_id("vulns").unwrap());
    assert!(dynamic.html.contains("data-sql=\"1\""));
    assert!(site.search_index().starts_with("window.WIKI_INDEX = ["));
}

#[test]
fn builds_a_static_site() {
    let (dir, w) = fixture();
    let out = dir.path().join("_site");
    let built = export::build(&w, &out).unwrap();
    assert!(built.pages >= 10);
    for f in [
        "index.html",
        "p/security.sbom.html",
        "history/security.sbom.html",
        "search-index.js",
        "assets/wiki.js",
        "assets/wiki.css",
        "assets/vendor/mermaid-11.4.1/mermaid.min.js",
        "assets/vendor/katex-0.16.11/fonts/KaTeX_Main-Regular.woff2",
        "files/wiki/security/img/flow.png",
        "maintenance.html",
    ] {
        assert!(out.join(f).is_file(), "{f}");
    }
    let index = std::fs::read_to_string(out.join("index.html")).unwrap();
    assert!(
        index.contains("href=\"p/categories.supply-chain.html\""),
        "home links from the root"
    );
}

fn call(w: &Wiki, name: &str, args: Value) -> Value {
    let r = mcp::respond(w, &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}})).unwrap();
    r["result"].clone()
}

#[test]
fn mcp_tools() {
    let (_dir, w) = fixture();
    let init = mcp::respond(&w, &json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}})).unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("wiki_overview")
    );
    assert!(
        mcp::respond(
            &w,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )
        .is_none()
    );
    let tools = mcp::respond(
        &w,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .unwrap();
    assert!(tools["result"]["tools"].as_array().unwrap().len() >= 10);

    let overview = call(&w, "wiki_overview", json!({}));
    assert_eq!(overview["structuredContent"]["home"]["id"], "home");

    let found = call(&w, "search", json!({"query": "SBOM"}));
    assert_eq!(found["structuredContent"]["hits"][0]["id"], "security.sbom");

    let page = call(
        &w,
        "read_page",
        json!({"page": "Software Bill of Materials", "section": "Data"}),
    );
    let text = page["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("id: security.sbom")
            && text.contains("## Data")
            && !text.contains("Bill of Materials** lists"),
        "{text}"
    );

    let links = call(&w, "links", json!({"page": "security.sbom"}));
    assert_eq!(
        links["structuredContent"]["missing"][0]["name"],
        "Missing page"
    );

    let q = call(
        &w,
        "query_table",
        json!({"pages": ["vulns"], "sql": "SELECT severity, count(*) AS n FROM t GROUP BY 1 ORDER BY 1"}),
    );
    assert_eq!(
        q["structuredContent"]["rows"],
        json!([["high", 2], ["low", 1]])
    );

    let ev = call(&w, "evidence", json!({"page": "SBOM"}));
    assert_eq!(
        ev["structuredContent"]["claims"][0]["sources"][0]["id"],
        "vulns"
    );

    let missing = call(&w, "read_page", json!({"page": "Nope nope"}));
    assert_eq!(missing["isError"], true);
    let bad = mcp::respond(&w, &json!({"jsonrpc": "2.0", "id": 3, "method": "nope"})).unwrap();
    assert_eq!(bad["error"]["code"], -32601);
}

#[test]
fn new_pages_and_init() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let written = evidence_wiki::scaffold::init(
        &root,
        &evidence_wiki::scaffold::InitOptions {
            pages: "wiki".into(),
            skills: true,
        },
    )
    .unwrap();
    assert!(
        written
            .written
            .iter()
            .any(|f| f == ".claude/skills/evidence-wiki/SKILL.md")
    );
    assert!(
        written
            .written
            .iter()
            .any(|f| f == ".claude/skills/evidence-wiki-update-category/SKILL.md")
    );
    let created = evidence_wiki::scaffold::new_page(
        &root,
        &evidence_wiki::scaffold::NewPage {
            path: "wiki/terms/vex.md".into(),
            title: "VEX".into(),
            kind: Some(evidence_core::format::PageKind::Term),
            aliases: vec!["Vulnerability Exploitability eXchange".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(created.id, "terms.vex");
    let w = Wiki::open(&root).unwrap();
    assert_eq!(w.home(), w.by_id("home"));
    let vex = w.resolve("vulnerability exploitability exchange").unwrap();
    assert_eq!(w.page(vex).kind, evidence_core::format::PageKind::Term);
    assert!(
        w.project().problems().is_empty(),
        "{:?}",
        w.project().problems()
    );
    assert!(
        evidence_wiki::scaffold::new_page(
            &root,
            &evidence_wiki::scaffold::NewPage {
                path: "wiki/terms/vex.md".into(),
                title: "VEX".into(),
                ..Default::default()
            },
        )
        .is_err()
    );
}
