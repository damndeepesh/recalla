import { h, num, time, lastSegments, absTime, savedText } from "../dom.js";
import { get, enc } from "../api.js";
import { pageHead, panel, pagedList, field, select, badge, emptyState, errorState, skeleton, kv, kindBadge, statusBadge, codeBlock, tokenBar } from "../ui.js";
import { filterForm, syncForm } from "../filters.js";
import { sessionHref, injectionHref, memoryHref } from "../router.js";

const VIA = [["", "Any source"], ["hook", "Hook"], ["mcp", "MCP"], ["cli", "CLI"]];

const savedTone = (p) => (p == null ? "neutral" : p >= 0 ? "ok" : "warn");

function row(i) {
  return h("li", { class: "row inj-row" },
    h("div", { class: "row-main" },
      h("a", { class: "row-link", href: injectionHref(i.id) }, `Pack #${i.id}`, h("span", { class: "muted" }, i.project ? `  for ${lastSegments(i.project)}` : "  (no project)")),
      h("div", { class: "row-meta" }, badge(i.via), time(i.at),
        i.session_id ? h("a", { class: "above", href: sessionHref(i.harness || "any", i.session_id) }, `session ${i.session_id.slice(0, 8)}`) : h("span", null, "no session"))),
    h("dl", { class: "row-stats" },
      h("div", null, h("dt", null, "Budget"), h("dd", { class: "num" }, num(i.budget))),
      h("div", null, h("dt", null, "Pack"), h("dd", { class: "num" }, `${num(i.est_tokens)} tokens`)),
      h("div", null, h("dt", null, "Candidates"), h("dd", { class: "num" }, i.candidate_count == null ? "n/a" : `${num(i.candidate_count)} (${num(i.candidate_tokens)})`)),
      h("div", null, h("dt", null, "Versus all"), h("dd", null, badge(savedText(i.saved_pct), savedTone(i.saved_pct))))));
}

function mountList(ctx) {
  ctx.title("What Claude got");
  const form = filterForm(["injections"], [
    field("Project", h("input", { type: "text", name: "project", placeholder: "Exact project path", autocomplete: "off", "data-search": true }), "grow"),
    field("Source", select("via", VIA)),
    field("Session id", h("input", { type: "text", name: "session", placeholder: "Session id", autocomplete: "off" })),
  ], { label: "Filter injections" });
  const holder = h("div");
  const load = (query) => {
    syncForm(form, query);
    const list = pagedList({
      label: "Injections", render: row,
      empty: [...query.keys()].length ? emptyState("No injections match", "Try clearing a filter.")
        : emptyState("Nothing has been injected yet", "When a hook, MCP call or the CLI hands memories to an agent, the exact pack shows up here.",
          h("a", { class: "btn", href: "#/connect" }, "Set up the connector")),
      fetchPage: (cursor) => get("/api/injections", { project: query.get("project"), via: query.get("via"), session: query.get("session"), cursor }, ctx.signal),
    });
    holder.replaceChildren(list.el);
  };
  load(ctx.query);
  return {
    el: h("div", { class: "view" }, pageHead("What Claude got", "The exact context handed to agents, and how it compares to everything Recalla could have sent."), form, holder),
    update: load,
  };
}

function mountDetail(ctx) {
  const id = ctx.segments[1];
  const crumb = h("p", { class: "crumb" }, h("a", { href: "#/injections" }, "What Claude got"));
  const el = h("div", { class: "view" }, crumb, skeleton(4));
  ctx.title(`Pack #${id}`);
  get(`/api/injections/${enc(id)}`, null, ctx.signal).then((d) => {
    el.replaceChildren(crumb.cloneNode(true),
      pageHead(`Pack #${d.id}`, h("span", null, badge(d.via), " ", d.project ? h("code", null, d.project) : "No project", " ", time(d.at))),
      panel("This pack versus all candidates", h("div", { class: "stack" }, tokenBar(d.est_tokens, d.candidate_tokens),
        h("p", { class: "muted" }, d.candidate_count == null ? "" : `${num(d.candidate_count)} candidate memories; this pack included ${num(d.memory_count)}. `,
          d.saved_pct == null ? "Savings not recorded." : d.saved_pct >= 0 ? `${d.saved_pct}% fewer tokens.` : `The pack was ${Math.abs(d.saved_pct)}% larger than the candidates, which happens on small data sets.`))),
      h("div", { class: "cols" },
        panel("Details", kv([["Sent", absTime(d.at)], ["Via", d.via], ["Budget", `${num(d.budget)} tokens`], ["Pack size", `${num(d.est_tokens)} tokens`],
          ["Session", d.session_id && h("a", { href: sessionHref(d.harness || "any", d.session_id) }, d.session_id)]])),
        panel(`Memories included (${d.memories.length})`, d.memories.length
          ? h("ul", { class: "plain" }, d.memories.map((m) => h("li", null, kindBadge(m.kind), " ", h("a", { href: memoryHref(m.uid) }, m.text), m.status !== "active" ? [" ", statusBadge(m.status)] : null)))
          : h("p", { class: "muted" }, "None of the memories in this pack still exist."))),
      panel("Exact pack text", d.pack_text == null
        ? h("p", { class: "muted" }, "The exact text wasn't recorded for this injection. It was sent before pack text recording was added.")
        : codeBlock(d.pack_text, "pack text", { tall: true })));
  }).catch((e) => {
    if (e.name === "AbortError") return;
    el.replaceChildren(crumb, e.status === 404 ? emptyState("Injection not found", null, h("a", { class: "btn", href: "#/injections" }, "All injections")) : errorState(e));
  });
  return { el };
}

export const mount = (ctx) => (ctx.segments.length > 1 ? mountDetail(ctx) : mountList(ctx));
