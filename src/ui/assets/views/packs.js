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

function why(m, project) {
  const src = m.first_source;
  const scope =
    m.scope_project == null
      ? [icon("globe", 13), "Applies to every project"]
      : m.scope_path
        ? [icon("folder", 13), `This folder: ${m.scope_path}`]
        : [icon("folder", 13), m.scope_project === project ? "This project" : `Project ${baseName(m.scope_project)}`];
  let origin;
  if (src?.harness === "mcp") origin = "Saved by the agent";
  else if (src?.session_id)
    origin = h(
      "a",
      { href: sessionHref(src.harness ?? "any", src.session_id) },
      `From a ${AGENT_LABEL[src.harness] ?? ""} session`.replace("  ", " "),
    );
  else origin = "From imported history";
  const fresh = m.stale_reason
    ? flag(`Needs review: ${m.stale_reason}`, "attn")
    : m.anchor_count
      ? h(
          "span",
          null,
          icon("anchor", 13),
          `Code unchanged (${num(m.anchor_count)} ${m.anchor_count === 1 ? "file" : "files"})`,
        )
      : h("span", null, icon("anchor", 13), "Not tied to code");
  return h(
    "li",
    { class: "why", id: `why-${m.uid.slice(0, 8)}`, tabindex: "-1" },
    h("div", { class: "cluster" }, kindDot(m.kind), statusFlag(m.status)),
    h("a", { class: "why-title", href: memoryHref(m.uid) }, m.text),
    h(
      "div",
      { class: "why-facts" },
      h("span", null, ...scope),
      h("span", null, origin),
      fresh,
      h("span", { class: "num", title: "How sure the extractor was" }, `${Math.round(m.confidence * 100)}% confident`),
      h("span", { class: "num", title: "Times this was seen across sessions" }, `seen ${num(m.seen_count)}×`),
    ),
  );
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

function packText(d, rows) {
  if (d.pack_text == null) {
    return h(
      "div",
      { class: "card pad" },
      h(
        "p",
        { class: "muted" },
        "The exact text wasn't recorded for this pack. It was sent before Recalla stored pack text.",
      ),
    );
  }
  const parsed = parsePack(d.pack_text);
  const byPrefix = (uid) => rows.find((m) => m.uid.startsWith(uid));
  const highlight = (uid, on) => {
    for (const x of document.querySelectorAll(`[data-hl="${CSS.escape(uid)}"], #why-${CSS.escape(uid)}`))
      x.classList.toggle("hl", on);
  };
  const doc = h(
    "div",
    { class: "pack-doc" },
    parsed.title ? h("p", { class: "title" }, parsed.title) : null,
    parsed.sections.map((s) => [
      s.name ? h("h3", null, s.name) : null,
      h(
        "ul",
        null,
        s.items.map((it) => {
          const m = it.uid ? byPrefix(it.uid) : null;
          const li = h(
            "li",
            { class: "pack-line", "data-hl": it.uid ?? "" },
            h("span", null, it.text),
            it.uid ? h("code", null, it.uid) : null,
          );
          if (m) {
            const key = m.uid.slice(0, 8);
            li.dataset.hl = key;
            li.addEventListener("mouseenter", () => highlight(key, true));
            li.addEventListener("mouseleave", () => highlight(key, false));
          }
          return li;
        }),
      ),
    ]),
  );
  const raw = h(
    "pre",
    {
      class: "code tall",
      tabindex: "0",
      "aria-label": "Pack text as sent",
      hidden: true,
    },
    d.pack_text,
  );
  const toggle = h("button", { type: "button", class: "btn sm ghost", "aria-pressed": "false" }, "Show as sent");
  toggle.addEventListener("click", () => {
    const showRaw = raw.hidden;
    raw.hidden = !showRaw;
    doc.hidden = showRaw;
    toggle.setAttribute("aria-pressed", String(showRaw));
  });
  return section(
    "The pack, as the agent read it",
    h(
      "div",
      { class: "card" },
      h(
        "div",
        { class: "code-bar" },
        h("span", null, `${num(d.est_tokens)} tokens`),
        h(
          "span",
          { class: "cluster" },
          toggle,
          copyButton(() => d.pack_text, "Copy pack text"),
        ),
      ),
      doc,
      raw,
    ),
  );
}

function mountDetail(ctx) {
  const id = ctx.segments[1];
  const crumb = h("p", { class: "crumb" }, h("a", { href: "#/packs" }, "Context packs"));
  const el = h(
    "div",
    { class: "view" },
    crumb,
    h("h1", { class: "sr", tabindex: "-1" }, "Context pack"),
    loading("Loading pack"),
  );
  ctx.title("Context pack");
  get(`/api/injections/${enc(id)}`, null, ctx.signal)
    .then((d) => {
      const name = d.project ? baseName(d.project) : "No project";
      ctx.title(`Pack for ${name}`);
      const rows = d.memories;
      const whyList = rows.length
        ? h(
            "ul",
            { class: "rows card", "aria-label": "Included memories" },
            rows.map((m) => {
              const li = why(m, d.project);
              const key = m.uid.slice(0, 8);
              li.addEventListener("mouseenter", () => {
                for (const x of el.querySelectorAll(`[data-hl="${CSS.escape(key)}"]`)) x.classList.add("hl");
              });
              li.addEventListener("mouseleave", () => {
                for (const x of el.querySelectorAll(`[data-hl="${CSS.escape(key)}"]`)) x.classList.remove("hl");
              });
              return li;
            }),
          )
        : h("div", { class: "card pad" }, h("p", { class: "muted" }, "None of the memories in this pack still exist."));
      const missing = d.memory_count - rows.length;
      el.replaceChildren(
        crumb,
        h(
          "header",
          { class: "page-head" },
          h(
            "div",
            { class: "stack", css: { gap: "var(--sp-2)" } },
            h("h1", { tabindex: "-1" }, `What the agent was given in ${name}`),
            h(
              "div",
              { class: "sess-meta" },
              projectChip(d.project),
              h("span", null, VIA_LABEL[d.via] ?? d.via),
              h("time", { datetime: new Date(d.at).toISOString() }, absTime(d.at)),
              time(d.at),
              d.session_id ? h("a", { href: sessionHref(d.harness || "any", d.session_id) }, "Open the session") : null,
            ),
          ),
        ),
        summary(d),
        h(
          "div",
          { class: "pack-cols" },
          packText(d, rows),
          section(
            `Why these ${num(rows.length)} memories`,
            h(
              "div",
              { class: "stack" },
              whyList,
              missing > 0
                ? h(
                    "p",
                    { class: "muted small" },
                    `${num(missing)} more ${missing === 1 ? "was" : "were"} in the pack but no longer exist.`,
                  )
                : null,
            ),
            h("span", { class: "aside" }, "in pack order"),
          ),
        ),
      );
      el.querySelector("h1").focus({ preventScroll: true });
    })
    .catch((e) => {
      if (e.name === "AbortError") return;
      el.replaceChildren(
        crumb,
        h("h1", { tabindex: "-1" }, e.status === 404 ? "Pack not found" : "Context pack"),
        e.status === 404
          ? emptyState(
              "There is no pack with that number",
              null,
              h("a", { class: "btn", href: "#/packs" }, "All packs"),
            )
          : errorState(e),
      );
    });
  return { el };
}

export const mount = (ctx) => (ctx.segments.length > 1 ? mountDetail(ctx) : mountList(ctx));
