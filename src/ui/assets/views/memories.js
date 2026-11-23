import { h, fill, num, time, lastSegments, absTime, short } from "../dom.js";
import { get, post, enc } from "../api.js";
import { pageHead, panel, pagedList, field, select, checkGroup, emptyState, errorState, skeleton, kv, kindBadge, statusBadge, confidence, badge, KINDS, STATUSES, STATUS_LABEL, statusLine, confirmDialog, toast } from "../ui.js";
import { filterForm, syncForm } from "../filters.js";
import { sessionHref, injectionHref, memoryHref } from "../router.js";

const DEFAULT_STATUS = "active,needs_reverify";
const SORTS = [["updated", "Recently updated"], ["confidence", "Confidence"], ["seen", "Most seen"], ["created", "Newest"]];
const AGENTS = [["", "Any agent"], ["claude-code", "Claude Code"], ["codex", "Codex"], ["mcp", "MCP"]];

function row(m) {
  return h("li", { class: "row memory-row" },
    h("div", { class: "row-main" },
      h("div", { class: "row-title" }, kindBadge(m.kind), h("a", { class: "row-link clamp", href: memoryHref(m.uid) }, m.text)),
      h("div", { class: "row-meta" }, statusBadge(m.status),
        m.stale_reason ? h("span", { class: "stale" }, m.stale_reason) : null,
        h("span", null, m.scope_project ? lastSegments(m.scope_project) : "Global"),
        confidence(m.confidence),
        h("span", { class: "num" }, `seen ${num(m.seen_count)}×`),
        h("span", null, "updated ", time(m.updated_at)))));
}

function mountList(ctx) {
  ctx.title("Memories");
  const form = filterForm(["memories"], [
    h("div", { class: "filter-row" },
      field("Search", h("input", { type: "search", name: "q", placeholder: "Search memories", "data-search": true, autocomplete: "off" }), "grow"),
      field("Project", h("input", { type: "text", name: "project", placeholder: "Path contains…", autocomplete: "off" })),
      field("Agent", select("harness", AGENTS)),
      field("Sort", select("sort", SORTS))),
    h("div", { class: "filter-row" },
      checkGroup("kind", "Kind", KINDS.map((k) => [k, k[0].toUpperCase() + k.slice(1)]), []),
      checkGroup("status", "Status", STATUSES.map((s) => [s, STATUS_LABEL[s]]), DEFAULT_STATUS.split(","))),
  ], { defaults: { status: DEFAULT_STATUS, sort: "updated" }, label: "Filter memories" });
  const holder = h("div");
  const load = (query) => {
    syncForm(form, query, { status: DEFAULT_STATUS, sort: "updated" });
    const filtered = [...query.keys()].length > 0;
    const list = pagedList({
      label: "Memories", render: row,
      empty: filtered ? emptyState("No memories match", "Try clearing a filter or including archived ones.")
        : emptyState("No memories yet", "Run Import history on the Connect page, or let an agent call remember.",
          h("a", { class: "btn", href: "#/connect" }, "Go to Connect")),
      fetchPage: (cursor) => get("/api/memories", {
        kind: query.get("kind"), status: query.get("status"), project: query.get("project"),
        harness: query.get("harness"), q: query.get("q"), sort: query.get("sort"), cursor,
      }, ctx.signal),
    });
    holder.replaceChildren(list.el);
  };
  load(ctx.query);
  return { el: h("div", { class: "view" }, pageHead("Memories", "What Recalla has learned from your sessions."), form, holder), update: load };
}

const ACTIONS = {
  confirm: { label: "Confirm", done: "Memory confirmed and re-anchored.", help: "re-checks the anchors and marks it active" },
  forget: { label: "Forget", done: "Memory forgotten (archived).", help: "archives it so it's no longer injected" },
  restore: { label: "Restore", done: "Memory restored.", help: "makes an archived or superseded memory active again" },
};
const available = (status) => [
  ...(status === "active" || status === "needs_reverify" ? ["confirm"] : []),
  ...(status !== "archived" ? ["forget"] : []),
  ...(status === "archived" || status === "superseded" ? ["restore"] : []),
];

function mountDetail(ctx) {
  const uid = ctx.segments[1];
  const el = h("div", { class: "view" }, h("p", { class: "crumb" }, h("a", { href: "#/memories" }, "Memories")), skeleton(4));
  ctx.title(`Memory ${short(uid)}`);
  let d = null;

  const crumb = () => h("p", { class: "crumb" }, h("a", { href: "#/memories" }, "Memories"));

  function draw(msg, tone = "ok") {
    const status = statusLine();
    const bar = h("div", { class: "actions" });
    const run = async (name) => {
      if (name === "forget") {
        const ok = await confirmDialog({
          title: "Forget this memory?", confirm: "Forget memory", danger: true,
          body: "It will be archived and no longer injected into sessions. You can restore it later from this page.",
        });
        if (!ok) return;
      }
      for (const b of bar.querySelectorAll("button")) b.disabled = true;
      status.set("Working…");
      try {
        const updated = await post(`/api/memories/${enc(d.uid)}/${name}`);
        d = { ...d, ...updated };
        draw(ACTIONS[name].done);
      } catch (e) {
        if (e.status === 401) return;
        draw(`${ACTIONS[name].label} failed: ${e.message}`, "err");
      }
    };
    for (const name of available(d.status)) {
      bar.append(h("button", { type: "button", class: `btn ${name === "forget" ? "danger-ghost" : name === "confirm" ? "" : "ghost"}`, onclick: () => run(name) }, ACTIONS[name].label));
    }
    const help = available(d.status).map((n) => `${ACTIONS[n].label} ${ACTIONS[n].help}.`).join(" ");
    const sup = d.superseded_by;
    fill(el,
      crumb(),
      h("header", { class: "page-head" }, h("div", null,
        h("div", { class: "row-meta" }, kindBadge(d.kind), statusBadge(d.status), confidence(d.confidence)),
        h("h1", { tabindex: "-1", class: "mem-title" }, d.text))),
      d.stale_reason ? h("p", { class: "status warn" }, `Needs review: ${d.stale_reason}`) : null,
      h("div", { class: "action-bar" }, bar, status, h("p", { class: "muted small" }, help)),
      panel("Full text", h("div", { class: "fulltext" }, d.text_full)),
      h("div", { class: "cols" },
        panel("Details", kv([
          ["Scope", d.scope_project ? h("code", null, d.scope_project) : "Global"],
          ["Path scope", d.scope_path && h("code", null, d.scope_path)],
          ["Importance", num(d.importance)], ["Seen", `${num(d.seen_count)} ${d.seen_count === 1 ? "time" : "times"}`],
          ["Created", absTime(d.created_at)], ["Updated", absTime(d.updated_at)],
          ["Valid from", absTime(d.valid_from)], ["Valid until", d.valid_to && absTime(d.valid_to)],
          ["Superseded by", sup && h("a", { href: memoryHref(sup) }, short(sup))],
          ["Id", h("code", null, d.uid)]])),
        panel(`Injected into sessions (${d.injections.length})`, d.injections.length
          ? moreList(d.injections.map((i) => h("li", null, h("a", { href: injectionHref(i.id) }, `Pack #${i.id}`),
            ` via ${i.via}, ${num(i.est_tokens)} tokens, `, time(i.at),
            i.session_id ? [" ", h("a", { href: sessionHref(i.harness || "any", i.session_id) }, "session")] : null)))
          : h("p", { class: "muted" }, "No recorded injection included this memory yet."))),
      panel(`Provenance (${d.provenance.length})`, d.provenance.length
        ? h("ul", { class: "prov" }, d.provenance.map(provItem))
        : h("p", { class: "muted" }, "No provenance recorded.")),
      panel(`Anchors (${d.anchors.length})`, d.anchors.length
        ? h("ul", { class: "plain" }, d.anchors.map((a) => h("li", null, h("code", null, a.path), a.symbol ? [" ", h("code", null, a.symbol)] : null,
          a.stale_reason ? [" ", badge(a.stale_reason, "warn")] : [" ", badge("fresh", "ok")])))
        : h("p", { class: "muted" }, "Not tied to any file, so it never goes stale from code changes.")));
    if (msg) status.set(msg, tone);
  }

  get(`/api/memories/${enc(uid)}`, null, ctx.signal).then((r) => { d = r; ctx.title(`Memory: ${r.text.slice(0, 40)}`); draw(); }).catch((e) => {
    if (e.name === "AbortError") return;
    el.replaceChildren(crumb(), e.status === 404 || e.status === 409
      ? emptyState(e.status === 409 ? "Ambiguous id" : "Memory not found", e.message, h("a", { class: "btn", href: "#/memories" }, "All memories"))
      : errorState(e));
  });
  return { el };
}

/** First few items, the rest behind a native disclosure. */
function moreList(items, shown = 5) {
  const rest = items.slice(shown);
  return h("div", null, h("ul", { class: "plain" }, items.slice(0, shown)),
    rest.length ? h("details", { class: "more" }, h("summary", null, `Show ${rest.length} more`), h("ul", { class: "plain" }, rest)) : null);
}

function provItem(p) {
  const where = [p.file, p.line != null ? `line ${p.line}` : null].filter(Boolean).join(", ");
  return h("li", null,
    h("div", { class: "row-meta" }, badge(p.origin), p.harness ? h("span", null, p.harness) : null,
      p.session_id ? h("a", { href: sessionHref(p.harness ?? "claude-code", p.session_id) }, `session ${short(p.session_id)}`) : null,
      where ? h("code", null, where) : null, p.git_commit ? h("code", null, `commit ${short(p.git_commit)}`) : null),
    p.excerpt ? h("blockquote", null, p.excerpt) : null);
}

export const mount = (ctx) => (ctx.segments.length > 1 ? mountDetail(ctx) : mountList(ctx));
