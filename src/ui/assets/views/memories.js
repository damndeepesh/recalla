// Memories: search + project scope up front, everything else behind Filters.
// Master-detail: the list stays put while a memory opens in the side pane
// (full page below 72rem). All state lives in the hash.
import { h, icon, num, time, absTime, short, baseName } from "../dom.js";
import { get, post, enc } from "../api.js";
import { go, href, memoryHref, sessionHref, packHref } from "../router.js";
import {
  pagedList,
  memoryRow,
  emptyState,
  errorState,
  loading,
  facts,
  flag,
  kindDot,
  statusFlag,
  projectChip,
  pathChip,
  section,
  meter,
  toast,
  statusLine,
  select,
  searchInput,
  projects,
  copyIcon,
  KINDS,
  KIND_LABEL,
  STATUS_LABEL,
  STATUSES,
  AGENT_LABEL,
  VIA_LABEL,
} from "../ui.js";

const DEFAULT_STATUS = "active,needs_reverify";
const SORTS = [
  ["updated", "Recently updated"],
  ["created", "Newest"],
  ["seen", "Most seen"],
  ["confidence", "Most confident"],
];
const FILTER_KEYS = ["q", "project", "kind", "status", "harness", "sort"];

/** The query that drives the list (the open memory is not part of it). */
const listKey = (q) => FILTER_KEYS.map((k) => `${k}=${q.get(k) ?? ""}`).join("&");
const listQuery = (q) => new URLSearchParams([...q].filter(([k, v]) => FILTER_KEYS.includes(k) && v));
const csv = (q, k) => (q.get(k) ?? "").split(",").filter(Boolean);

// ------------------------------------------------------------------ list
function toolbar(state) {
  const q = state.query;
  const search = searchInput("q", "Search memories", q.get("q") ?? "");
  const input = search.querySelector("input");
  let t = 0;
  input.addEventListener("input", () => {
    clearTimeout(t);
    t = setTimeout(() => state.set({ q: input.value.trim() }, true), 250);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      clearTimeout(t);
      state.set({ q: input.value.trim() }, true);
    }
  });
  const proj = select("project", [["", "All projects"]], "", "Project");
  projects(get)
    .then((items) => {
      const cur = q.get("project") ?? "";
      const opts = items.map((p) => [p.project, `${baseName(p.project)} (${num(p.memory_count)})`]);
      if (cur && !items.some((p) => p.project === cur)) opts.unshift([cur, baseName(cur)]);
      proj.replaceChildren(
        ...[["", "All projects"], ...opts].map(([v, l]) => h("option", { value: v, selected: v === cur, title: v }, l)),
      );
    })
    .catch(() => {});
  proj.addEventListener("change", () => state.set({ project: proj.value }));
  const sort = select("sort", SORTS, q.get("sort") ?? "updated");
  sort.classList.add("sm");
  sort.addEventListener("change", () => state.set({ sort: sort.value === "updated" ? "" : sort.value }));

  const nActive =
    csv(q, "kind").length +
    (q.get("harness") ? 1 : 0) +
    (q.get("status") && q.get("status") !== DEFAULT_STATUS ? 1 : 0);
  const panel = filtersPanel(state);
  panel.hidden = !state.filtersOpen;
  const toggle = h(
    "button",
    {
      type: "button",
      class: "btn",
      "aria-expanded": String(state.filtersOpen),
      "aria-controls": "mem-filters",
    },
    icon("filter", 16),
    "Filters",
    nActive ? h("span", { class: "count-badge num" }, String(nActive)) : null,
  );
  toggle.addEventListener("click", () => {
    state.filtersOpen = !state.filtersOpen;
    panel.hidden = !state.filtersOpen;
    toggle.setAttribute("aria-expanded", String(state.filtersOpen));
  });
  return [
    h("div", { class: "toolbar", role: "search", "aria-label": "Find memories" }, search, proj, toggle),
    panel,
    activeChips(state),
    sort,
  ];
}

function filtersPanel(state) {
  const q = state.query;
  const kinds = csv(q, "kind");
  const statuses = csv(q, "status").length ? csv(q, "status") : DEFAULT_STATUS.split(",");
  const box = (name, value, label, checked) => {
    const input = h("input", { type: "checkbox", name, value, checked });
    return h("label", { class: "check" }, input, label);
  };
  const form = h(
    "form",
    { class: "filters-panel card", id: "mem-filters", "aria-label": "Filters" },
    h(
      "fieldset",
      null,
      h("legend", null, "Kind"),
      KINDS.map((k) => box("kind", k, h("span", { class: `kind kind-${k}` }, KIND_LABEL[k]), kinds.includes(k))),
    ),
    h(
      "fieldset",
      null,
      h("legend", null, "Status"),
      STATUSES.map((s) => box("status", s, STATUS_LABEL[s], statuses.includes(s))),
    ),
    h(
      "fieldset",
      null,
      h("legend", null, "Learned from"),
      [["", "Any agent"], ...Object.entries(AGENT_LABEL)].map(([v, l]) =>
        h(
          "label",
          { class: "check" },
          h("input", {
            type: "radio",
            name: "harness",
            value: v,
            checked: (q.get("harness") ?? "") === v,
          }),
          l,
        ),
      ),
    ),
  );
  form.addEventListener("change", () => {
    const fd = new FormData(form);
    const st = fd.getAll("status").join(",");
    state.set({
      kind: fd.getAll("kind").join(","),
      status: st === DEFAULT_STATUS ? "" : st || DEFAULT_STATUS,
      harness: fd.get("harness") ?? "",
    });
  });
  form.addEventListener("submit", (e) => e.preventDefault());
  return form;
}

function activeChips(state) {
  const q = state.query;
  const chips = [];
  const chip = (label, patch) =>
    chips.push(
      h(
        "span",
        { class: "chip" },
        h("span", null, label),
        h(
          "button",
          {
            type: "button",
            "aria-label": `Remove filter: ${label}`,
            onclick: () => state.set(patch),
          },
          icon("x", 12),
        ),
      ),
    );
  if (q.get("q")) chip(`“${q.get("q")}”`, { q: "" });
  if (q.get("project")) chip(baseName(q.get("project")), { project: "" });
  for (const k of csv(q, "kind"))
    chip(KIND_LABEL[k] ?? k, {
      kind: csv(q, "kind")
        .filter((x) => x !== k)
        .join(","),
    });
  if (q.get("status") && q.get("status") !== DEFAULT_STATUS)
    chip(
      csv(q, "status")
        .map((s) => STATUS_LABEL[s] ?? s)
        .join(" or "),
      { status: "" },
    );
  if (q.get("harness"))
    chip(`From ${AGENT_LABEL[q.get("harness")] ?? q.get("harness")}`, {
      harness: "",
    });
  if (!chips.length) return null;
  return h(
    "div",
    { class: "active-filters", "aria-label": "Active filters", role: "group" },
    chips,
    h("a", { class: "link-btn", href: "#/memories" }, "Clear all"),
  );
}

// ---------------------------------------------------------------- detail
const ACTIONS = {
  confirm: {
    label: "Confirm",
    busy: "Confirming…",
    done: "Confirmed. It stays in context packs and its code links were re-checked.",
  },
  forget: {
    label: "Forget",
    busy: "Forgetting…",
    done: "Forgotten. Agents won't receive it any more.",
  },
  restore: {
    label: "Restore",
    busy: "Restoring…",
    done: "Restored. Agents will receive it again.",
  },
};
const available = (s) => [
  ...(s === "active" || s === "needs_reverify" ? ["confirm"] : []),
  ...(s !== "archived" ? ["forget"] : []),
  ...(s === "archived" || s === "superseded" ? ["restore"] : []),
];
const ORIGIN = {
  remember: "Saved by the agent",
  extract: "Learned from a session",
  extractor: "Learned from a session",
  backfill: "Learned from imported history",
  mirror: "Restored from the markdown mirror",
};

function provItem(p) {
  const where = [p.file, p.line != null ? `line ${p.line}` : null].filter(Boolean).join(", ");
  return h(
    "li",
    null,
    h(
      "div",
      { class: "cluster small" },
      h("strong", null, ORIGIN[p.origin] ?? p.origin),
      p.harness ? h("span", { class: "muted" }, AGENT_LABEL[p.harness] ?? p.harness) : null,
      p.session_id ? h("a", { href: sessionHref(p.harness ?? "any", p.session_id) }, "Open session") : null,
      where ? h("code", { class: "muted" }, where) : null,
      p.git_commit ? h("code", { class: "muted", title: p.git_commit }, `commit ${short(p.git_commit)}`) : null,
    ),
    p.excerpt ? h("blockquote", null, p.excerpt) : null,
  );
}

function detail(d, { onChange, closeHref, wide }) {
  const status = statusLine();
  const bar = h("div", { class: "mem-actions" });
  const run = async (name, undoOf) => {
    for (const b of bar.querySelectorAll("button")) b.disabled = true;
    status.set(ACTIONS[name].busy);
    try {
      const updated = await post(`/api/memories/${enc(d.uid)}/${name}`);
      const before = d.status;
      onChange({ ...d, ...updated });
      if (name === "forget")
        toast(ACTIONS.forget.done, "ok", {
          label: "Undo",
          run: () => run("restore", before),
        });
      else toast(undoOf ? "Undone." : ACTIONS[name].done);
    } catch (e) {
      if (e.status === 401) return;
      for (const b of bar.querySelectorAll("button")) b.disabled = false;
      status.set(`${ACTIONS[name].label} failed: ${e.message}`, "err");
    }
  };
  for (const name of available(d.status)) {
    const cls =
      name === "forget" ? "btn danger" : name === "confirm" && d.status === "needs_reverify" ? "btn primary" : "btn";
    bar.append(
      h(
        "button",
        { type: "button", class: cls, onclick: () => run(name) },
        icon(name === "forget" ? "x" : name === "restore" ? "undo" : "check", 15),
        ACTIONS[name].label,
      ),
    );
  }
  const full = d.text_full && d.text_full.trim() !== d.text.trim() ? d.text_full : null;
  const close = h(
    "a",
    {
      class: wide ? "icon-btn" : "link-btn",
      href: closeHref,
      "data-close-pane": "",
      "aria-label": "Close memory",
      title: "Close (Esc)",
    },
    wide ? icon("x", 18) : "Back to memories",
  );
  return h(
    "article",
    { class: "stack-lg", "aria-labelledby": "mem-title" },
    h(
      "div",
      { class: "mem-head" },
      h(
        "div",
        { class: "pane-bar" },
        h("div", { class: "cluster" }, kindDot(d.kind), statusFlag(d.status), projectChip(d.scope_project)),
        close,
      ),
      h("h2", { id: "mem-title", class: "mem-text", tabindex: "-1" }, d.text),
      d.status === "needs_reverify"
        ? h(
            "p",
            { class: "notice" },
            icon("alert", 16),
            h(
              "span",
              null,
              h("strong", null, "Needs review. "),
              d.stale_reason ? `${d.stale_reason[0].toUpperCase()}${d.stale_reason.slice(1)}. ` : "",
              "The code this memory points to has changed. Confirm it if it's still true, or forget it.",
            ),
          )
        : null,
      d.status === "archived"
        ? h(
            "p",
            { class: "notice" },
            icon("alert", 16),
            "Forgotten: agents don't receive this memory. Restore it to bring it back.",
          )
        : null,
      bar,
      status,
    ),
    full ? section("Full text", h("p", { class: "msg" }, full)) : null,
    section(
      "Where it came from",
      d.provenance.length
        ? h("ul", { class: "prov" }, d.provenance.map(provItem))
        : h("p", { class: "muted" }, "No source recorded."),
    ),
    section(
      "Code it points to",
      d.anchors.length
        ? h(
            "ul",
            { class: "plain" },
            d.anchors.map((a) =>
              h(
                "li",
                { class: "cluster" },
                pathChip(a.path, "file path"),
                a.symbol ? h("code", null, a.symbol) : null,
                a.stale_reason ? flag(a.stale_reason, "attn") : flag("Unchanged", "ok"),
              ),
            ),
          )
        : h("p", { class: "muted small" }, "Not tied to any file, so code changes never mark it for review."),
    ),
    section(
      `Sent to agents (${num(d.injections.length)}${d.injections.length >= 20 ? "+" : ""})`,
      d.injections.length
        ? h(
            "ul",
            { class: "plain" },
            d.injections.map((i) =>
              h(
                "li",
                { class: "cluster" },
                h("a", { href: packHref(i.id) }, `Pack ${i.id}`),
                h("span", { class: "muted" }, VIA_LABEL[i.via] ?? i.via),
                time(i.at),
              ),
            ),
          )
        : h("p", { class: "muted small" }, "Not included in any recorded context pack yet."),
    ),
    section(
      "Details",
      facts([
        ["Applies to", d.scope_project ? pathChip(d.scope_project, "project path") : "All projects"],
        ["Folder", d.scope_path && pathChip(d.scope_path, "folder")],
        ["Confidence", meter(d.confidence)],
        ["Seen", `${num(d.seen_count)} ${d.seen_count === 1 ? "time" : "times"}`],
        ["Importance", num(d.importance)],
        ["Learned", absTime(d.created_at)],
        ["Updated", absTime(d.updated_at)],
        ["Valid until", d.valid_to && absTime(d.valid_to)],
        ["Replaced by", d.superseded_by && h("a", { href: memoryHref(d.superseded_by) }, short(d.superseded_by))],
        [
          "Id",
          h(
            "span",
            { class: "cluster" },
            h("code", null, short(d.uid)),
            copyIcon(() => d.uid, "Copy memory id"),
          ),
        ],
      ]),
    ),
  );
}

// ------------------------------------------------------------------ mount
export function mount(ctx) {
  ctx.title("Memories");
  const wideMq = matchMedia("(min-width: 72rem)");
  const state = {
    query: ctx.query,
    uid: ctx.segments[1] ?? null,
    filtersOpen: false,
    key: null,
  };
  const listHolder = h("div", { class: "md-list" });
  const pane = h("div", { class: "pane", "aria-live": "off" });
  const md = h("div", { class: "md" }, listHolder, pane);
  let head = null;
  const sub = h(
    "p",
    { class: "sub" },
    "What Recalla has learned from your sessions. Open one to read where it came from.",
  );
  const el = h(
    "div",
    { class: "view wide" },
    md,
  );
  let paneAbort = null;

  state.set = (patch, replace = false) => {
    const q = listQuery(state.query);
    for (const [k, v] of Object.entries(patch)) {
      if (v) q.set(k, v);
      else q.delete(k);
    }
    go(["memories", ...(state.uid ? [state.uid] : [])], q, replace);
  };
  const rowHref = (uid) => memoryHref(uid, listQuery(state.query));

  head = h("header", { class: "page-head" }, h("div", null, h("h1", { tabindex: "-1" }, "Memories"), sub));

  function drawList() {
    const q = state.query;
    const [bar, panel, chips, sort] = toolbar(state);
    const shown = h("span", { class: "num" });
    const filtered = FILTER_KEYS.some((k) => q.get(k) && k !== "sort");
    const list = pagedList({
      label: "Memories",
      head: () => h("div", { class: "list-head" }, shown, h("label", { class: "cluster" }, h("span", null, "Sort"), sort)),
      onPage: (n, more) => (shown.textContent = more ? `First ${num(n)} shown` : `${num(n)} ${n === 1 ? "memory" : "memories"}`),
      render: (m) =>
        memoryRow(m, {
          link: rowHref(m.uid),
          selected: m.uid === state.uid,
          showProject: !q.get("project"),
        }),
      empty: filtered
        ? emptyState(
            "No memories match",
            "Try other words, or clear a filter to widen the search.",
            h("a", { class: "btn", href: "#/memories" }, "Clear all filters"),
          )
        : emptyState(
            "No memories yet",
            "Memories appear after you import your history, or when an agent saves one with the remember tool.",
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
          "/api/memories",
          {
            kind: q.get("kind"),
            status: q.get("status"),
            project: q.get("project"),
            harness: q.get("harness"),
            q: q.get("q"),
            sort: q.get("sort"),
            cursor,
          },
          ctx.signal,
        ),
    });
    const hadSearchFocus = document.activeElement?.name === "q";
    listHolder.replaceChildren(...[head, bar, panel, chips, list.el].filter(Boolean));
    if (hadSearchFocus) {
      const i = listHolder.querySelector("[name=q]");
      i.focus();
      i.setSelectionRange(i.value.length, i.value.length);
    }
  }

  function markSelected() {
    for (const li of listHolder.querySelectorAll(".row[data-uid]")) {
      if (li.dataset.uid === state.uid) li.setAttribute("aria-current", "true");
      else li.removeAttribute("aria-current");
    }
  }

  function drawPane(focus) {
    paneAbort?.abort();
    md.classList.toggle("has-pane", Boolean(state.uid));
    if (!state.uid) {
      pane.replaceChildren();
      ctx.title("Memories");
      return;
    }
    paneAbort = new AbortController();
    const signal = AbortSignal.any([ctx.signal, paneAbort.signal]);
    const closeHref = href(["memories"], listQuery(state.query));
    pane.replaceChildren(loading("Loading memory"));
    get(`/api/memories/${enc(state.uid)}`, null, signal)
      .then((d) => {
        ctx.title(`Memory: ${d.text.slice(0, 48)}`);
        const draw = (m) => {
          pane.replaceChildren(
            detail(m, {
              onChange: (next) => {
                draw({ ...m, ...next });
                updateRow(next);
              },
              closeHref,
              wide: wideMq.matches,
            }),
          );
        };
        draw(d);
        if (focus) pane.querySelector("#mem-title")?.focus({ preventScroll: !wideMq.matches ? false : true });
        if (!wideMq.matches) window.scrollTo(0, 0);
      })
      .catch((e) => {
        if (e.name === "AbortError") return;
        pane.replaceChildren(
          h(
            "div",
            { class: "pane-bar" },
            h("span"),
            h("a", { class: "link-btn", href: closeHref, "data-close-pane": "" }, "Back to memories"),
          ),
          e.status === 404 || e.status === 409
            ? emptyState(
                e.status === 409 ? "That id matches several memories" : "Memory not found",
                e.status === 409 ? "Use a longer id." : "It may have been replaced by a newer memory.",
              )
            : errorState(e, () => drawPane(true)),
        );
      });
  }

  function updateRow(m) {
    const li = listHolder.querySelector(`.row[data-uid="${CSS.escape(m.uid)}"]`);
    if (!li) return;
    const fresh = memoryRow(m, {
      link: rowHref(m.uid),
      selected: true,
      showProject: !state.query.get("project"),
    });
    li.replaceWith(fresh);
  }

  function update(segments, query) {
    const prevUid = state.uid;
    state.query = query;
    state.uid = segments[1] ?? null;
    const key = listKey(query);
    if (key !== state.key) {
      state.key = key;
      drawList();
    } else markSelected();
    if (state.uid !== prevUid || !state.uid) {
      drawPane(Boolean(state.uid));
      if (!state.uid && prevUid) {
        const row = listHolder.querySelector(`.row[data-uid="${CSS.escape(prevUid)}"] .row-title`);
        row?.focus({ preventScroll: false });
      }
    }
  }
  const onMq = () => state.uid && drawPane(false);
  wideMq.addEventListener("change", onMq);
  ctx.onCleanup(() => wideMq.removeEventListener("change", onMq));

  state.key = listKey(state.query);
  drawList();
  drawPane(false);
  return { el, update };
}
