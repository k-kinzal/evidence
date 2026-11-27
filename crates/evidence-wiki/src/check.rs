//! What the wiki lacks: the state `/evidence-wiki` reads to decide what to do next.
//!
//! Every finding names the skill that fixes it. The report is deterministic for the same
//! files, so an agent can compare it before and after its work.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use aho_corasick::{AhoCorasick, MatchKind};
use evidence_core::Health;
use evidence_core::format::PageKind;
use serde::Serialize;

use crate::wiki::{View, Wiki, normalize};

/// The maintenance report.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    /// Counts of pages by kind, and of everything below.
    pub summary: Summary,
    /// What to do, most important first: one entry per kind of finding.
    pub actions: Vec<Action>,
    /// Problems reading evidence files (fix these first).
    pub problems: Vec<Problem>,
    /// Links (and categories) to pages that do not exist, by name.
    pub wanted_pages: Vec<Wanted>,
    /// Names that match several pages equally well.
    pub ambiguous: Vec<Ambiguous>,
    /// Pages whose evidence is not ok (stale or broken claims, failed productions, …).
    pub stale: Vec<Stale>,
    /// Markdown files in the pages directory without an evidence file.
    pub unpaired: Vec<String>,
    /// Categories pages name that have no page.
    pub wanted_categories: Vec<Wanted>,
    /// Category pages that do not link to all their members.
    pub outdated_categories: Vec<OutdatedCategory>,
    /// Sources nothing in the wiki uses: no page links to them or cites them.
    pub unused_sources: Vec<PageRef>,
    /// Words used on several pages that have no page of their own.
    pub term_candidates: Vec<Term>,
    /// Pages that mention another page's title or alias without linking to it.
    pub unlinked_mentions: Vec<Mention>,
    /// Pages nothing links to and that are in no category.
    pub orphans: Vec<PageRef>,
    /// Articles in no category.
    pub uncategorized: Vec<PageRef>,
    /// Articles in the pages directory with no claims and no primary ranges.
    pub uncited: Vec<PageRef>,
}

/// Counts.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    /// Visible pages.
    pub pages: usize,
    /// Articles.
    pub articles: usize,
    /// Category pages.
    pub categories: usize,
    /// Term pages.
    pub terms: usize,
    /// Pages that are not Markdown (tables, text, images, …).
    pub sources: usize,
    /// Findings in total.
    pub findings: usize,
}

/// One thing to do.
#[derive(Debug, Clone, Serialize)]
pub struct Action {
    /// The finding (`wanted_pages`, `stale`, …): the report field to read.
    pub finding: &'static str,
    /// The skill that handles it.
    pub skill: &'static str,
    /// How many.
    pub count: usize,
    /// What it means.
    pub why: &'static str,
}

/// A problem in an evidence file.
#[derive(Debug, Clone, Serialize)]
pub struct Problem {
    /// Diagnostic code.
    pub code: String,
    /// Message.
    pub message: String,
    /// File, relative to the root.
    pub file: Option<String>,
}

/// A page by id and title.
#[derive(Debug, Clone, Serialize)]
pub struct PageRef {
    /// Page id.
    pub id: String,
    /// Title.
    pub title: String,
    /// Subject path.
    pub path: String,
}

/// A missing page or category.
#[derive(Debug, Clone, Serialize)]
pub struct Wanted {
    /// The name as first written.
    pub name: String,
    /// Pages that link to it (or list it as a category).
    pub from: Vec<String>,
}

/// An ambiguous name.
#[derive(Debug, Clone, Serialize)]
pub struct Ambiguous {
    /// The name.
    pub name: String,
    /// Pages it could mean.
    pub pages: Vec<String>,
}

/// A page whose evidence needs attention.
#[derive(Debug, Clone, Serialize)]
pub struct Stale {
    /// Page id.
    pub id: String,
    /// Subject path.
    pub path: String,
    /// Health.
    pub health: Health,
    /// Why (first few reasons).
    pub reasons: Vec<String>,
}

/// A category page missing members.
#[derive(Debug, Clone, Serialize)]
pub struct OutdatedCategory {
    /// Category page id.
    pub id: String,
    /// Title.
    pub title: String,
    /// Members (and subcategories) the page does not link to.
    pub missing: Vec<String>,
}

/// A term candidate.
#[derive(Debug, Clone, Serialize)]
pub struct Term {
    /// The word.
    pub term: String,
    /// Number of pages it occurs on.
    pub pages: usize,
    /// Some of those pages.
    pub examples: Vec<String>,
    /// Why it looks like a term: `strong`, `acronym`, `katakana`, `phrase`, `kanji`.
    pub signals: Vec<&'static str>,
}

/// An unlinked mention.
#[derive(Debug, Clone, Serialize)]
pub struct Mention {
    /// The page with the mention.
    pub page: String,
    /// The page mentioned.
    pub target: String,
    /// The text that matched (title or alias).
    pub text: String,
    /// 1-based line of the first mention.
    pub line: usize,
}

fn page_ref(wiki: &Wiki, i: usize) -> PageRef {
    let p = wiki.page(i);
    PageRef {
        id: p.id.clone(),
        title: p.title.clone(),
        path: p.path.clone(),
    }
}

/// Builds the report.
pub fn check(wiki: &Wiki) -> Report {
    let cfg = &wiki.config().check;
    let limit = cfg.limit.max(1);
    let prefix = wiki.config().pages_prefix();
    let home = wiki.home();
    let mut r = Report::default();

    // Summary.
    for i in wiki.visible() {
        let p = wiki.page(i);
        r.summary.pages += 1;
        match (p.kind, p.view) {
            (PageKind::Category, _) => r.summary.categories += 1,
            (PageKind::Term, _) => r.summary.terms += 1,
            (_, View::Markdown) => r.summary.articles += 1,
            _ => r.summary.sources += 1,
        }
    }

    // Problems.
    r.problems = wiki
        .project()
        .problems()
        .iter()
        .map(|d| Problem {
            code: d.code.to_owned(),
            message: d.message.clone(),
            file: d.file.as_ref().map(|f| wiki.project().relative(f)),
        })
        .collect();

    // Wanted pages and ambiguous names.
    let mut wanted: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
    let mut ambiguous: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for i in 0..wiki.pages().len() {
        for l in wiki.links(i) {
            if l.target.is_none() && !l.file {
                let key = if l.wiki {
                    normalize(&l.written)
                } else {
                    l.written.clone()
                };
                wanted
                    .entry(key)
                    .or_insert_with(|| (l.written.clone(), BTreeSet::new()))
                    .1
                    .insert(wiki.page(i).id.clone());
            } else if l.wiki && wiki.is_ambiguous(&l.written) && wiki.by_id(&l.written).is_none() {
                ambiguous.entry(l.written.clone()).or_insert_with(|| {
                    wiki.candidates(&l.written)
                        .iter()
                        .map(|&c| wiki.page(c).id.clone())
                        .collect()
                });
            }
        }
    }
    r.wanted_pages = sorted_wanted(
        wanted
            .into_values()
            .map(|(n, f)| (n, f.into_iter().collect())),
        limit,
    );
    r.ambiguous = ambiguous
        .into_iter()
        .take(limit)
        .map(|(name, pages)| Ambiguous { name, pages })
        .collect();

    // Stale evidence.
    let statuses = wiki.project().statuses();
    for s in &statuses {
        if matches!(s.health, Health::Ok | Health::Due) {
            continue;
        }
        let Some(i) = wiki.by_id(&s.id) else { continue };
        if wiki.page(i).hidden {
            continue;
        }
        let mut reasons: Vec<String> = s.reasons.iter().map(|x| x.message.clone()).collect();
        for c in &s.claims {
            for x in &c.reasons {
                reasons.push(format!("claim {}: {}", c.id, x.message));
            }
        }
        reasons.truncate(5);
        r.stale.push(Stale {
            id: s.id.clone(),
            path: s.subject.clone(),
            health: s.health,
            reasons,
        });
    }
    r.stale
        .sort_by(|a, b| b.health.cmp(&a.health).then(a.id.cmp(&b.id)));
    r.stale.truncate(limit);

    // Unpaired Markdown files.
    r.unpaired = unpaired(wiki, &prefix, limit);

    // Categories.
    r.wanted_categories = sorted_wanted(
        wiki.wanted_categories().values().map(|(name, members)| {
            (
                name.clone(),
                members.iter().map(|&m| wiki.page(m).id.clone()).collect(),
            )
        }),
        limit,
    );
    for c in wiki.category_pages() {
        let p = wiki.page(c);
        if p.view != View::Markdown {
            continue;
        }
        let linked: HashSet<usize> = wiki.links(c).iter().filter_map(|l| l.target).collect();
        let members: Vec<usize> = wiki
            .members(c)
            .iter()
            .copied()
            .filter(|&m| !wiki.page(m).hidden)
            .collect();
        let missing: Vec<String> = members
            .iter()
            .filter(|m| !linked.contains(m))
            .map(|&m| wiki.page(m).id.clone())
            .collect();
        if !missing.is_empty() {
            r.outdated_categories.push(OutdatedCategory {
                id: p.id.clone(),
                title: p.title.clone(),
                missing,
            });
        }
    }
    r.outdated_categories.truncate(limit);

    // Unused sources: outside the pages directory, nothing links to them or uses them.
    let used: HashSet<String> = wiki
        .project()
        .pairs()
        .iter()
        .flat_map(|p| p.edges().into_iter().map(|e| e.id))
        .collect();
    for i in wiki.visible() {
        let p = wiki.page(i);
        if p.in_pages_dir(&prefix) && !prefix.is_empty() {
            continue;
        }
        if prefix.is_empty() && p.view == View::Markdown {
            continue;
        }
        if wiki.backlinks(i).is_empty() && !used.contains(&p.id) {
            r.unused_sources.push(page_ref(wiki, i));
        }
    }
    r.unused_sources.truncate(limit);

    // Terms and mentions.
    r.term_candidates = term_candidates(wiki, cfg.term_pages.max(2), limit);
    r.unlinked_mentions = unlinked_mentions(wiki, cfg.mention_chars, limit);

    // Orphans, uncategorized, uncited.
    for i in wiki.visible() {
        let p = wiki.page(i);
        if p.view != View::Markdown || Some(i) == home {
            continue;
        }
        let in_category = !wiki.categories_of(i).is_empty();
        if wiki.backlinks(i).is_empty() && !in_category && r.orphans.len() < limit {
            r.orphans.push(page_ref(wiki, i));
        }
        if p.kind != PageKind::Category && !in_category && r.uncategorized.len() < limit {
            r.uncategorized.push(page_ref(wiki, i));
        }
        if p.in_pages_dir(&prefix)
            && p.kind != PageKind::Category
            && p.claims == 0
            && !p.primary
            && r.uncited.len() < limit
        {
            r.uncited.push(page_ref(wiki, i));
        }
    }

    r.actions = actions(&r);
    r.summary.findings = r.actions.iter().map(|a| a.count).sum();
    r
}

fn sorted_wanted(items: impl Iterator<Item = (String, Vec<String>)>, limit: usize) -> Vec<Wanted> {
    let mut v: Vec<Wanted> = items.map(|(name, from)| Wanted { name, from }).collect();
    v.sort_by(|a, b| b.from.len().cmp(&a.from.len()).then(a.name.cmp(&b.name)));
    v.truncate(limit);
    v
}

fn actions(r: &Report) -> Vec<Action> {
    let mut out = Vec::new();
    let mut add = |finding, skill, count: usize, why| {
        if count > 0 {
            out.push(Action {
                finding,
                skill,
                count,
                why,
            });
        }
    };
    add(
        "problems",
        "evidence-wiki",
        r.problems.len(),
        "evidence files that cannot be read; fix them by hand first (`evidence status`)",
    );
    add(
        "stale",
        "evidence-wiki-refresh",
        r.stale.len(),
        "pages whose sources changed or whose claims are not pinned: revise the text, then pin",
    );
    add(
        "unpaired",
        "evidence-wiki-page",
        r.unpaired.len(),
        "Markdown files without an evidence file: add one (title, [wiki], claims)",
    );
    add(
        "wanted_pages",
        "evidence-wiki-page",
        r.wanted_pages.len(),
        "links to pages that do not exist: write the page, or fix the link",
    );
    add(
        "ambiguous",
        "evidence-wiki-link",
        r.ambiguous.len(),
        "names that match several pages: link by id, or rename",
    );
    add(
        "wanted_categories",
        "evidence-wiki-update-category",
        r.wanted_categories.len(),
        "categories without a page: create the category page",
    );
    add(
        "outdated_categories",
        "evidence-wiki-update-category",
        r.outdated_categories.len(),
        "category pages that do not list all their members",
    );
    add(
        "unused_sources",
        "evidence-wiki-ingest",
        r.unused_sources.len(),
        "sources no page uses: summarize them into pages with claims",
    );
    add(
        "term_candidates",
        "evidence-wiki-term",
        r.term_candidates.len(),
        "words used on several pages without a page: write term pages (or add aliases)",
    );
    add(
        "unlinked_mentions",
        "evidence-wiki-link",
        r.unlinked_mentions.len(),
        "mentions of other pages without a link",
    );
    add(
        "orphans",
        "evidence-wiki-link",
        r.orphans.len(),
        "pages nothing links to and in no category",
    );
    add(
        "uncategorized",
        "evidence-wiki-update-category",
        r.uncategorized.len(),
        "articles in no category: add `categories` to their [wiki]",
    );
    add(
        "uncited",
        "evidence-wiki-refresh",
        r.uncited.len(),
        "articles with no claims: back their statements with sources",
    );
    out
}

fn unpaired(wiki: &Wiki, prefix: &str, limit: usize) -> Vec<String> {
    let root = wiki.project().root();
    let dir = root.join(prefix.trim_end_matches('/'));
    if prefix.is_empty() || !dir.is_dir() {
        return Vec::new();
    }
    let subjects: HashSet<&str> = wiki
        .project()
        .pairs()
        .iter()
        .map(|p| p.subject_rel.as_str())
        .collect();
    let mut out = Vec::new();
    for entry in ignore::WalkBuilder::new(&dir)
        .hidden(true)
        .build()
        .flatten()
    {
        let path = entry.path();
        let is_md = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"));
        if !is_md || !path.is_file() {
            continue;
        }
        let rel = wiki.project().relative(path);
        if !subjects.contains(rel.as_str()) {
            out.push(rel);
        }
    }
    out.sort();
    out.truncate(limit);
    out
}

fn regexes() -> &'static [(regex::Regex, &'static str)] {
    static RE: std::sync::OnceLock<Vec<(regex::Regex, &'static str)>> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        [
            (r"\b[A-Z][A-Z0-9]{1,7}s?\b", "acronym"),
            (r"\b[A-Z][a-z]+(?:[ -][A-Z][a-z]+)+\b", "phrase"),
            (r"[\p{Katakana}ー・]{3,}", "katakana"),
            (r"\p{Han}{3,8}", "kanji"),
        ]
        .into_iter()
        .filter_map(|(p, s)| regex::Regex::new(p).ok().map(|r| (r, s)))
        .collect()
    })
}

fn term_candidates(wiki: &Wiki, min_pages: usize, limit: usize) -> Vec<Term> {
    // term (normalized) → (display, pages, signals)
    let mut terms: HashMap<String, (String, BTreeSet<usize>, BTreeSet<&'static str>)> =
        HashMap::new();
    for i in wiki.visible() {
        let p = wiki.page(i);
        if p.view != View::Markdown {
            continue;
        }
        let mut add = |word: &str, signal: &'static str| {
            let word = word
                .trim()
                .trim_matches(|c: char| c.is_ascii_punctuation() || c == '・');
            let n = normalize(word);
            if n.chars().count() < 2
                || n.chars().count() > 60
                || n.chars().all(|c| c.is_ascii_digit())
            {
                return;
            }
            let e = terms
                .entry(n)
                .or_insert_with(|| (word.to_owned(), BTreeSet::new(), BTreeSet::new()));
            e.1.insert(i);
            e.2.insert(signal);
        };
        for s in &p.strong {
            add(s, "strong");
        }
        for (re, signal) in regexes() {
            for m in re.find_iter(&p.plain) {
                add(m.as_str(), signal);
            }
        }
    }
    let mut out: Vec<Term> = terms
        .into_values()
        .filter(|(word, pages, _)| pages.len() >= min_pages && wiki.resolve(word).is_none())
        .map(|(term, pages, signals)| Term {
            term,
            pages: pages.len(),
            examples: pages
                .iter()
                .take(5)
                .map(|&i| wiki.page(i).id.clone())
                .collect(),
            signals: signals.into_iter().collect(),
        })
        .collect();
    // Strong text and acronyms are better signals than any run of kanji.
    let weight = |t: &Term| {
        let w = if t.signals.contains(&"strong") {
            4
        } else if t.signals.contains(&"acronym") || t.signals.contains(&"katakana") {
            3
        } else if t.signals.contains(&"phrase") {
            2
        } else {
            1
        };
        w * t.pages
    };
    out.sort_by(|a, b| weight(b).cmp(&weight(a)).then(a.term.cmp(&b.term)));
    out.truncate(limit);
    out
}

fn unlinked_mentions(wiki: &Wiki, min_chars: usize, limit: usize) -> Vec<Mention> {
    // Names worth looking for: titles and aliases of visible pages.
    let mut names: Vec<(String, usize, String)> = Vec::new();
    for i in wiki.visible() {
        let p = wiki.page(i);
        for n in std::iter::once(&p.title).chain(&p.aliases) {
            if n.chars().count() >= min_chars.max(2) {
                names.push((n.to_lowercase(), i, n.clone()));
            }
        }
    }
    if names.is_empty() {
        return Vec::new();
    }
    let Ok(ac) = AhoCorasick::builder()
        .match_kind(MatchKind::LeftmostLongest)
        .build(names.iter().map(|(n, _, _)| n.as_str()))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in wiki.visible() {
        let p = wiki.page(i);
        if p.view != View::Markdown {
            continue;
        }
        let Some(text) = &p.text else { continue };
        let linked: HashSet<usize> = wiki.links(i).iter().filter_map(|l| l.target).collect();
        let mut seen: HashSet<usize> = HashSet::new();
        let plain = p.plain.to_lowercase();
        for m in ac.find_iter(&plain) {
            let (_, target, written) = &names[m.pattern().as_usize()];
            if *target == i || linked.contains(target) || seen.contains(target) {
                continue;
            }
            if !word_bounded(&plain, m.start(), m.end()) {
                continue;
            }
            seen.insert(*target);
            let line = text
                .lines()
                .position(|l| l.to_lowercase().contains(&written.to_lowercase()))
                .map_or(0, |n| n + 1);
            out.push(Mention {
                page: p.id.clone(),
                target: wiki.page(*target).id.clone(),
                text: written.clone(),
                line,
            });
            if out.len() >= limit {
                return out;
            }
        }
    }
    out
}

/// ASCII words must not continue into neighbouring letters (`Rust` in `Rustacean`); other
/// scripts have no word boundaries.
fn word_bounded(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    let first = text[start..].chars().next();
    let last = text[..end].chars().next_back();
    let joins = |a: Option<char>, b: Option<char>| matches!((a, b), (Some(a), Some(b)) if a.is_ascii_alphanumeric() && b.is_ascii_alphanumeric());
    !joins(before, first) && !joins(last, after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_bounds() {
        assert!(word_bounded("use rust here", 4, 8));
        assert!(!word_bounded("rustacean", 0, 4));
        assert!(word_bounded("脆弱性管理", 0, 9));
    }
}
