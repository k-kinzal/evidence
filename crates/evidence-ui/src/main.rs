//! Evidence desktop browser: `evidence-ui [PROJECT_DIR]`.
//!
//! A wiki-like view of an Evidence project: every pair with its freshness, the subject with
//! coverage highlighting, the evidence file, upstream/downstream navigation, and history with
//! snapshot diffs. It reads the same files as the CLI (no index needed) and refreshes
//! automatically when they change.
#![forbid(unsafe_code)]

mod input;
mod theme;

use std::path::PathBuf;
use std::time::Duration;

use evidence_core::format::ContentMode;
use evidence_core::{Event, Health};
use evidence_ui::{Browser, Content, Detail, DiffLine, Filter, PairRow};
use gpui::{
    AnyElement, App, Application, Bounds, ClickEvent, Context, Entity, FocusHandle, Focusable,
    FontWeight, Hsla, KeyBinding, Menu, MenuItem, SharedString, Subscription, Task,
    TitlebarOptions, UniformListScrollHandle, Window, WindowAppearance, WindowBounds,
    WindowOptions, actions, div, prelude::*, px, size, uniform_list,
};

use crate::input::{Changed, TextInput};

actions!(evidence_ui, [Quit, Reload, FocusSearch, Back]);

const ROW_HEIGHT: f32 = 46.;
const LINE_HEIGHT: f32 = 20.;
const POLL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Content,
    Evidence,
    Graph,
    History,
}

impl Tab {
    const ALL: [Self; 4] = [Self::Content, Self::Evidence, Self::Graph, Self::History];

    fn label(self) -> &'static str {
        match self {
            Self::Content => "Content",
            Self::Evidence => "Evidence",
            Self::Graph => "Graph",
            Self::History => "History",
        }
    }
}

struct EvidenceApp {
    path: PathBuf,
    browser: Result<Browser, String>,
    fingerprint: u64,
    filter: Filter,
    search: Entity<TextInput>,
    selected: Option<String>,
    detail: Option<Detail>,
    tab: Tab,
    back: Vec<String>,
    diff: Option<(usize, Result<Vec<DiffLine>, String>)>,
    list_scroll: UniformListScrollHandle,
    content_scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}

impl EvidenceApp {
    fn new(
        path: PathBuf,
        open: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| TextInput::new("Filter by id, title, path, tag…", cx));
        let subscriptions = vec![
            cx.subscribe(&search, |_, _, _: &Changed, cx| cx.notify()),
            cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
        ];
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, EvidenceApp::poll).is_err() {
                    break;
                }
            }
        });
        let browser = Browser::open(&path).map_err(|e| e.to_string());
        let fingerprint = browser.as_ref().map_or(0, Browser::fingerprint);
        let focus = cx.focus_handle();
        if open.is_some() {
            window.focus(&focus);
        } else {
            window.focus(&search.focus_handle(cx));
        }
        let mut this = Self {
            path,
            browser,
            fingerprint,
            filter: Filter::All,
            search,
            selected: None,
            detail: None,
            tab: Tab::Content,
            back: Vec::new(),
            diff: None,
            list_scroll: UniformListScrollHandle::new(),
            content_scroll: UniformListScrollHandle::new(),
            focus,
            _subscriptions: subscriptions,
            _poll: poll,
        };
        if let (Some(target), Ok(b)) = (open, &this.browser) {
            let id = b.resolve(&target);
            this.select(id, false, cx);
        }
        this
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        match &self.browser {
            Ok(b) if b.fingerprint() != self.fingerprint => self.reload(cx),
            Ok(_) => {}
            // Not a project (yet): pick it up once `evidence init` ran.
            Err(_) => {
                if let Ok(b) = Browser::open(&self.path) {
                    self.fingerprint = b.fingerprint();
                    self.browser = Ok(b);
                    cx.notify();
                }
            }
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        if let Ok(b) = &mut self.browser {
            match b.reload() {
                Ok(()) => {
                    self.fingerprint = b.fingerprint();
                    self.detail = self.selected.as_deref().and_then(|id| b.detail(id));
                    if self.detail.is_none() {
                        self.selected = None;
                    }
                    self.diff = None;
                }
                Err(e) => self.browser = Err(e.to_string()),
            }
        } else {
            self.browser = Browser::open(&self.path).map_err(|e| e.to_string());
        }
        cx.notify();
    }

    fn select(&mut self, id: Option<String>, remember: bool, cx: &mut Context<Self>) {
        if remember
            && let Some(current) = self.selected.take()
            && Some(&current) != id.as_ref()
        {
            self.back.push(current);
        }
        self.detail = match (&self.browser, &id) {
            (Ok(b), Some(id)) => b.detail(id),
            _ => None,
        };
        self.selected = id.filter(|_| self.detail.is_some());
        self.diff = None;
        self.content_scroll = UniformListScrollHandle::new();
        cx.notify();
    }

    fn rows(&self, cx: &App) -> Vec<PairRow> {
        let query = self.search.read(cx).text().to_owned();
        match &self.browser {
            Ok(b) => b
                .filtered(self.filter, &query)
                .into_iter()
                .cloned()
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

impl Focusable for EvidenceApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

// ---- small building blocks ----

fn badge(label: impl Into<SharedString>, color: Hsla) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .whitespace_nowrap()
        .flex_shrink_0()
        .px_1p5()
        .rounded_md()
        .text_xs()
        .bg(color.opacity(0.14))
        .text_color(color)
        .child(label.into())
}

fn health_badge(h: Health) -> gpui::Div {
    badge(h.as_str(), theme::health(h))
}

fn content_label(c: ContentMode) -> &'static str {
    c.as_str()
}

fn section(title: &str) -> gpui::Div {
    div()
        .pt_4()
        .pb_1()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::muted())
        .child(title.to_uppercase())
}

fn mono(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .font_family(theme::MONO)
        .text_sm()
        .whitespace_nowrap()
        .child(text.into())
}

fn short(id: &str) -> String {
    id.parse::<evidence_core::format::ObjectId>()
        .map_or_else(|_| id.to_owned(), |o| o.short())
}

impl Render for EvidenceApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        theme::set_dark(matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ));
        div()
            .id("root")
            .key_context("EvidenceApp")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Reload, _, cx| this.reload(cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                window.focus(&this.search.focus_handle(cx));
            }))
            .on_action(cx.listener(|this, _: &Back, _, cx| {
                let previous = this.back.pop();
                this.select(previous, false, cx);
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg())
            .text_color(theme::text())
            .text_sm()
            .child(self.render_header(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(self.render_sidebar(cx))
                    .child(self.render_main(cx)),
            )
    }
}

impl EvidenceApp {
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut bar = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme::border())
            .child(div().font_weight(FontWeight::BOLD).child("Evidence"));
        match &self.browser {
            Ok(b) => {
                bar = bar.child(
                    div()
                        .text_color(theme::muted())
                        .truncate()
                        .child(b.root().display().to_string()),
                );
                bar = bar.child(div().flex_1());
                for (h, n) in b.counts() {
                    bar = bar.child(health_badge(h).child(format!(" {n}")));
                }
            }
            Err(_) => bar = bar.child(div().flex_1()),
        }
        bar.child(
            div()
                .id("reload")
                .px_2()
                .rounded_md()
                .border_1()
                .border_color(theme::border())
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child("Reload")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.reload(cx))),
        )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows(cx);
        let mut chips = div().flex().flex_wrap().gap_1().px_2().pb_2();
        for (i, f) in Filter::ALL.into_iter().enumerate() {
            let active = f == self.filter;
            chips = chips.child(
                div()
                    .id(("filter", i))
                    .px_2()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .when(active, |d| {
                        d.bg(theme::selected()).text_color(theme::accent())
                    })
                    .when(!active, |d| d.hover(|s| s.bg(theme::hover())))
                    .child(f.label())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.filter = f;
                        cx.notify();
                    })),
            );
        }
        let count = rows.len();
        div()
            .w(px(320.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(theme::sidebar())
            .border_r_1()
            .border_color(theme::border())
            .child(div().p_2().child(self.search.clone()))
            .child(chips)
            .child(
                div()
                    .px_3()
                    .pb_1()
                    .text_xs()
                    .text_color(theme::muted())
                    .child(format!("{count} pair(s)")),
            )
            .child(
                uniform_list(
                    "pairs",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                        let rows = this.rows(cx);
                        range
                            .filter_map(|i| rows.get(i).map(|r| (i, r.clone())))
                            .map(|(i, row)| this.render_row(i, &row, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(self.list_scroll.clone())
                .flex_1(),
            )
    }

    fn render_row(&self, i: usize, row: &PairRow, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected.as_deref() == Some(row.id.as_str());
        let id = row.id.clone();
        div()
            .id(("row", i))
            .w_full()
            .h(px(ROW_HEIGHT))
            .px_3()
            .flex()
            .flex_col()
            .justify_center()
            .cursor_pointer()
            .when(selected, |d| d.bg(theme::selected()))
            .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.select(Some(id.clone()), true, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(8.))
                            .rounded_full()
                            .flex_shrink_0()
                            .bg(theme::health(row.health)),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(row.id.clone()),
                    )
                    .child(div().flex_1())
                    .child(div().text_xs().text_color(theme::muted()).child(
                        if row.productions.is_empty() {
                            content_label(row.content).to_owned()
                        } else {
                            format!(
                                "{} · {}",
                                content_label(row.content),
                                row.productions.join(", ")
                            )
                        },
                    )),
            )
            .child(
                div()
                    .pl_4()
                    .text_xs()
                    .text_color(theme::muted())
                    .truncate()
                    .child(if row.title == row.id {
                        row.subject.clone()
                    } else {
                        row.title.clone()
                    }),
            )
            .into_any_element()
    }

    fn render_main(&self, cx: &mut Context<Self>) -> AnyElement {
        let main = div().flex_1().min_w(px(0.)).flex().flex_col();
        match (&self.browser, &self.detail) {
            (Err(e), _) => main
                .p_6()
                .gap_2()
                .child(div().text_lg().font_weight(FontWeight::BOLD).child("No evidence project"))
                .child(div().text_color(theme::red()).child(e.clone()))
                .child(
                    div()
                        .text_color(theme::muted())
                        .child(format!("Run `evidence init` in {} (it is picked up automatically), or start `evidence-ui <project-dir>`.", self.path.display())),
                )
                .into_any_element(),
            (Ok(b), None) => main.child(Self::render_dashboard(b, cx)).into_any_element(),
            (Ok(_), Some(detail)) => main.child(self.render_page(detail, cx)).into_any_element(),
        }
    }

    fn render_dashboard(b: &Browser, cx: &mut Context<Self>) -> impl IntoElement {
        let attention = b.filtered(Filter::Attention, "");
        let mut page = div()
            .id("dashboard")
            .flex_1()
            .overflow_y_scroll()
            .p_6()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .child("Overview"),
            )
            .child(div().text_color(theme::muted()).child(format!(
                "{} pairs in {}",
                b.rows().len(),
                b.root().display()
            )));
        let mut cards = div().flex().flex_wrap().gap_3().pt_4();
        for (h, n) in b.counts() {
            cards = cards.child(
                div()
                    .w(px(120.))
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme::health(h))
                            .child(n.to_string()),
                    )
                    .child(div().text_color(theme::muted()).child(h.as_str())),
            );
        }
        page = page.child(cards);
        let problems = b.problems();
        if !problems.is_empty() {
            page = page.child(section("Problems in evidence files"));
            for p in problems {
                page = page.child(mono(p).text_color(theme::red()));
            }
        }
        page = page.child(section("Needs attention"));
        if attention.is_empty() {
            page = page.child(
                div()
                    .text_color(theme::green())
                    .child("Everything is up to date."),
            );
        }
        for (i, row) in attention.into_iter().enumerate() {
            let id = row.id.clone();
            page = page.child(
                div()
                    .id(("attention", i))
                    .flex()
                    .items_center()
                    .gap_2()
                    .py_1()
                    .px_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.select(Some(id.clone()), true, cx);
                    }))
                    .child(health_badge(row.health))
                    .child(div().font_weight(FontWeight::MEDIUM).child(row.id.clone()))
                    .child(
                        div()
                            .text_color(theme::muted())
                            .truncate()
                            .child(row.reason.clone().unwrap_or_default()),
                    ),
            );
        }
        page
    }

    fn render_page(&self, d: &Detail, cx: &mut Context<Self>) -> impl IntoElement {
        let v = &d.view;
        let mut title = div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .pt_3()
            .when(!self.back.is_empty(), |t| {
                t.child(
                    div()
                        .id("back")
                        .px_1()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .child("←")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            let previous = this.back.pop();
                            this.select(previous, false, cx);
                        })),
                )
            })
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .child(v.id.clone()),
            )
            .child(badge(
                content_label(v.definition.content_mode()),
                theme::muted(),
            ))
            .child(health_badge(v.status.health));
        if v.working_differs {
            title = title.child(badge("working file differs", theme::amber()));
        }
        if v.status.legacy {
            title = title.child(badge("format v1", theme::amber()));
        }
        let mut header = div().flex().flex_col().child(title).child(
            div()
                .px_4()
                .text_color(theme::muted())
                .child(match &v.definition.title {
                    Some(t) => format!("{t} — {}", v.subject),
                    None => v.subject.clone(),
                }),
        );
        if !v.status.reasons.is_empty() {
            let mut reasons = div()
                .mx_4()
                .mt_2()
                .p_2()
                .rounded_md()
                .bg(theme::health(v.status.health).opacity(0.08));
            for r in &v.status.reasons {
                reasons = reasons.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(badge(r.code, theme::health(r.health)))
                        .child(r.message.clone()),
                );
            }
            header = header.child(reasons);
        }
        let mut tabs = div()
            .flex()
            .gap_1()
            .px_4()
            .pt_3()
            .border_b_1()
            .border_color(theme::border());
        for (i, tab) in Tab::ALL.into_iter().enumerate() {
            let active = tab == self.tab;
            let label = match tab {
                Tab::History => format!("History ({})", d.history.len()),
                Tab::Graph => format!("Graph ({}↑ {}↓)", d.upstream.len(), d.downstream.len()),
                _ => tab.label().to_owned(),
            };
            tabs = tabs.child(
                div()
                    .id(("tab", i))
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .border_b_2()
                    .border_color(if active {
                        theme::accent()
                    } else {
                        gpui::transparent_black()
                    })
                    .when(active, |t| t.text_color(theme::accent()))
                    .child(label)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.tab = tab;
                        cx.notify();
                    })),
            );
        }
        let body: AnyElement = match self.tab {
            Tab::Content => self.render_content(d, cx),
            Tab::Evidence => Self::render_evidence(d).into_any_element(),
            Tab::Graph => self.render_graph(d, cx).into_any_element(),
            Tab::History => self.render_history(d, cx).into_any_element(),
        };
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(header)
            .child(tabs)
            .child(div().flex_1().min_h(px(0.)).flex().flex_col().child(body))
    }

    fn render_content(&self, d: &Detail, cx: &mut Context<Self>) -> AnyElement {
        match &d.content {
            Content::Missing => div()
                .p_4()
                .text_color(theme::muted())
                .child("The subject does not exist yet. Run `evidence update` to produce it.")
                .into_any_element(),
            Content::Opaque { size } => div()
                .p_4()
                .text_color(theme::muted())
                .child(format!(
                    "Binary or large subject ({size} bytes); not shown."
                ))
                .into_any_element(),
            Content::Directory { entries, truncated } => {
                let entries = entries.clone();
                let count = entries.len();
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px_4()
                            .py_1()
                            .text_xs()
                            .text_color(theme::muted())
                            .child(format!(
                                "{count} file(s){}",
                                if *truncated { " (truncated)" } else { "" }
                            )),
                    )
                    .child(
                        uniform_list(
                            "entries",
                            count,
                            move |range: std::ops::Range<usize>, _, _| {
                                range
                                    .filter_map(|i| entries.get(i))
                                    .map(|e| mono(e.clone()).w_full().h(px(LINE_HEIGHT)).px_4())
                                    .collect::<Vec<_>>()
                            },
                        )
                        .flex_1(),
                    )
                    .into_any_element()
            }
            Content::Text { lines, truncated } => {
                let lines = lines.clone();
                let count = lines.len();
                let mut col = div().flex_1().flex().flex_col();
                if let Some(cov) = &d.coverage {
                    let c = &cov.counts;
                    col = col.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .px_4()
                            .py_2()
                            .items_center()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(format!(
                                "{:.0}% backed ({:.0}% by evidence)",
                                cov.ratio * 100.0,
                                cov.evidence_ratio * 100.0
                            )))
                            .child(badge(format!("matched {}", c.matched), theme::green()))
                            .child(badge(format!("primary {}", c.primary), theme::blue()))
                            .child(badge(format!("stale {}", c.stale), theme::amber()))
                            .child(badge(format!("broken {}", c.broken), theme::red()))
                            .child(badge(format!("uncovered {}", c.uncovered), theme::purple())),
                    );
                }
                if *truncated {
                    col = col.child(
                        div()
                            .px_4()
                            .text_xs()
                            .text_color(theme::amber())
                            .child(format!("Showing the first {count} lines.")),
                    );
                }
                let _ = cx;
                col.child(
                    uniform_list(
                        "lines",
                        count,
                        move |range: std::ops::Range<usize>, _, _| {
                            range
                                .filter_map(|i| {
                                    lines
                                        .get(i)
                                        .map(|l| (l, i.checked_sub(1).and_then(|p| lines.get(p))))
                                })
                                .map(|(line, previous)| {
                                    // Label a claim only where it starts.
                                    let starts = previous.is_none_or(|p| p.claims != line.claims);
                                    let bar =
                                        line.status.map_or(gpui::transparent_black(), theme::line);
                                    div()
                                        .w_full()
                                        .h(px(LINE_HEIGHT))
                                        .flex()
                                        .items_center()
                                        .when(
                                            line.status.is_some_and(|s| {
                                                s != evidence_core::LineStatus::Uncovered
                                            }),
                                            |d| d.bg(bar.opacity(0.06)),
                                        )
                                        .child(div().w(px(3.)).h_full().flex_shrink_0().bg(bar))
                                        .child(
                                            div()
                                                .w(px(52.))
                                                .flex_shrink_0()
                                                .pr_2()
                                                .text_right()
                                                .text_xs()
                                                .font_family(theme::MONO)
                                                .text_color(theme::muted())
                                                .child(line.number.to_string()),
                                        )
                                        .child(mono(line.text.clone()).flex_1().overflow_hidden())
                                        .when(starts && !line.claims.is_empty(), |d| {
                                            d.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .px_2()
                                                    .text_xs()
                                                    .text_color(theme::muted())
                                                    .child(line.claims.join(", ")),
                                            )
                                        })
                                })
                                .collect::<Vec<_>>()
                        },
                    )
                    .track_scroll(self.content_scroll.clone())
                    .flex_1(),
                )
                .into_any_element()
            }
        }
    }

    fn render_evidence(d: &Detail) -> impl IntoElement {
        let v = &d.view;
        let mut page = div()
            .id("evidence")
            .flex_1()
            .overflow_y_scroll()
            .px_4()
            .pb_6();
        if !v.status.claims.is_empty() {
            page = page.child(section("Claims"));
            for c in &v.status.claims {
                let lines = c
                    .lines
                    .map_or_else(|| "?".to_owned(), |(a, b)| format!("lines {a}-{b}"));
                let mut card = div()
                    .mb_2()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(badge(c.health.as_str(), theme::claim(c.health)))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(c.id.clone()))
                            .child(div().text_color(theme::muted()).child(lines)),
                    );
                if !c.evidence.is_empty() {
                    card = card.child(
                        div()
                            .text_color(theme::muted())
                            .child(format!("based on {}", c.evidence.join(", "))),
                    );
                }
                if let Some(note) = &c.note {
                    card = card.child(div().italic().child(note.clone()));
                }
                for r in &c.reasons {
                    card = card.child(
                        div()
                            .text_color(theme::claim(c.health))
                            .child(r.message.clone()),
                    );
                }
                for n in &c.notices {
                    card = card.child(div().text_color(theme::blue()).child(n.message.clone()));
                }
                if let Some(s) = &c.suggestion {
                    card = card.child(div().text_color(theme::blue()).child(format!("hint: {s}")));
                }
                page = page.child(card);
            }
        }
        if let Some(state) = &v.definition.state {
            page = page.child(section("State"));
            let mut kv = Vec::new();
            if let Some(c) = &state.content {
                kv.push(("adopted".to_owned(), c.snapshot.to_string()));
                kv.push(("updated".to_owned(), c.updated_at.to_string()));
            }
            for (name, r) in &state.runs {
                kv.push((format!("run {name}"), r.at.to_string()));
                for (k, s) in &r.inputs {
                    kv.push((format!("  input {k}"), s.snapshot.to_string()));
                }
                for (k, val) in &r.locator {
                    kv.push((format!("  locator.{k}"), val.clone()));
                }
            }
            for (name, f) in &state.failures {
                kv.push((format!("failed {name}"), format!("{}: {}", f.at, f.message)));
            }
            for f in &d.fixed {
                let diff = f
                    .notice
                    .as_ref()
                    .map_or_else(|| "same as current".to_owned(), |n| n.message.clone());
                kv.push((
                    format!("fixed {}", f.key),
                    format!("{} — {diff}", f.revision),
                ));
            }
            for (k, val) in kv {
                page = page.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .w(px(160.))
                                .flex_shrink_0()
                                .text_color(theme::muted())
                                .child(k),
                        )
                        .child(mono(val)),
                );
            }
        }
        page = page.child(section(&format!("Evidence file — {}", v.evid)));
        let mut file = div().p_2().rounded_md().bg(theme::sidebar());
        for line in d.evid_text.lines() {
            let color = if line.trim_start().starts_with('#') {
                theme::muted()
            } else {
                theme::text()
            };
            file = file.child(
                mono(line.to_owned())
                    .text_color(color)
                    .min_h(px(LINE_HEIGHT)),
            );
        }
        page.child(file)
    }

    fn render_graph(&self, d: &Detail, cx: &mut Context<Self>) -> impl IntoElement {
        let link_list = |title: &str,
                         items: &[(String, Health)],
                         prefix: &'static str,
                         cx: &mut Context<Self>| {
            let mut col = div().child(section(title));
            if items.is_empty() {
                col = col.child(div().text_color(theme::muted()).child("none"));
            }
            for (i, (id, h)) in items.iter().enumerate() {
                let target = id.clone();
                col = col.child(
                    div()
                        .id((prefix, i))
                        .flex()
                        .items_center()
                        .gap_2()
                        .py_0p5()
                        .px_2()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.select(Some(target.clone()), true, cx);
                        }))
                        .child(health_badge(*h))
                        .child(id.clone()),
                );
            }
            col
        };
        let impact: Vec<(String, Health)> = d
            .impact
            .iter()
            .map(|id| {
                let h = self
                    .browser
                    .as_ref()
                    .ok()
                    .and_then(|b| b.health(id))
                    .unwrap_or(Health::Broken);
                (id.clone(), h)
            })
            .collect();
        div()
            .id("graph")
            .flex_1()
            .overflow_y_scroll()
            .px_4()
            .pb_6()
            .child(link_list("Depends on (upstream)", &d.upstream, "up", cx))
            .child(link_list("Used by (downstream)", &d.downstream, "down", cx))
            .child(link_list(
                "Impact: everything affected by a change",
                &impact,
                "impact",
                cx,
            ))
    }

    fn render_history(&self, d: &Detail, cx: &mut Context<Self>) -> impl IntoElement {
        let mut page = div()
            .id("history")
            .flex_1()
            .overflow_y_scroll()
            .px_4()
            .pb_6();
        if d.history.is_empty() {
            return page.child(
                div()
                    .pt_4()
                    .text_color(theme::muted())
                    .child("No runs or reviews recorded yet."),
            );
        }
        page = page.child(section(
            "Runs and reviews (newest first) — click one with a snapshot to see what changed",
        ));
        let previous_snapshot = |i: usize, e: &Event| -> Option<String> {
            e.previous
                .clone()
                .or_else(|| d.history[..i].iter().rev().find_map(|p| p.snapshot.clone()))
        };
        for (i, e) in d.history.iter().enumerate().rev() {
            let comparable = e.snapshot.is_some()
                && previous_snapshot(i, e).is_some_and(|p| Some(&p) != e.snapshot.as_ref());
            let pair = comparable.then(|| {
                (
                    previous_snapshot(i, e).unwrap_or_default(),
                    e.snapshot.clone().unwrap_or_default(),
                )
            });
            let color = match e.status.as_str() {
                "failed" => theme::red(),
                "changed" | "reviewed" => theme::green(),
                _ => theme::muted(),
            };
            let open = self.diff.as_ref().is_some_and(|(at, _)| *at == i);
            let mut row = div()
                .id(("event", i))
                .flex()
                .items_center()
                .gap_2()
                .py_1()
                .px_2()
                .rounded_md()
                .when(open, |r| r.bg(theme::selected()))
                .child(
                    div()
                        .w(px(220.))
                        .flex_shrink_0()
                        .font_family(theme::MONO)
                        .text_xs()
                        .child(e.at.clone()),
                )
                .child(div().w(px(80.)).flex_shrink_0().child(e.operation.clone()))
                .child(badge(e.status.clone(), color))
                .child(
                    mono(e.snapshot.as_deref().map(short).unwrap_or_default())
                        .text_color(theme::muted()),
                )
                .child(
                    div().text_color(theme::muted()).truncate().child(
                        e.message
                            .clone()
                            .or_else(|| e.by.clone())
                            .unwrap_or_default(),
                    ),
                );
            if let Some((old, new)) = pair {
                row = row
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if this.diff.as_ref().is_some_and(|(at, _)| *at == i) {
                            this.diff = None;
                        } else if let Ok(b) = &this.browser {
                            this.diff = Some((i, b.diff(&old, &new).map_err(|e| e.to_string())));
                        }
                        cx.notify();
                    }));
            }
            page = page.child(row);
            if open && let Some((_, diff)) = &self.diff {
                page = page.child(render_diff(diff));
            }
        }
        page
    }
}

fn render_diff(diff: &Result<Vec<DiffLine>, String>) -> gpui::Div {
    let mut block = div().ml_4().mb_2().p_2().rounded_md().bg(theme::sidebar());
    match diff {
        Err(e) => block = block.child(div().text_color(theme::red()).child(e.clone())),
        Ok(lines) => {
            // Show changes with a little context; collapse long unchanged runs.
            const CONTEXT: usize = 2;
            let changed: Vec<bool> = lines
                .iter()
                .map(|l| !matches!(l, DiffLine::Same(_)))
                .collect();
            let near = |i: usize| {
                let lo = i.saturating_sub(CONTEXT);
                let hi = (i + CONTEXT).min(lines.len().saturating_sub(1));
                changed[lo..=hi].iter().any(|c| *c)
            };
            let mut skipped = 0usize;
            for (i, l) in lines.iter().enumerate() {
                if !near(i) {
                    skipped += 1;
                    continue;
                }
                if skipped > 0 {
                    block = block.child(
                        mono(format!("  … {skipped} unchanged line(s)")).text_color(theme::muted()),
                    );
                    skipped = 0;
                }
                let (sign, text, color) = match l {
                    DiffLine::Same(t) => (" ", t, theme::muted()),
                    DiffLine::Removed(t) => ("-", t, theme::red()),
                    DiffLine::Added(t) => ("+", t, theme::green()),
                };
                block = block.child(mono(format!("{sign} {text}")).text_color(color));
            }
            if skipped > 0 {
                block = block.child(
                    mono(format!("  … {skipped} unchanged line(s)")).text_color(theme::muted()),
                );
            }
            if !changed.iter().any(|c| *c) {
                block = block.child(div().text_color(theme::muted()).child("No textual change."));
            }
        }
    }
    block
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let arg = args.next();
    let open = args.next().map(|a| a.to_string_lossy().into_owned());
    if let Some(a) = &arg
        && (a == "--help" || a == "-h")
    {
        let mut out = std::io::stdout().lock();
        let _ = std::io::Write::write_all(
            &mut out,
            b"evidence-ui [PROJECT_DIR] [ID_OR_PATH]\n\nBrowse an Evidence project: freshness, coverage, evidence, graph, history.\n",
        );
        return;
    }
    let path = arg
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    Application::new().run(move |cx: &mut App| {
        cx.bind_keys(input::bindings());
        cx.bind_keys([
            KeyBinding::new("secondary-r", Reload, None),
            KeyBinding::new("secondary-f", FocusSearch, None),
            KeyBinding::new("secondary-[", Back, None),
            KeyBinding::new("secondary-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.set_menus(vec![Menu {
            name: "Evidence".into(),
            items: vec![
                MenuItem::action("Reload", Reload),
                MenuItem::action("Find", FocusSearch),
                MenuItem::action("Back", Back),
                MenuItem::separator(),
                MenuItem::action("Quit Evidence", Quit),
            ],
        }]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Evidence".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| EvidenceApp::new(path, open, window, cx)),
        );
        if let Err(e) = opened {
            let _ = std::io::Write::write_all(
                &mut std::io::stderr(),
                format!("cannot open window: {e}\n").as_bytes(),
            );
            cx.quit();
        }
        cx.activate(true);
    });
}
