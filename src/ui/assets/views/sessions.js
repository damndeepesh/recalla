// Sessions: a list, and each session as a conversation grouped into turns.
// Tool calls collapse to one line each and expand to the input and output.
import { h, icon, num, time, absTime, short, baseName } from "../dom.js";
import { get, enc } from "../api.js";
import { go, href, sessionHref, memoryHref, packHref } from "../router.js";
import {
  pagedList,
  emptyState,
  errorState,
  loading,
  facts,
  flag,
  kindDot,
  statusFlag,
  pathChip,
  section,
  select,
  searchInput,
  projects,
  codeBlock,
  copyIcon,
  AGENT_LABEL,
  VIA_LABEL,
} from "../ui.js";

const HARNESSES = [
  ["", "Any agent"],
  ["claude-code", "Claude Code"],
  ["codex", "Codex"],
];
const plural = (n, one, many) => `${num(n)} ${n === 1 ? one : many}`;

// ------------------------------------------------------------------ list
function row(s) {
  return h(
    "li",
    { class: "row" },
    h(
      "a",
      {
        class: "row-title",
        href: sessionHref(s.harness, s.session_id),
        title: s.project ?? "",
      },
      s.project ? baseName(s.project) : "No project",
      h("span", { class: "sr" }, `, session ${short(s.session_id)}`),
    ),
    h(
      "div",
      { class: "row-meta" },
      h("span", null, AGENT_LABEL[s.harness] ?? s.harness),
      h("span", { class: "num" }, plural(s.event_count, "event", "events")),
      s.memory_count ? h("span", { class: "num" }, plural(s.memory_count, "memory", "memories")) : null,
      s.injection_count
        ? h("span", { class: "num" }, plural(s.injection_count, "pack received", "packs received"))
        : null,
      s.sidechain_event_count ? h("span", null, "with subagents") : null,
    ),
    h("div", { class: "row-side" }, time(s.last_ts)),
  );
}

function mountList(ctx) {
  ctx.title("Sessions");
  const q = ctx.query;
  const set = (patch, replace = false) => {
    const next = new URLSearchParams(q);
    for (const [k, v] of Object.entries(patch)) v ? next.set(k, v) : next.delete(k);
    go(["sessions"], next, replace);
  };
  const search = searchInput("q", "Search sessions", q.get("q") ?? "");
  const input = search.querySelector("input");
  let t = 0;
  input.addEventListener("input", () => {
    clearTimeout(t);
    t = setTimeout(() => set({ q: input.value.trim() }, true), 300);
  });
  const agent = select("harness", HARNESSES, q.get("harness") ?? "", "Agent");
  agent.addEventListener("change", () => set({ harness: agent.value }));
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
  const filtered = ["q", "harness", "project"].some((k) => q.get(k));
  const list = pagedList({
    label: "Sessions",
    render: row,
    empty: filtered
      ? emptyState(
          "No sessions match",
          "Try other words or clear the filters.",
          h("a", { class: "btn", href: "#/sessions" }, "Clear filters"),
        )
      : emptyState(
          "No sessions yet",
          "Import your history to read the Claude Code and Codex sessions already on this machine. New ones are captured once hooks are installed.",
          h(
            "a",
            {
              class: "btn primary",
              href: href(["setup"], new URLSearchParams({ step: "import" })),
            },
            "Import history",
          ),
        ),
    fetchPage: (cursor) =>
      get(
        "/api/sessions",
        {
          harness: q.get("harness"),
          project: q.get("project"),
          q: q.get("q"),
          cursor,
        },
        ctx.signal,
      ),
  });
  const el = h(
    "div",
    { class: "view" },
    h(
      "header",
      { class: "page-head" },
      h(
        "div",
        null,
        h("h1", { tabindex: "-1" }, "Sessions"),
        h("p", { class: "sub" }, "Every captured conversation, newest first."),
      ),
    ),
    h("div", { class: "toolbar", role: "search", "aria-label": "Find sessions" }, search, proj, agent),
    list.el,
  );
  if (document.activeElement?.name === "q")
    queueMicrotask(() => {
      input.focus();
      input.setSelectionRange(input.value.length, input.value.length);
    });
  return { el };
}

// --------------------------------------------------------- conversation
const LONG = 700;

/** A short label for a tool call's input: the command, path or pattern. */
function toolArg(input, project) {
  if (!input || typeof input !== "object") return "";
  const v =
    input.command ??
    input.file_path ??
    input.path ??
    input.pattern ??
    input.url ??
    input.query ??
    input.description ??
    input.prompt ??
    (Array.isArray(input.todos)
      ? `${input.todos.length} items`
      : Object.values(input).find((x) => typeof x === "string"));
  if (typeof v !== "string") return "";
  const s = project && v.startsWith(`${project}/`) ? v.slice(project.length + 1) : v;
  return s.split("\n")[0];
}
function parse(text) {
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}
const lines = (t) => (t ? t.split("\n").length : 0);

/** Event text with an expander; "Show full" fetches the rest when truncated. */
function longText(e, cls, signal) {
  const pre = h(cls === "msg" ? "div" : "pre", { class: cls }, e.text);
  const needs = e.truncated || e.text.length > LONG || lines(e.text) > 8;
  if (!needs) return pre;
  pre.classList.add("clip");
  let full = !e.truncated;
  let open = false;
  const btn = h("button", { type: "button", class: "link-btn", "aria-expanded": "false" }, "Show more");
  btn.addEventListener("click", async () => {
    if (!open && !full) {
      btn.disabled = true;
      try {
        pre.textContent = (await get(`/api/events/${enc(e.id)}/text`, null, signal)).text;
        full = true;
      } catch (err) {
        btn.disabled = false;
        btn.textContent = `Couldn't load the rest: ${err.message} Try again`;
        return;
      }
      btn.disabled = false;
    }
    open = !open;
    pre.classList.toggle("clip", !open);
    btn.setAttribute("aria-expanded", String(open));
    btn.textContent = open ? "Show less" : "Show more";
  });
  return h("div", { class: "stack", css: { gap: "var(--sp-1)" } }, pre, btn);
}

function tsLabel(ts) {
  return ts
    ? h(
        "time",
        { datetime: new Date(ts).toISOString(), title: absTime(ts) },
        new Date(ts).toLocaleTimeString([], {
          hour: "2-digit",
          minute: "2-digit",
        }),
      )
    : null;
}

/** One tool call (plus its result once it arrives) as a one-line disclosure. */
function toolItem(call, project, signal) {
  const input = parse(call.text);
  const res = h("span", { class: "tool-res" }, "running");
  const body = h(
    "div",
    { class: "tool-detail" },
    codeBlock(input ? JSON.stringify(input, null, 2) : call.text, `${call.tool_name ?? "Tool"} input`),
  );
  const det = h(
    "details",
    { class: "tool" },
    h(
      "summary",
      null,
      icon("chev", 14),
      h("span", { class: "tool-name" }, call.tool_name ?? "tool"),
      h("span", { class: "tool-arg" }, toolArg(input, project) || "no input"),
      res,
    ),
    body,
  );
  det.setResult = (r) => {
    det.classList.toggle("is-error", r.is_error);
    res.textContent = r.is_error ? "failed" : r.text.trim() ? plural(lines(r.text.trim()), "line", "lines") : "done";
    body.append(
      h("div", { class: "small muted" }, r.is_error ? "Error output" : "Output"),
      longText(r, "code", signal),
    );
  };
  return det;
}

function conversation(harness, id, project, signal) {
  const root = h("ol", { class: "turns", "aria-label": "Conversation" });
  const holder = h("div", { class: "stack" }, root);
  const status = h("p", { class: "sr", role: "status", "aria-live": "polite" });
  let turn = null; // {body, sub, pending: [toolItem]}
  let count = 0;

  function newTurn(user) {
    const n = ++count;
    const body = h("div", { class: "turn-body" });
    const li = h(
      "li",
      { class: "turn", "aria-label": `Turn ${n}` },
      user
        ? h(
            "div",
            { class: "turn-user" },
            h("div", { class: "who" }, h("span", null, "You"), tsLabel(user.ts)),
            longText(user, "msg", signal),
          )
        : null,
      body,
    );
    root.append(li);
    turn = { body, sub: null, pending: [] };
  }
  function target(e) {
    if (!e.sidechain) {
      turn.sub = null;
      return turn.body;
    }
    if (!turn.sub) {
      const inner = h("div", {
        class: "turn-body",
        css: { padding: "var(--sp-2) 0 0" },
      });
      const det = h("details", { class: "sub" }, h("summary", null, icon("chev", 14), "Subagent work"), inner);
      det.inner = inner;
      turn.body.append(det);
      turn.sub = det;
    }
    return turn.sub.inner;
  }
  function add(e) {
    if (e.kind === "user_text" && !e.sidechain) return newTurn(e);
    if (!turn) newTurn(null);
    const box = target(e);
    if (e.kind === "tool_call") {
      const item = toolItem(e, project, signal);
      turn.pending.push(item);
      box.append(item);
    } else if (e.kind === "tool_result") {
      const call = turn.pending.shift();
      if (call) call.setResult(e);
      else
        box.append(
          h("div", { class: "small muted" }, e.is_error ? flag("Tool failed", "err") : "Tool output"),
          longText(e, "code", signal),
        );
    } else {
      const who = e.kind === "user_text" ? "Instruction to the subagent" : e.kind === "summary" ? "Summary" : null;
      box.append(
        h(
          "div",
          null,
          who ? h("div", { class: "who" }, h("span", null, who), tsLabel(e.ts)) : null,
          longText(e, "msg", signal),
        ),
      );
    }
  }

  async function load(after, more) {
    if (more) {
      more.disabled = true;
      more.textContent = "Loading…";
    } else root.replaceChildren(h("li", null, loading("Loading conversation")));
    try {
      const page = await get(`/api/sessions/${enc(harness)}/${enc(id)}/events`, { after, limit: 200 }, signal);
      if (!more) root.replaceChildren();
      more?.remove();
      if (!more && !page.items.length) {
        holder.replaceChildren(emptyState("No messages stored", "This session has no captured events."));
        return;
      }
      for (const e of page.items) add(e);
      status.textContent = `${count} turns shown.`;
      if (page.next != null) {
        const btn = h(
          "button",
          { type: "button", class: "btn", onclick: () => load(page.next, btn) },
          "Load more of this conversation",
        );
        holder.append(btn);
      }
    } catch (e) {
      if (e.name === "AbortError" || e.status === 401) return;
      const box = errorState(e, () => {
        box.remove();
        load(after, more);
      });
      if (more) {
        more.disabled = false;
        more.textContent = "Load more of this conversation";
      }
      holder.append(box);
    }
  }
  load(null, null);
  holder.append(status);
  return holder;
}

function duration(a, b) {
  if (a == null || b == null) return null;
  const m = Math.round((b - a) / 60000);
  return m < 1 ? "under a minute" : m < 90 ? `${m} min` : `${(m / 60).toFixed(1)} h`;
}

function aside(d) {
  const s = d.session;
  const mems = d.memories;
  const shown = mems.slice(0, 8);
  const memItem = (m) =>
    h(
      "li",
      null,
      h("div", { class: "cluster" }, kindDot(m.kind), statusFlag(m.status)),
      h("a", { href: memoryHref(m.uid) }, m.text),
    );
  return h(
    "aside",
    { "aria-label": "About this session" },
    section(
      "About",
      h(
        "div",
        { class: "card pad" },
        facts([
          ["Agent", AGENT_LABEL[s.harness] ?? s.harness],
          ["Started", s.first_ts && absTime(s.first_ts)],
          ["Length", duration(s.first_ts, s.last_ts)],
          [
            "Events",
            `${num(s.event_count)}${s.sidechain_event_count ? `, ${num(s.sidechain_event_count)} by subagents` : ""}`,
          ],
          ["Project", s.project && pathChip(s.project, "project path")],
          [
            "Id",
            h(
              "span",
              { class: "cluster" },
              h("code", null, short(s.session_id)),
              copyIcon(() => s.session_id, "Copy session id"),
            ),
          ],
        ]),
      ),
    ),
    section(
      `Memories learned (${num(mems.length)})`,
      mems.length
        ? h(
            "div",
            { class: "stack" },
            h("ul", { class: "side-list" }, shown.map(memItem)),
            mems.length > shown.length
              ? h(
                  "details",
                  null,
                  h("summary", { class: "link-btn" }, `Show ${mems.length - shown.length} more`),
                  h(
                    "ul",
                    {
                      class: "side-list",
                      css: { "margin-top": "var(--sp-3)" },
                    },
                    mems.slice(8).map(memItem),
                  ),
                )
              : null,
          )
        : h("p", { class: "muted small" }, "Nothing from this session became a memory."),
    ),
    section(
      `Context packs received (${num(d.injections.length)})`,
      d.injections.length
        ? h(
            "ul",
            { class: "side-list" },
            d.injections.map((i) =>
              h(
                "li",
                { class: "cluster" },
                h("a", { href: packHref(i.id) }, `${num(i.est_tokens)} tokens`),
                h("span", { class: "muted small" }, VIA_LABEL[i.via] ?? i.via),
                time(i.at),
              ),
            ),
          )
        : h("p", { class: "muted small" }, "This session didn't receive a context pack."),
    ),
  );
}

function mountDetail(ctx) {
  let [, harness, id] = ctx.segments;
  const crumb = h("p", { class: "crumb" }, h("a", { href: "#/sessions" }, "Sessions"));
  const el = h(
    "div",
    { class: "view" },
    crumb,
    h("h1", { class: "sr", tabindex: "-1" }, "Session"),
    loading("Loading session"),
  );
  ctx.title(`Session ${short(id)}`);
  const known = ["claude-code", "codex"];
  // Packs only record a session id, so "any" tries each harness in turn.
  const find = async () => {
    for (const hn of harness === "any" ? known : [harness]) {
      try {
        const d = await get(`/api/sessions/${enc(hn)}/${enc(id)}`, null, ctx.signal);
        harness = hn;
        return d;
      } catch (e) {
        if (e.status !== 404 || harness !== "any" || hn === known.at(-1)) throw e;
      }
    }
  };
  find()
    .then((d) => {
      const s = d.session;
      if (ctx.segments[1] === "any") history.replaceState(null, "", sessionHref(harness, id));
      const name = s.project ? baseName(s.project) : "Session";
      ctx.title(`${name} session`);
      el.replaceChildren(
        crumb,
        h(
          "header",
          { class: "page-head" },
          h(
            "div",
            { class: "stack", css: { gap: "var(--sp-2)" } },
            h("h1", { tabindex: "-1" }, name),
            h(
              "div",
              { class: "sess-meta" },
              flag(AGENT_LABEL[s.harness] ?? s.harness),
              s.last_ts ? h("span", null, "Last active ", time(s.last_ts)) : null,
              h("span", { class: "num" }, plural(s.event_count, "event", "events")),
            ),
          ),
        ),
        h("div", { class: "split" }, h("div", null, conversation(harness, id, s.project, ctx.signal)), aside(d)),
      );
      if (document.activeElement === document.body || el.contains(document.activeElement))
        el.querySelector("h1").focus({ preventScroll: true });
    })
    .catch((e) => {
      if (e.name === "AbortError") return;
      el.replaceChildren(
        crumb,
        h("h1", { tabindex: "-1" }, e.status === 404 ? "Session not found" : "Session"),
        e.status === 404
          ? emptyState(
              "This session isn't in the database",
              "It may come from a file that hasn't been imported yet.",
              h("a", { class: "btn", href: "#/sessions" }, "All sessions"),
            )
          : errorState(e),
      );
    });
  return { el };
}

export const mount = (ctx) => (ctx.segments.length > 1 ? mountDetail(ctx) : mountList(ctx));
