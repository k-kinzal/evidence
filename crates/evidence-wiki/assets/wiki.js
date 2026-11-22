/* evidence-wiki client: search, sortable tables, data grids, diagrams; on the server also SQL,
   live reload, and agents. No dependencies besides the bundled diagram libraries; works from
   file:// for the static site. Runs under a Content Security Policy without inline script. */
(function () {
  "use strict";
  const body = document.body;
  const W = {
    root: body.dataset.root || "",
    dynamic: body.dataset.dynamic === "true",
    version: body.dataset.version || "",
  };
  const $ = (sel, el) => (el || document).querySelector(sel);
  const $$ = (sel, el) => Array.from((el || document).querySelectorAll(sel));
  const h = (tag, attrs, ...kids) => {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (k === "class") el.className = v;
      else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
      else if (v !== undefined && v !== null && v !== false) el.setAttribute(k, v === true ? "" : v);
    }
    for (const kid of kids.flat()) if (kid !== null && kid !== undefined && kid !== false) el.append(kid instanceof Node ? kid : String(kid));
    return el;
  };
  const button = (label, onclick, extra) => h("button", Object.assign({ type: "button", class: "button", onclick }, extra || {}), label);
  const pageHref = (id) => W.root + "p/" + encodeURIComponent(id) + ".html";
  const norm = (s) => String(s || "").toLowerCase().replace(/_/g, " ").replace(/\s+/g, " ").trim();
  const api = (path, opts) => fetch(W.root + "api/" + path, opts).then(async (r) => {
    const data = await r.json().catch(() => ({ error: r.statusText }));
    if (!r.ok) throw new Error(data.error || r.statusText);
    return data;
  });
  const postJson = (path, data) => api(path, {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(data || {}),
  });
  const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

  /* ---- Search ---- */
  function terms(q) {
    const out = []; let cur = ""; let quoted = false;
    for (const c of q) {
      if (c === '"') { if (cur.trim()) out.push(cur.trim().toLowerCase()); cur = ""; quoted = !quoted; }
      else if (/\s/.test(c) && !quoted) { if (cur) out.push(cur.toLowerCase()); cur = ""; }
      else cur += c;
    }
    if (cur.trim()) out.push(cur.trim().toLowerCase());
    return out;
  }
  function localSearch(q, limit) {
    const index = window.WIKI_INDEX || [];
    const ts = terms(q); const whole = norm(q.replace(/"/g, ""));
    if (!ts.length) return [];
    const hits = [];
    for (const p of index) {
      const title = p.t.toLowerCase(), aliases = p.a.map((a) => a.toLowerCase());
      const tags = p.g.map((a) => a.toLowerCase()), cats = p.c.map((a) => a.toLowerCase());
      const heads = p.h.map((a) => a.toLowerCase()), text = (p.b || "").toLowerCase();
      const path = p.p.toLowerCase(), desc = (p.s || "").toLowerCase();
      let score = 0; let ok = true;
      if (norm(p.t) === whole || p.i === whole) score += 100;
      else if (p.a.some((a) => norm(a) === whole)) score += 80;
      for (const t of ts) {
        let s = 0;
        if (title.includes(t)) s += 20;
        if (aliases.some((a) => a.includes(t))) s += 15;
        if (tags.includes(t)) s += 10;
        if (cats.some((a) => a.includes(t))) s += 8;
        if (desc.includes(t)) s += 6;
        if (heads.some((a) => a.includes(t))) s += 5;
        if (path.includes(t) || p.i.includes(t)) s += 4;
        const n = text.split(t).length - 1;
        if (n > 0) s += 1 + Math.log(n);
        if (!s) { ok = false; break; }
        score += s;
      }
      if (!ok) continue;
      let snippet = p.s || "";
      for (const t of ts) {
        const at = text.indexOf(t);
        if (at >= 0) {
          const start = Math.max(0, at - 70);
          snippet = (start ? "…" : "") + p.b.slice(start, at + t.length + 110) + "…";
          break;
        }
      }
      hits.push({ id: p.i, title: p.t, path: p.p, kind: p.k, view: p.v, categories: p.c, score, snippet });
    }
    hits.sort((a, b) => b.score - a.score || a.title.localeCompare(b.title));
    return hits.slice(0, limit);
  }
  /* `text` with each term in bold (`span.searchmatch`). */
  function highlight(text, ts) {
    const frag = document.createDocumentFragment();
    if (!ts.length) { frag.append(text); return frag; }
    const re = new RegExp("(" + ts.map(escapeRe).join("|") + ")", "gi");
    let last = 0;
    text.replace(re, (m, _g, at) => { frag.append(text.slice(last, at), h("span", { class: "searchmatch" }, m)); last = at + m.length; return m; });
    frag.append(text.slice(last));
    return frag;
  }
  function exactPage(q) {
    const n = norm(q);
    for (const p of window.WIKI_INDEX || []) {
      if (norm(p.t) === n || p.i === q.trim() || p.a.some((a) => norm(a) === n)) return p.i;
    }
    return null;
  }
  async function searchPage() {
    const box = $("#search-results");
    if (!box) return;
    const q = new URLSearchParams(location.search).get("q") || "";
    const input = $("form.search-form input");
    if (input) { input.value = q; if (!q) input.focus(); }
    if (!q.trim()) return;
    document.title = q + " — " + document.title;
    const ts = terms(q);
    let hits;
    if (W.dynamic) {
      try { hits = (await api("search?limit=100&q=" + encodeURIComponent(q))).hits; } catch (e) { hits = localSearch(q, 100); }
    } else hits = localSearch(q, 100);
    box.replaceChildren();
    const exact = exactPage(q);
    if (exact) {
      box.append(h("p", {}, "There is a page named “", h("a", { href: pageHref(exact) }, q), "”."));
    } else {
      box.append(h("p", {}, "There is no page titled “", h("span", { class: "new" }, q), "”."));
    }
    box.append(h("p", { class: "search-summary" },
      hits.length ? "Results 1–" + hits.length + (hits.length === 100 ? " (more not shown)" : "") : "No page contains these words."));
    box.append(h("ul", { class: "search-results" }, hits.map((hit) => h("li", {},
      h("div", { class: "search-result-title" }, h("a", { href: pageHref(hit.id) }, highlight(hit.title, ts))),
      hit.snippet ? h("div", { class: "search-result-snippet" }, highlight(hit.snippet, ts)) : null,
      (hit.categories || []).length ? h("div", { class: "search-result-meta" }, hit.categories.join(", ")) : null))));
  }
  function suggest() {
    const form = $("form.search"); if (!form) return;
    const input = $("input", form), list = $("ul.suggest", form);
    let items = [], active = -1;
    const close = () => { list.hidden = true; active = -1; };
    const show = () => {
      const q = input.value.trim();
      if (!q) { close(); return; }
      const n = norm(q);
      const scored = [];
      for (const p of window.WIKI_INDEX || []) {
        let best = 0;
        for (const name of [p.t, ...p.a]) {
          const m = norm(name);
          if (m === n) best = Math.max(best, 3);
          else if (m.startsWith(n)) best = Math.max(best, 2);
          else if (m.includes(n)) best = Math.max(best, 1);
        }
        if (best) scored.push([best, p]);
      }
      scored.sort((a, b) => b[0] - a[0] || a[1].t.length - b[1].t.length);
      items = scored.slice(0, 8).map((s) => s[1]);
      const ts = [q.toLowerCase()];
      list.replaceChildren(...items.map((p, k) => h("li", { class: k === active ? "active" : "", role: "option" },
        h("a", { href: pageHref(p.i) },
          h("span", { class: "suggest-title" }, highlight(p.t, ts)),
          h("span", { class: "suggest-description" }, p.s || "")))));
      list.append(h("li", { class: "suggest-all" + (active === items.length ? " active" : "") },
        h("a", { href: W.root + "search.html?q=" + encodeURIComponent(q) }, "Search for pages containing “" + q + "”")));
      list.hidden = false;
    };
    input.addEventListener("input", () => { active = -1; show(); });
    input.addEventListener("keydown", (e) => {
      if (e.key === "ArrowDown") { active = Math.min(active + 1, items.length); show(); e.preventDefault(); }
      else if (e.key === "ArrowUp") { active = Math.max(active - 1, -1); show(); e.preventDefault(); }
      else if (e.key === "Escape") close();
    });
    form.addEventListener("submit", (e) => {
      const q = input.value.trim();
      if (!q) { e.preventDefault(); return; }
      if (active === items.length) return;
      const target = active >= 0 ? items[active].i : exactPage(q);
      if (target) { e.preventDefault(); location.href = pageHref(target); }
    });
    document.addEventListener("click", (e) => { if (!form.contains(e.target)) close(); });
    document.addEventListener("keydown", (e) => {
      if (e.key === "/" && !/INPUT|TEXTAREA|SELECT/.test((document.activeElement || {}).tagName || "")) { input.focus(); e.preventDefault(); }
    });
  }

  /* ---- Values ---- */
  const isNum = (v) => typeof v === "number";
  function cellText(v) {
    if (v === null || v === undefined) return "";
    if (typeof v === "object") return JSON.stringify(v);
    return String(v);
  }
  function compare(a, b) {
    if (a === b) return 0;
    if (a === null || a === undefined || a === "") return 1;
    if (b === null || b === undefined || b === "") return -1;
    if (isNum(a) && isNum(b)) return a - b;
    return cellText(a).localeCompare(cellText(b), undefined, { numeric: true });
  }

  /* ---- Sortable tables (`table.wikitable.sortable`) and filters (`[data-filter]`) ---- */
  function sortable(table) {
    const head = $("thead tr", table), tbody = $("tbody", table);
    if (!head || !tbody) return;
    let col = -1, dir = 1;
    $$("th", head).forEach((th, i) => {
      const mark = h("span", { class: "sort-indicator", "aria-hidden": "true" }, "↕");
      th.append(mark);
      th.setAttribute("aria-sort", "none");
      th.addEventListener("click", () => {
        dir = col === i ? -dir : 1; col = i;
        const rows = $$("tr", tbody);
        const key = (r) => { const t = r.children[i] ? r.children[i].textContent.trim() : ""; const n = Number(t); return t !== "" && !isNaN(n) ? n : t; };
        rows.sort((a, b) => dir * compare(key(a), key(b)));
        tbody.append(...rows);
        $$("th", head).forEach((x, k) => {
          x.setAttribute("aria-sort", k === i ? (dir > 0 ? "ascending" : "descending") : "none");
          $(".sort-indicator", x).textContent = k === i ? (dir > 0 ? "↑" : "↓") : "↕";
        });
      });
    });
  }
  function filters() {
    $$("input[data-filter]").forEach((input) => {
      input.addEventListener("input", () => {
        const q = input.value.trim().toLowerCase();
        $$(input.dataset.filter).forEach((row) => { row.hidden = Boolean(q) && !row.textContent.toLowerCase().includes(q); });
      });
    });
  }

  /* ---- Data grid ---- */
  function matches(v, f) {
    const m = /^(>=|<=|!=|=|>|<|~)\s*(.*)$/.exec(f.trim());
    const text = cellText(v);
    if (!m) return text.toLowerCase().includes(f.trim().toLowerCase());
    const [, op, arg] = m;
    if (op === "~") { try { return new RegExp(arg, "i").test(text); } catch (e) { return false; } }
    const a = isNum(v) ? v : parseFloat(text), b = parseFloat(arg);
    const numeric = !isNaN(a) && !isNaN(b) && arg.trim() !== "";
    const cmp = numeric ? a - b : text.localeCompare(arg);
    switch (op) {
      case "=": return numeric ? cmp === 0 : text.toLowerCase() === arg.toLowerCase();
      case "!=": return numeric ? cmp !== 0 : text.toLowerCase() !== arg.toLowerCase();
      case ">": return cmp > 0; case "<": return cmp < 0;
      case ">=": return cmp >= 0; case "<=": return cmp <= 0;
    }
    return true;
  }
  function toCsv(columns, rows) {
    const q = (s) => /[",\n\r]/.test(s) ? '"' + s.replace(/"/g, '""') + '"' : s;
    return [columns.map(q).join(","), ...rows.map((r) => r.map((v) => q(cellText(v))).join(","))].join("\n");
  }
  /* Columns whose values are all numbers are right-aligned, so their digits line up. */
  function columnKinds(t) {
    return t.columns.map((_, i) => {
      let num = 0;
      for (const r of t.rows) {
        const v = r[i];
        if (v === null || v === undefined || v === "") continue;
        if (!isNum(v)) return "text";
        num++;
      }
      return num ? "number" : "text";
    });
  }
  function grid(el) {
    const src = document.getElementById(el.dataset.table);
    if (!src) return;
    const original = JSON.parse(src.textContent);
    let t = original, kinds = columnKinds(t), sortCol = -1, sortDir = 1, page = 0, global = "", columnFilters = [], filtersOpen = false;
    const size = 100;
    const count = h("span", { class: "datagrid-count", role: "status" });
    const search = h("input", { class: "input input-search", type: "search", name: "filter", placeholder: "Filter rows", "aria-label": "Filter rows" });
    const scroll = h("div", { class: "datagrid-scroll" });
    const pager = h("div", { class: "datagrid-pager" });
    const toolbar = h("div", { class: "toolbar datagrid-toolbar" }, search, count, h("span", { class: "toolbar-spacer" }));
    const sql = h("div", { class: "datagrid-sql", hidden: true });
    const filterToggle = button("Column filters", () => {
      filtersOpen = !filtersOpen;
      filterToggle.setAttribute("aria-pressed", String(filtersOpen));
      if (!filtersOpen && columnFilters.some(Boolean)) { columnFilters = []; page = 0; }
      render(true);
      if (filtersOpen) { const first = $(".datagrid-filters input", scroll); if (first) first.focus(); }
    }, { class: "button button-quiet", "aria-pressed": "false", title: "Text, or >10, <=3, =x, !=x, ~regex" });
    toolbar.append(filterToggle);
    if (el.dataset.sql && W.dynamic) {
      const name = el.dataset.page.replace(/[^A-Za-z0-9]/g, "_");
      const area = h("textarea", { class: "input", name: "sql", spellcheck: "false", "aria-label": "SQL query" }, "SELECT * FROM t LIMIT 100");
      const message = h("p", { class: "datagrid-message", role: "status" });
      const run = async () => {
        message.textContent = ""; message.className = "datagrid-message";
        try {
          const r = await postJson("query", { pages: [el.dataset.page], sql: area.value });
          t = r; kinds = columnKinds(t); sortCol = -1; page = 0; columnFilters = []; render(true);
          if (r.truncated) message.textContent = "Showing the first " + r.rows.length + " rows.";
        } catch (e) { message.textContent = e.message; message.className = "datagrid-message status-error"; }
      };
      area.addEventListener("keydown", (e) => { if ((e.metaKey || e.ctrlKey) && e.key === "Enter") run(); });
      sql.append(
        h("p", {}, "Read-only SQLite. This table is ", h("code", {}, "t"), " (also ", h("code", {}, name), "); other table pages by their id with . and - as _. Ctrl+Enter or ⌘+Enter runs the query."),
        area,
        h("div", { class: "toolbar" },
          button("Run", run, { class: "button button-primary" }),
          button("Reset", () => { t = original; kinds = columnKinds(t); sortCol = -1; columnFilters = []; message.textContent = ""; render(true); })),
        message);
      const toggle = button("SQL", () => { sql.hidden = !sql.hidden; toggle.setAttribute("aria-expanded", String(!sql.hidden)); if (!sql.hidden) area.focus(); }, { class: "button button-quiet", "aria-expanded": "false" });
      toolbar.append(toggle);
    }
    toolbar.append(button("Download CSV", () => {
      const blob = new Blob([toCsv(t.columns, view())], { type: "text/csv" });
      const a = h("a", { href: URL.createObjectURL(blob), download: (el.dataset.page || "table") + ".csv" });
      document.body.append(a); a.click(); a.remove();
    }, { class: "button button-quiet" }));
    el.append(toolbar, sql, scroll, pager);
    search.addEventListener("input", () => { global = search.value; page = 0; render(false); });
    function view() {
      let rows = t.rows;
      if (global.trim()) {
        const g = global.trim().toLowerCase();
        rows = rows.filter((r) => r.some((v) => cellText(v).toLowerCase().includes(g)));
      }
      columnFilters.forEach((f, i) => { if (f && f.trim()) rows = rows.filter((r) => matches(r[i], f)); });
      if (sortCol >= 0) rows = rows.slice().sort((a, b) => sortDir * compare(a[sortCol], b[sortCol]));
      return rows;
    }
    function render(rebuildHead) {
      const rows = view();
      const pages = Math.max(1, Math.ceil(rows.length / size));
      page = Math.min(page, pages - 1);
      const n = (x) => x.toLocaleString();
      count.textContent = rows.length === t.rows.length
        ? n(t.rows.length) + " rows" + (t.truncated ? " (of " + n(t.total) + "; the rest is not embedded)" : "")
        : n(rows.length) + " of " + n(t.rows.length) + " rows";
      let table = $("table", scroll);
      if (!table || rebuildHead) {
        const head = h("tr", {}, t.columns.map((c, i) => h("th", {
          scope: "col", title: c, class: kinds[i] === "number" ? "number" : null,
          "aria-sort": sortCol === i ? (sortDir > 0 ? "ascending" : "descending") : "none",
          onclick: () => { if (sortCol === i) sortDir = -sortDir; else { sortCol = i; sortDir = 1; } render(true); },
        }, c, h("span", { class: "sort-indicator", "aria-hidden": "true" }, sortCol === i ? (sortDir > 0 ? "↑" : "↓") : "↕"))));
        const filterRow = filtersOpen ? h("tr", { class: "datagrid-filters" }, t.columns.map((c, i) => {
          const input = h("input", { class: "input", type: "text", name: "filter-" + i, placeholder: kinds[i] === "number" ? ">0" : "contains", "aria-label": "Filter " + c, value: columnFilters[i] || "", title: "Text, or >10, <=3, =x, !=x, ~regex" });
          input.addEventListener("input", () => { columnFilters[i] = input.value; page = 0; render(false); });
          return h("th", {}, input);
        })) : null;
        table = h("table", { class: "wikitable" }, h("thead", {}, head, filterRow), h("tbody"));
        scroll.replaceChildren(table);
      }
      $("tbody", table).replaceChildren(...rows.slice(page * size, page * size + size).map((r) => h("tr", {}, r.map((v, i) => {
        const text = cellText(v);
        const cls = text === "" ? "empty" : kinds[i] === "number" ? "number" : null;
        const content = /^https?:\/\/\S+$/.test(text) ? h("a", { href: text, class: "external", rel: "noopener noreferrer" }, text) : text;
        return h("td", { class: cls, title: text.length > 40 ? text : null }, content);
      }))));
      const from = rows.length ? page * size + 1 : 0, to = Math.min(rows.length, (page + 1) * size);
      pager.replaceChildren(
        button("Previous", () => { page = Math.max(0, page - 1); render(false); }, { class: "button button-quiet", disabled: page === 0 }),
        h("span", {}, from + "–" + to + " of " + rows.length),
        button("Next", () => { page = Math.min(pages - 1, page + 1); render(false); }, { class: "button button-quiet", disabled: page >= pages - 1 }));
      pager.hidden = pages <= 1;
    }
    render(true);
  }

  /* ---- Diagrams and math (the bundled libraries load only on pages that need them) ---- */
  /* The page's colors, read from its stylesheet, so diagrams and charts match either theme. */
  function palette() {
    const css = getComputedStyle(document.documentElement);
    const v = (name) => css.getPropertyValue(name).trim();
    return {
      dark: matchMedia("(prefers-color-scheme: dark)").matches,
      font: getComputedStyle(body).fontFamily,
      bg: v("--bg"), subtle: v("--bg-subtle"), muted: v("--bg-muted"),
      text: v("--text"), secondary: v("--text-secondary"), faint: v("--text-muted"),
      border: v("--border"), strong: v("--border-strong"), accent: v("--accent"),
      grid: v("--chart-grid"), axis: v("--chart-axis"),
      series: [1, 2, 3, 4, 5, 6, 7, 8].map((i) => v("--series-" + i)),
    };
  }
  const failed = (el, e) => el.prepend(h("div", { class: "messagebox messagebox-error" }, h("p", { class: "messagebox-title" }, String((e && e.message) || e))));
  const charts = new Map();
  function mermaids(c) {
    if (!window.mermaid) return;
    const els = $$("pre.mermaid");
    if (!els.length) return;
    els.forEach((el) => {
      if (el.dataset.source === undefined) el.dataset.source = el.textContent;
      else { el.removeAttribute("data-processed"); el.textContent = el.dataset.source; }
    });
    try {
      window.mermaid.initialize({
        startOnLoad: false, securityLevel: "strict", theme: "base", fontFamily: c.font,
        themeVariables: {
          darkMode: c.dark, background: c.bg, fontFamily: c.font, fontSize: "14px",
          primaryColor: c.subtle, primaryBorderColor: c.strong, primaryTextColor: c.text,
          secondaryColor: c.muted, secondaryBorderColor: c.strong, secondaryTextColor: c.text,
          tertiaryColor: c.bg, tertiaryBorderColor: c.border, tertiaryTextColor: c.text,
          mainBkg: c.subtle, nodeBorder: c.strong, nodeTextColor: c.text, textColor: c.text,
          lineColor: c.faint, clusterBkg: c.bg, clusterBorder: c.border, titleColor: c.text,
          edgeLabelBackground: c.bg, noteBkgColor: c.muted, noteTextColor: c.text, noteBorderColor: c.strong,
          actorBkg: c.subtle, actorBorder: c.strong, actorTextColor: c.text, signalColor: c.faint, signalTextColor: c.text,
        },
      });
      window.mermaid.run({ nodes: els }).catch((e) => console.error(e));
    } catch (e) { console.error(e); }
  }
  function graphvizes(c) {
    if (!window.Viz) return;
    const els = $$(".graphviz");
    if (!els.length) return;
    // Defaults only: anything the source sets wins.
    const font = "Helvetica,Arial,sans-serif";
    const options = {
      graphAttributes: { bgcolor: "transparent", fontname: font, fontsize: "12", fontcolor: c.text, pad: "0.15", nodesep: "0.45", ranksep: "0.55" },
      nodeAttributes: { shape: "box", style: "rounded,filled", fillcolor: c.subtle, color: c.strong, fontcolor: c.text, fontname: font, fontsize: "13", margin: "0.2,0.1", penwidth: "1", height: "0.42" },
      edgeAttributes: { color: c.faint, fontcolor: c.secondary, fontname: font, fontsize: "11", arrowsize: "0.7", penwidth: "1.2" },
    };
    window.Viz.instance().then((viz) => els.forEach((el) => {
      if (el.dataset.source === undefined) { const pre = $("pre", el); el.dataset.source = pre ? pre.textContent : ""; }
      try {
        const svg = viz.renderSVGElement(el.dataset.source, options);
        // The graph's name would show as a tooltip over the whole diagram.
        const name = svg.querySelector("svg > g > title");
        if (name) name.remove();
        svg.setAttribute("role", "img");
        el.replaceChildren(svg);
      } catch (e) { el.replaceChildren(h("pre", { class: "diagram-source" }, el.dataset.source)); failed(el, e); }
    })).catch((e) => console.error(e));
  }
  function vegas(c) {
    if (!(window.vegaEmbed && window.vega)) return;
    const label = { labelColor: c.secondary, titleColor: c.secondary, labelFont: c.font, titleFont: c.font, labelFontSize: 12, titleFontSize: 12, titleFontWeight: 500 };
    const config = {
      background: null, padding: { top: 8, right: 4, bottom: 4, left: 4 }, font: c.font,
      view: { stroke: null },
      title: { color: c.text, font: c.font, fontSize: 14, fontWeight: 600, anchor: "start", offset: 12 },
      axis: Object.assign({ gridColor: c.grid, domainColor: c.axis, tickColor: c.axis, labelPadding: 6, titlePadding: 10, tickSize: 4 }, label),
      axisY: { domain: false, ticks: false, titleAngle: 0, titleAlign: "left", titleAnchor: "end", titleBaseline: "bottom", titleY: -8, titleX: 0 },
      axisX: { labelAngle: 0, labelOverlap: "greedy", labelLimit: 160, grid: false },
      legend: Object.assign({ orient: "top", direction: "horizontal", symbolType: "circle", symbolSize: 70, symbolStrokeWidth: 0, columnPadding: 16, padding: 0, offset: 28, titleOrient: "left", titlePadding: 8 }, label),
      range: { category: c.series, ordinal: { scheme: "blues" }, ramp: { scheme: "blues" } },
      mark: { color: c.series[0], tooltip: { content: "encoding" } },
      bar: { cornerRadiusEnd: 4, discreteBandSize: { band: 0.5 } },
      line: { strokeWidth: 2, strokeCap: "round", strokeJoin: "round" },
      point: { size: 64, filled: true },
      area: { opacity: 0.12, line: true },
      text: { color: c.secondary, font: c.font },
      scale: { bandPaddingInner: 0.35, bandPaddingOuter: 0.2 },
    };
    $$(".vega").forEach((el) => {
      let spec;
      try { spec = JSON.parse(el.dataset.spec); } catch (e) { failed(el, e); return; }
      // Fill the figure's width unless the spec sets one.
      if (spec.width === undefined && !spec.facet && !spec.repeat && !spec.concat && !spec.hconcat) { spec.width = "container"; spec.autosize = spec.autosize || { type: "fit-x", contains: "padding" }; }
      if (spec.height === undefined && !spec.facet && !spec.repeat && !spec.concat && !spec.vconcat) spec.height = 240;
      const old = charts.get(el);
      if (old) { old.finalize(); el.replaceChildren(); }
      // The interpreter evaluates expressions without `new Function`, as the policy requires.
      window.vegaEmbed(el, spec, { actions: false, ast: true, expr: window.vega.expressionInterpreter, config, tooltip: { theme: c.dark ? "dark" : "light" } })
        .then((r) => charts.set(el, r))
        .catch((e) => failed(el, e));
    });
  }
  function diagrams() {
    const draw = () => { const c = palette(); mermaids(c); graphvizes(c); vegas(c); };
    draw();
    // Follow the system theme without a reload.
    matchMedia("(prefers-color-scheme: dark)").addEventListener("change", draw);
    if (window.katex) {
      $$(".math").forEach((el) => {
        try { window.katex.render(el.textContent, el, { displayMode: el.classList.contains("math-display"), throwOnError: false }); } catch (e) { /* keep the source */ }
      });
    }
  }

  /* ---- Header menu on narrow screens; the current section in the contents ---- */
  function navigation() {
    const menu = $(".header-menu"), nav = $("#site-nav");
    if (menu && nav) {
      const set = (open) => { body.classList.toggle("menu-open", open); menu.setAttribute("aria-expanded", String(open)); };
      menu.addEventListener("click", (e) => { e.stopPropagation(); set(!body.classList.contains("menu-open")); });
      document.addEventListener("click", (e) => { if (!nav.contains(e.target)) set(false); });
      document.addEventListener("keydown", (e) => { if (e.key === "Escape") set(false); });
    }
    // The contents are beside the text on wide screens; on narrow ones they are a closed
    // disclosure above it that closes again after a jump.
    const toc = $(".toc details");
    if (toc) {
      const narrow = matchMedia("(max-width: 960px)");
      const fit = () => { toc.open = !narrow.matches; };
      fit();
      narrow.addEventListener("change", fit);
      toc.addEventListener("click", (e) => { if (narrow.matches && e.target.closest("a")) toc.open = false; });
      toc.querySelector("summary").addEventListener("click", (e) => { if (!narrow.matches) e.preventDefault(); });
    }
    const links = $$(".toc a[href^='#']");
    const targets = links.map((a) => document.getElementById(decodeURIComponent(a.getAttribute("href").slice(1)))).filter(Boolean);
    if (!targets.length) return;
    const offset = () => ($(".header") || { offsetHeight: 0 }).offsetHeight + 24;
    const mark = () => {
      let current = null;
      for (const t of targets) if (t.getBoundingClientRect().top <= offset() + 1) current = t;
      // At the bottom of the page, the last section is the current one.
      if (innerHeight + scrollY >= document.documentElement.scrollHeight - 2) current = targets[targets.length - 1];
      links.forEach((a) => a.classList.toggle("active", Boolean(current) && a.getAttribute("href") === "#" + current.id));
    };
    let pending = false;
    addEventListener("scroll", () => { if (!pending) { pending = true; requestAnimationFrame(() => { pending = false; mark(); }); } }, { passive: true });
    mark();
  }

  /* ---- Server only: live reload and agents ---- */
  function liveReload() {
    if (!W.dynamic) return;
    setInterval(async () => {
      if (document.hidden) return;
      const busy = document.activeElement && /TEXTAREA|INPUT/.test(document.activeElement.tagName) && document.activeElement.value;
      try {
        const v = await api("version");
        if (String(v.version) !== String(W.version) && !busy) location.reload();
      } catch (e) { /* the server is restarting */ }
    }, 3000);
  }
  function agents() {
    const box = $("#agents");
    if (!box || !W.dynamic) return;
    const log = h("pre", { class: "agents-log", hidden: true });
    const message = h("div");
    const showError = (text) => message.replaceChildren(h("div", { class: "messagebox messagebox-error" }, h("p", { class: "messagebox-title" }, text)));
    const last = (a) => {
      if (!a.last) return h("span", { class: "subtle" }, "never");
      const ok = a.last.ok;
      return h("span", {}, (a.last.finished || "running") + " ",
        h("span", { class: "status " + (ok ? "status-ok" : "status-error") }, ok ? "succeeded" : "failed" + (a.last.exit !== null && a.last.exit !== undefined ? " (exit " + a.last.exit + ")" : "")));
    };
    const load = async () => {
      let list;
      try { list = (await api("agents")).agents; } catch (e) { showError(e.message); return; }
      if (!list.length) {
        box.replaceChildren(h("div", { class: "messagebox messagebox-notice" },
          h("p", { class: "messagebox-title" }, "No agents are configured."),
          h("p", {}, "Add ", h("code", {}, "[[agents]]"), " to ", h("code", {}, ".evidence/wiki.toml"), ".")));
        return;
      }
      const rows = list.map((a) => h("tr", {},
        h("td", {}, h("b", {}, a.name), a.enabled ? null : h("div", { class: "subtle" }, "disabled")),
        h("td", {}, h("code", {}, a.run.join(" "))),
        h("td", {}, a.interval || h("span", { class: "subtle" }, "on demand")),
        h("td", {}, a.running ? h("span", { class: "status status-notice" }, "running since " + a.running) : a.queued ? h("span", { class: "status status-notice" }, "queued") : (a.next || "")),
        h("td", {}, last(a)),
        h("td", {}, h("div", { class: "toolbar" },
          button("Run now", async () => {
            try { await postJson("agents/" + encodeURIComponent(a.name) + "/run"); message.replaceChildren(); } catch (e) { showError(e.message); }
            load();
          }, { disabled: Boolean(a.running || a.queued) }),
          button("Log", async () => {
            log.hidden = false;
            try { log.textContent = (await api("agents/" + encodeURIComponent(a.name) + "/log")).log || "(no log yet)"; } catch (e) { log.textContent = e.message; }
          })))));
      box.replaceChildren(message,
        h("table", { class: "wikitable full" },
          h("thead", {}, h("tr", {}, ["Agent", "Command", "Interval", "Next run", "Last run", ""].map((x) => h("th", { scope: "col" }, x)))),
          h("tbody", {}, rows)),
        log);
    };
    load();
    setInterval(load, 5000);
  }

  /* ---- Evidence: a marker shows what the statement rests on, without leaving the text ---- */
  function evidencePreviews() {
    let box = null, timer = 0;
    const hide = () => { clearTimeout(timer); if (box) { box.hidden = true; box.replaceChildren(); } };
    const show = (a) => {
      clearTimeout(timer);
      const note = document.getElementById(decodeURIComponent(a.getAttribute("href").slice(1)));
      if (!note) return;
      if (!box) {
        box = h("div", { class: "evidence-preview", role: "tooltip", hidden: true });
        box.addEventListener("mouseenter", () => clearTimeout(timer));
        box.addEventListener("mouseleave", () => { timer = setTimeout(hide, 150); });
        document.body.append(box);
      }
      box.replaceChildren(...Array.from(note.childNodes, (n) => n.cloneNode(true)));
      box.hidden = false;
      const r = a.getBoundingClientRect();
      const left = Math.max(16, Math.min(r.left + scrollX - 24, scrollX + innerWidth - box.offsetWidth - 16));
      const below = r.bottom + 8 + box.offsetHeight < innerHeight;
      box.style.left = left + "px";
      box.style.top = (below ? r.bottom + scrollY + 8 : r.top + scrollY - box.offsetHeight - 8) + "px";
    };
    $$("sup.reference a[href^='#cite-note-']").forEach((a) => {
      a.addEventListener("mouseenter", () => show(a));
      a.addEventListener("mouseleave", () => { timer = setTimeout(hide, 150); });
      a.addEventListener("focus", () => show(a));
      a.addEventListener("blur", () => { timer = setTimeout(hide, 150); });
    });
    document.addEventListener("keydown", (e) => { if (e.key === "Escape") hide(); });
  }

  suggest(); searchPage(); filters(); navigation(); evidencePreviews();
  $$("table.wikitable.sortable").forEach(sortable);
  $$(".datagrid[data-table]").forEach(grid);
  liveReload(); agents();
  // The libraries are deferred scripts before this one, so they have run already.
  diagrams();
})();
