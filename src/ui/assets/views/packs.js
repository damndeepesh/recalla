// Context packs: exactly what each agent was given, which memories made it in
// and why, and how it compares with sending every candidate memory.
import { h, icon, num, time, absTime, short, baseName } from "../dom.js";
import { get, enc } from "../api.js";
import { go, href, sessionHref, memoryHref } from "../router.js";
import {
  pagedList,
  packRow,
  emptyState,
  errorState,
  loading,
  flag,
  kindDot,
  statusFlag,
  projectChip,
  section,
  select,
  projects,
  ruler,
  copyButton,
  savedWords,
  AGENT_LABEL,
  VIA_LABEL,
} from "../ui.js";

const VIA = [
  ["", "Any trigger"],
  ["hook", VIA_LABEL.hook],
  ["mcp", VIA_LABEL.mcp],
  ["cli", VIA_LABEL.cli],
];

// ------------------------------------------------------------------ list
function mountList(ctx) {
  ctx.title("Context packs");
  const q = ctx.query;
  const set = (patch) => {
    const next = new URLSearchParams(q);
    for (const [k, v] of Object.entries(patch)) v ? next.set(k, v) : next.delete(k);
    go(["packs"], next);
  };
  const proj = select("project", [["", "All projects"]], "", "Project");
  projects(get)
    .then((items) => {
      const cur = q.get("project") ?? "";
      const opts = [["", "All projects"], ...items.map((p) => [p.project, baseName(p.project)])];
      if (cur && !items.some((p) => p.project === cur)) opts.push([cur, baseName(cur)]);
      proj.replaceChildren(...opts.map(([v, l]) => h("option", { value: v, selected: v === cur, title: v }, l)));
    })
    .catch(() => {});
  proj.addEventListener("change", () => set({ project: proj.value }));
  const via = select("via", VIA, q.get("via") ?? "", "Trigger");
  via.addEventListener("change", () => set({ via: via.value }));
  const filtered = ["project", "via", "session"].some((k) => q.get(k));
  const list = pagedList({
    label: "Context packs",
    render: packRow,
    empty: filtered
      ? emptyState(
          "No packs match",
          "Clear the filters to see every pack.",
          h("a", { class: "btn", href: "#/packs" }, "Clear filters"),
        )
      : emptyState(
          "No context packs yet",
          "A pack is the block of memories an agent receives, usually when a session starts. Install the hooks and start a Claude Code session, or run a test recall to preview one.",
          h(
            "a",
            {
              class: "btn primary",
              href: href(["setup"], new URLSearchParams({ step: "hooks" })),
            },
            "Set up hooks",
          ),
        ),
    fetchPage: (cursor) =>
      get(
        "/api/injections",
        {
          project: q.get("project"),
          via: q.get("via"),
          session: q.get("session"),
          cursor,
        },
        ctx.signal,
      ),
  });
  const sessionChip = q.get("session")
    ? h(
        "div",
        { class: "active-filters" },
        h(
          "span",
          { class: "chip" },
          h("span", null, `Session ${short(q.get("session"))}`),
          h(
            "button",
            {
              type: "button",
              "aria-label": "Remove session filter",
              onclick: () => set({ session: "" }),
            },
            icon("x", 12),
          ),
        ),
      )
    : null;
  const el = h(
    "div",
    { class: "view" },
    h(
      "header",
      { class: "page-head" },
      h(
        "div",
        null,
        h("h1", { tabindex: "-1" }, "Context packs"),
        h("p", { class: "sub" }, "What each agent was given, and how much smaller it was than sending every memory."),
      ),
    ),
    h("div", { class: "toolbar", role: "search", "aria-label": "Filter packs" }, proj, via),
    sessionChip,
    list.el,
  );
  return { el };
}

// ---------------------------------------------------------------- detail
/** Splits pack markdown into a title, sections and items ending in [uid8]. */
function parsePack(text) {
  const out = { title: null, sections: [] };
  let cur = null;
  for (const line of text.split("\n")) {
    if (line.startsWith("# ")) out.title = line.slice(2);
    else if (line.startsWith("## ")) out.sections.push((cur = { name: line.slice(3), items: [] }));
    else if (line.startsWith("- ")) {
      const m = line.match(/^- (.*?)\s*\[([0-9a-f]{4,})\]\s*$/);
      if (!cur) out.sections.push((cur = { name: "", items: [] }));
      cur.items.push(m ? { text: m[1], uid: m[2] } : { text: line.slice(2), uid: null });
    } else if (line.trim() && cur) cur.items.push({ text: line.trim(), uid: null });
  }
  return out;
}

function summary(d) {
  const p = d.saved_pct;
  const name = d.project ? baseName(d.project) : "no project";
  let big;
  let body;
  if (p == null) {
    big = `${num(d.est_tokens)} tokens sent`;
    body = "This pack was recorded before Recalla tracked candidate totals, so there is nothing to compare it with.";
  } else if (p >= 0) {
    big = `${savedWords(p)} than sending everything`;
    body = `Recalla picked ${num(d.memory_count)} of ${num(d.candidate_count)} candidate memories for ${name}: ${num(d.est_tokens)} tokens, inside a ${num(d.budget)}-token budget.`;
  } else {
    big = "No saving on this pack";
    body = `${name} had only ${num(d.candidate_count)} candidate memories, so sending them all would have cost ${num(d.candidate_tokens)} tokens. The pack's headings and ids made it ${savedWords(p)}. Packs get smaller than the alternative once a project has more memories.`;
  }
  return h(
    "section",
    { class: "card pack-sum", "aria-label": "Size" },
    h("p", { class: "big" }, big),
    h("p", null, body),
    ruler(d.est_tokens, d.candidate_tokens),
  );
}

/** Plain reasons a memory made it into the pack, from its list row. */
function reasons(m, project) {
  const src = m.first_source;
  const scope =
    m.scope_project == null
      ? [icon("globe", 14), "Applies to every project"]
      : m.scope_path
        ? [icon("folder", 14), `Scoped to the folder ${m.scope_path}`]
        : [icon("folder", 14), m.scope_project === project ? "Belongs to this project" : `Belongs to ${baseName(m.scope_project)}`];
  let origin;
  if (src?.harness === "mcp") origin = [icon("memories", 14), "Saved by the agent with the remember tool"];
  else if (src?.session_id)
    origin = [icon("sessions", 14), h("span", null, "Learned from ", h("a", { href: sessionHref(src.harness ?? "any", src.session_id) }, `a ${AGENT_LABEL[src.harness] ?? ""} session`.replace("a  ", "a ")))];
  else origin = [icon("sessions", 14), "Learned from imported history"];
  const fresh = m.stale_reason
    ? [icon("alert", 14), h("span", null, h("strong", null, "Sent with a warning: "), `the code it points to changed (${m.stale_reason}).`)]
    : m.anchor_count
      ? [icon("anchor", 14), `Points to ${num(m.anchor_count)} ${m.anchor_count === 1 ? "file" : "files"}, unchanged since it was learned`]
      : [icon("anchor", 14), "Not tied to a file"];
  const rank = [icon("bolt", 14), `Ranked by kind, ${Math.round(m.confidence * 100)}% confidence and seen ${num(m.seen_count)} ${m.seen_count === 1 ? "time" : "times"}`];
  return h("ul", { class: "reasons" }, [scope, origin, fresh, rank].map((r) => h("li", null, ...r)));
}

/** Aggregate "why" for the whole pack: scope and freshness at a glance. */
function overview(rows, project) {
  const count = (f) => rows.filter(f).length;
  const here = count((m) => m.scope_project && m.scope_project === project);
  const global = count((m) => m.scope_project == null);
  const stale = count((m) => m.stale_reason || m.status === "needs_reverify");
  const fromAgent = count((m) => m.first_source?.harness === "mcp");
  const stat = (n, label) => h("div", null, h("dt", null, label), h("dd", { class: "num" }, num(n)));
  return h("dl", { class: "pack-stats" },
    stat(here, "From this project"), stat(global, "For every project"),
    stat(fromAgent, "Saved by the agent"), stat(stale, "Sent with a warning"));
}

function whyPanel(m, project) {
  if (!m) return h("p", { class: "muted" }, "Select a line in the pack to see why it was included.");
  return h("div", { class: "stack" },
    h("div", { class: "cluster" }, kindDot(m.kind), statusFlag(m.status)),
    h("p", { class: "why-text" }, m.text),
    reasons(m, project),
    h("div", null, h("a", { class: "btn sm", href: memoryHref(m.uid) }, "Open memory")));
}

function packDoc(d, rows, onSelect) {
  const parsed = parsePack(d.pack_text);
  const byPrefix = (uid) => rows.find((m) => m.uid.startsWith(uid));
  const buttons = [];
  const doc = h("div", { class: "pack-doc" },
    parsed.title ? h("p", { class: "title" }, parsed.title) : null,
    parsed.sections.map((sec) => [
      sec.name ? h("h3", null, sec.name, h("span", { class: "muted num" }, ` ${sec.items.length}`)) : null,
      h("ul", null, sec.items.map((it) => {
        const m = it.uid ? byPrefix(it.uid) : null;
        if (!m) return h("li", { class: "pack-line static" }, h("span", null, it.text), it.uid ? h("code", null, it.uid) : null);
        const b = h("button", { type: "button", class: "pack-line", "aria-pressed": "false", "data-uid": m.uid },
          h("span", null, it.text), h("code", null, it.uid));
        b.addEventListener("click", () => select(m));
        buttons.push(b);
        return h("li", null, b);
      })),
    ]));
  function select(m) {
    for (const b of buttons) b.setAttribute("aria-pressed", String(b.dataset.uid === m.uid));
    onSelect(m);
  }
  doc.addEventListener("keydown", (e) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const i = buttons.indexOf(document.activeElement);
    if (i < 0) return;
    e.preventDefault();
    const next = buttons[Math.max(0, Math.min(buttons.length - 1, i + (e.key === "ArrowDown" ? 1 : -1)))];
    next.focus();
    next.click();
  });
  doc.selectFirst = () => buttons[0]?.click();
  return doc;
}

function packCard(d, rows, onSelect) {
  if (d.pack_text == null) {
    return h("div", { class: "card pad" }, h("p", { class: "muted" }, "The exact text wasn't recorded for this pack. It was sent before Recalla stored pack text."));
  }
  const doc = packDoc(d, rows, onSelect);
  const raw = h("pre", { class: "code tall", tabindex: "0", "aria-label": "Pack text as sent", hidden: true }, d.pack_text);
  const toggle = h("button", { type: "button", class: "btn sm ghost", "aria-pressed": "false" }, "Show as sent");
  toggle.addEventListener("click", () => {
    const showRaw = raw.hidden;
    raw.hidden = !showRaw;
    doc.hidden = showRaw;
    toggle.setAttribute("aria-pressed", String(showRaw));
  });
  const card = h("div", { class: "card" },
    h("div", { class: "code-bar" }, h("span", null, `${num(d.est_tokens)} tokens, ${num(d.memory_count)} memories`),
      h("span", { class: "cluster" }, toggle, copyButton(() => d.pack_text, "Copy pack text"))),
    doc, raw);
  card.selectFirst = doc.selectFirst;
  return card;
}

function mountDetail(ctx) {
  const id = ctx.segments[1];
  const crumb = h("p", { class: "crumb" }, h("a", { href: "#/packs" }, "Context packs"));
  const el = h("div", { class: "view" }, crumb, h("h1", { class: "sr", tabindex: "-1" }, "Context pack"), loading("Loading pack"));
  ctx.title("Context pack");
  get(`/api/injections/${enc(id)}`, null, ctx.signal)
    .then((d) => {
      const name = d.project ? baseName(d.project) : "No project";
      ctx.title(`Pack for ${name}`);
      const rows = d.memories;
      const detail = h("div", { "aria-live": "polite" });
      const card = packCard(d, rows, (m) => detail.replaceChildren(whyPanel(m, d.project)));
      const missing = d.memory_count - rows.length;
      const side = h("aside", { class: "why-side", "aria-label": "Why these memories" },
        section("Why these memories", h("div", { class: "card pad stack" },
          rows.length ? overview(rows, d.project) : h("p", { class: "muted" }, "None of the memories in this pack still exist."),
          missing > 0 ? h("p", { class: "muted small" }, `${num(missing)} ${missing === 1 ? "memory was" : "memories were"} in the pack but no longer exist.`) : null)),
        rows.length && d.pack_text != null ? section("Selected line", h("div", { class: "card pad" }, detail)) : null,
        rows.length && d.pack_text == null ? section("Included memories", h("ul", { class: "side-list card pad" }, rows.map((m) => h("li", null, kindDot(m.kind), " ", h("a", { href: memoryHref(m.uid) }, m.text))))) : null);
      el.replaceChildren(
        crumb,
        h("header", { class: "page-head" }, h("div", { class: "stack gap-2" },
          h("h1", { tabindex: "-1" }, `What the agent was given in ${name}`),
          h("div", { class: "sess-meta" }, projectChip(d.project), h("span", null, VIA_LABEL[d.via] ?? d.via),
            h("time", { datetime: new Date(d.at).toISOString(), title: absTime(d.at) }, `${absTime(d.at)} (`, time(d.at), ")"),
            d.session_id ? h("a", { href: sessionHref(d.harness || "any", d.session_id) }, "Open the session") : null))),
        summary(d),
        h("div", { class: "pack-cols" }, section("The pack, as the agent read it", card, h("span", { class: "aside" }, "Select a line to see why")), side));
      card.selectFirst?.();
      el.querySelector("h1").focus({ preventScroll: true });
    })
    .catch((e) => {
      if (e.name === "AbortError") return;
      el.replaceChildren(crumb, h("h1", { tabindex: "-1" }, e.status === 404 ? "Pack not found" : "Context pack"),
        e.status === 404 ? emptyState("There is no pack with that number", null, h("a", { class: "btn", href: "#/packs" }, "All packs")) : errorState(e));
    });
  return { el };
}

export const mount = (ctx) => (ctx.segments.length > 1 ? mountDetail(ctx) : mountList(ctx));
