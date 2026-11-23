import { h, num, time, lastSegments, short, absTime } from "../dom.js";
import { get, enc } from "../api.js";
import { pageHead, panel, pagedList, field, select, badge, emptyState, errorState, skeleton, kv, kindBadge, statusBadge } from "../ui.js";
import { filterForm, syncForm } from "../filters.js";
import { sessionHref, memoryHref, injectionHref } from "../router.js";

const HARNESSES = [["", "Any agent"], ["claude-code", "Claude Code"], ["codex", "Codex"]];
const LABEL = { "claude-code": "Claude Code", codex: "Codex" };

function row(s) {
  return h("li", { class: "row session-row" },
    h("div", { class: "row-main" },
      h("a", { class: "row-link", href: sessionHref(s.harness, s.session_id) }, s.project ? lastSegments(s.project) : "No project"),
      h("div", { class: "row-meta" }, badge(LABEL[s.harness] ?? s.harness), h("code", null, short(s.session_id)))),
    h("dl", { class: "row-stats" },
      h("div", null, h("dt", null, "Last active"), h("dd", null, time(s.last_ts))),
      h("div", null, h("dt", null, "Events"), h("dd", { class: "num" }, num(s.event_count), s.sidechain_event_count ? ` (${num(s.sidechain_event_count)} subagent)` : "")),
      h("div", null, h("dt", null, "Memories"), h("dd", { class: "num" }, num(s.memory_count))),
      h("div", null, h("dt", null, "Injections"), h("dd", { class: "num" }, num(s.injection_count)))));
}

function mountList(ctx) {
  ctx.title("Sessions");
  const q = ctx.query;
  const form = filterForm(["sessions"], [
    field("Search", h("input", { type: "search", name: "q", placeholder: "Search sessions", "data-search": true, autocomplete: "off" }), "grow"),
    field("Agent", select("harness", HARNESSES)),
    field("Project", h("input", { type: "text", name: "project", placeholder: "Path contains…", autocomplete: "off" })),
  ], { label: "Filter sessions" });
  const holder = h("div");
  const load = (query) => {
    syncForm(form, query);
    const empty = [...query.keys()].length
      ? emptyState("No sessions match", "Try clearing a filter.")
      : emptyState("No sessions yet", "Run Import history on the Connect page to read your existing Claude Code and Codex sessions.",
        h("a", { class: "btn", href: "#/connect" }, "Go to Connect"));
    const list = pagedList({
      label: "Sessions", empty, render: row,
      fetchPage: (cursor) => get("/api/sessions", { harness: query.get("harness"), project: query.get("project"), q: query.get("q"), cursor }, ctx.signal),
    });
    holder.replaceChildren(list.el);
  };
  load(q);
  return { el: h("div", { class: "view" }, pageHead("Sessions", "Every captured conversation, newest first."), form, holder), update: load };
}

const KIND_LABEL = { user_text: "You", assistant_text: "Assistant", tool_call: "Tool call", tool_result: "Tool result", summary: "Summary" };
const LONG = 600;

function eventItem(e, signal) {
  const label = e.kind === "tool_call" || e.kind === "tool_result" ? `${KIND_LABEL[e.kind]}${e.tool_name ? `: ${e.tool_name}` : ""}` : KIND_LABEL[e.kind] ?? e.kind;
  const text = h("pre", { class: "ev-text collapsed" }, e.text);
  const needsToggle = e.truncated || e.text.length > LONG || e.text.split("\n").length > 6;
  const li = h("li", { class: `event ev-${e.kind}${e.sidechain ? " sidechain" : ""}${e.is_error ? " is-error" : ""}` },
    h("div", { class: "ev-head" }, h("strong", null, label),
      e.sidechain ? badge("Subagent", "info") : null, e.is_error ? badge("Error", "err") : null,
      e.ts ? h("time", { datetime: new Date(e.ts).toISOString(), title: absTime(e.ts) }, new Date(e.ts).toLocaleTimeString()) : null),
    text);
  if (needsToggle) {
    let full = !e.truncated;
    let open = false;
    const btn = h("button", { type: "button", class: "link-btn", "aria-expanded": "false" }, "Show full");
    btn.addEventListener("click", async () => {
      if (!open && !full) {
        btn.disabled = true;
        try {
          text.textContent = (await get(`/api/events/${enc(e.id)}/text`, null, signal)).text;
          full = true;
        } catch (err) {
          btn.disabled = false;
          btn.textContent = `Couldn't load: ${err.message} Retry`;
          return;
        }
        btn.disabled = false;
      }
      open = !open;
      text.classList.toggle("collapsed", !open);
      btn.setAttribute("aria-expanded", String(open));
      btn.textContent = open ? "Show less" : "Show full";
    });
    li.append(btn);
  }
  return li;
}

function mountDetail(ctx) {
  let [, harness, id] = ctx.segments;
  const el = h("div", { class: "view" }, h("p", { class: "crumb" }, h("a", { href: "#/sessions" }, "Sessions")), skeleton(4));
  ctx.title(`Session ${short(id)}`);
  // Injections only record a session id, so "any" tries each harness in turn.
  const find = async () => {
    for (const hn of harness === "any" ? Object.keys(LABEL) : [harness]) {
      try {
        const d = await get(`/api/sessions/${enc(hn)}/${enc(id)}`, null, ctx.signal);
        harness = hn;
        return d;
      } catch (e) {
        if (e.status !== 404) throw e;
        if (hn === Object.keys(LABEL).at(-1) || harness !== "any") throw e;
      }
    }
  };
  find().then((d) => {
    const s = d.session;
    if (ctx.segments[1] === "any") history.replaceState(null, "", sessionHref(harness, id));
    ctx.title(`Session ${s.project ? lastSegments(s.project, 1) : short(id)}`);
    const events = pagedList({
      tag: "ol", rowClass: "timeline", label: "Event timeline",
      empty: emptyState("No events stored", "This session has no captured events."),
      render: (e) => eventItem(e, ctx.signal),
      fetchPage: (after) => get(`/api/sessions/${enc(harness)}/${enc(id)}/events`, { after, limit: 50 }, ctx.signal),
    });
    el.replaceChildren(
      h("p", { class: "crumb" }, h("a", { href: "#/sessions" }, "Sessions")),
      pageHead(s.project ? lastSegments(s.project) : "Session", h("span", null, badge(LABEL[s.harness] ?? s.harness), " ", h("code", null, s.session_id))),
      h("div", { class: "cols" },
        panel("Details", kv([["Project", s.project && h("code", null, s.project)], ["First event", s.first_ts && absTime(s.first_ts)],
          ["Last event", s.last_ts && absTime(s.last_ts)], ["Events", num(s.event_count)],
          ["Subagent events", s.sidechain_event_count ? num(s.sidechain_event_count) : null],
          ["Source files", s.sources.length ? s.sources.map((f) => h("code", { class: "block" }, f)) : null]])),
        h("div", { class: "stack" },
          panel(`Memories extracted (${d.memories.length})`, d.memories.length
            ? h("ul", { class: "plain" }, d.memories.map((m) => h("li", null, kindBadge(m.kind), " ", h("a", { href: memoryHref(m.uid) }, m.text), " ", m.status !== "active" ? statusBadge(m.status) : null)))
            : h("p", { class: "muted" }, "No memories came from this session.")),
          panel(`Injections received (${d.injections.length})`, d.injections.length
            ? h("ul", { class: "plain" }, d.injections.map((i) => h("li", null, h("a", { href: injectionHref(i.id) }, `Pack #${i.id}`), ` via ${i.via}, `, h("span", { class: "num" }, `${num(i.est_tokens)} tokens`), ", ", time(i.at))))
            : h("p", { class: "muted" }, "No context pack was recorded for this session.")))),
      panel("Timeline", events.el));
  }).catch((e) => {
    if (e.name === "AbortError") return;
    el.replaceChildren(h("p", { class: "crumb" }, h("a", { href: "#/sessions" }, "Sessions")),
      e.status === 404 ? emptyState("Session not found", "It may have been removed.", h("a", { class: "btn", href: "#/sessions" }, "All sessions")) : errorState(e));
  });
  return { el };
}

export const mount = (ctx) => (ctx.segments.length > 1 ? mountDetail(ctx) : mountList(ctx));
