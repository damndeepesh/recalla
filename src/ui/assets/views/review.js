// Review: a triage queue of memories that need a look (code changed under
// them, or another memory contradicts them). Keyboard first: j/k move,
// Enter shows why, c keep, e edit, p promote, f forget.
import { h, inline, icon, num, time, short, baseName } from "../dom.js";
import { get, post, enc } from "../api.js";
import { memoryHref } from "../router.js";
import {
  pagedList,
  emptyState,
  errorState,
  flag,
  kindDot,
  projectChip,
  pathChip,
  meter,
  toast,
  statusLine,
  provItem,
  projects,
  select,
} from "../ui.js";

const MAX_TEXT = 1000;
const LEAVE_MS = 240;
const GO_KEYS = new Set(["h", "o", "m", "r", "s", "p", "i", "c"]);
const RELATION = {
  contradicts: "Contradicts",
  "contradicted-by": "Contradicted by",
  supersedes: "Replaces",
  "superseded-by": "Replaced by",
};
const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)");
const sentence = (s) => (s ? `${s[0].toUpperCase()}${s.slice(1)}${/[.!?]$/.test(s) ? "" : "."}` : "");
const kbd = (k) => h("kbd", { "aria-hidden": "true" }, k);

// ------------------------------------------------------------------ why
function relationItem(r, signal) {
  const contra = r.relation.startsWith("contradict");
  const other = h("a", { href: memoryHref(r.uid) }, `memory ${short(r.uid)}`);
  const quote = h("blockquote", { hidden: true });
  get(`/api/memories/${enc(r.uid)}`, null, signal)
    .then((o) => {
      other.textContent = "the other memory";
      quote.replaceChildren(...inline(o.text));
      quote.hidden = false;
    })
    .catch(() => {}); // The id link still works.
  return h(
    "li",
    null,
    h(
      "p",
      null,
      h("strong", null, `${RELATION[r.relation] ?? r.relation} `),
      other,
      contra
        ? `. ${r.reason ? sentence(r.reason) : "They can't both be true."} Keep the one that's right and forget the other.`
        : ".",
    ),
    quote,
  );
}

function whyBody(d, signal) {
  const stale = d.anchors.filter((a) => a.stale_reason);
  const rels = (d.relations ?? []).slice(0, 3);
  const prov = d.provenance.find((p) => p.excerpt) ?? d.provenance[0];
  const parts = [];
  if (stale.length)
    parts.push(
      h(
        "div",
        { class: "rv-why-part" },
        h("h4", null, "Code it points to changed"),
        h(
          "ul",
          { class: "plain" },
          stale.map((a) =>
            h(
              "li",
              { class: "cluster" },
              pathChip(a.path, "file path"),
              a.symbol ? h("code", null, a.symbol) : null,
              flag(a.stale_reason, "attn"),
            ),
          ),
        ),
      ),
    );
  if (rels.length)
    parts.push(
      h(
        "div",
        { class: "rv-why-part" },
        h("h4", null, rels.some((r) => r.relation.startsWith("contradict")) ? "Conflicts" : "Related memories"),
        h(
          "ul",
          { class: "plain rv-rels" },
          rels.map((r) => relationItem(r, signal)),
        ),
      ),
    );
  if (!stale.length && !rels.length)
    parts.push(h("p", { class: "muted" }, "Flagged by a check. Keep it if it's still true, or edit it so it is."));
  if (prov)
    parts.push(
      h(
        "div",
        { class: "rv-why-part" },
        h("h4", null, "Where it came from"),
        h("ul", { class: "prov" }, provItem(prov)),
      ),
    );
  return parts;
}

// ----------------------------------------------------------------- mount
export function mount(ctx) {
  ctx.title("Review");
  const cards = new Map(); // uid -> card
  let count = null; // exact queue size from /api/overview
  let loaded = 0;
  let more = false;
  let projectList = null;
  let lastG = 0;

  const countEl = h("p", { class: "rv-count num", role: "status", "aria-live": "polite", "aria-atomic": "true" });
  const hint = h(
    "p",
    { class: "rv-hint small muted" },
    kbd("j"),
    kbd("k"),
    " move ",
    kbd("Enter"),
    " why ",
    kbd("c"),
    " keep ",
    kbd("e"),
    " edit ",
    kbd("p"),
    " promote ",
    kbd("f"),
    " forget ",
    kbd("?"),
    " all shortcuts",
  );
  const head = h(
    "header",
    { class: "page-head" },
    h(
      "div",
      null,
      h("h1", { tabindex: "-1" }, "Review"),
      h(
        "p",
        { class: "sub" },
        "Memories whose code changed or that disagree with another memory. Agents still get them until you decide.",
      ),
    ),
    countEl,
  );
  const holder = h("div");
  const el = h("div", { class: "view rv" }, head, hint, holder);

  const remaining = () => [...cards.values()].filter((c) => !c.done).length;
  function paintCount() {
    const n = count ?? loaded;
    countEl.textContent = n ? `${num(n)} to review` : loaded || count === 0 ? "All clear" : "";
    ctx.setReviewCount?.(n);
  }
  get("/api/overview", null, ctx.signal)
    .then((o) => {
      count = o.memories.by_status?.needs_reverify ?? null;
      paintCount();
    })
    .catch(() => {}); // The loaded count stands in.

  const allClear = () =>
    emptyState(
      "Nothing to review",
      "When code a memory points to changes, or two memories disagree, it lands here so you can keep, fix or forget it.",
      h("a", { class: "btn", href: "#/memories" }, "Browse all memories"),
      "done",
    );

  const list = pagedList({
    label: "Memories to review",
    fetchPage: (cursor) => get("/api/memories", { status: "needs_reverify", cursor }, ctx.signal),
    render: (m) => card(m).li,
    empty: allClear(),
    onPage: (n, hasMore) => {
      loaded = n;
      more = hasMore;
      paintCount();
    },
  });
  holder.append(list.el);

  // ------------------------------------------------------------- a card
  function card(m) {
    const id = `rv-${m.uid}`;
    const c = { m, done: false, why: null, panel: null };
    const status = statusLine();
    const whyEl = h("div", { class: "rv-why", id: `${id}-why`, hidden: true });
    const toggle = h(
      "button",
      {
        type: "button",
        class: "rv-text",
        "aria-expanded": "false",
        "aria-controls": whyEl.id,
        id: `${id}-t`,
      },
      h("span", { class: "rv-text-body" }, inline(m.text)),
      icon("chev", 16),
    );
    const reason = h(
      "p",
      { class: "rv-reason" },
      icon("alert", 15),
      h("span", null, m.stale_reason ? sentence(m.stale_reason) : "Needs a second look."),
    );
    c.reasonText = reason.lastChild;
    const btn = (name, label, ic, key, cls, run) =>
      h(
        "button",
        { type: "button", class: `btn sm ${cls}`, "aria-keyshortcuts": key, "data-act": name, onclick: run },
        icon(ic, 14),
        label,
        kbd(key),
      );
    const actions = h(
      "div",
      { class: "mem-actions rv-actions" },
      btn("keep", "Keep", "check", "c", "primary", () => keep(c)),
      btn("edit", "Edit", "pencil", "e", "", () => openEdit(c)),
      m.kind !== "task" ? btn("promote", "Promote", "globe", "p", "", () => openPromote(c)) : null,
      btn("forget", "Forget", "x", "f", "danger", () => forget(c)),
    );
    const slot = h("div", { class: "rv-slot" });
    const inner = h(
      "article",
      { class: "rv-inner", "aria-labelledby": toggle.id },
      h(
        "div",
        { class: "rv-meta" },
        kindDot(m.kind),
        projectChip(m.scope_project),
        h(
          "span",
          { class: "rv-conf", title: "Confidence" },
          h("span", { class: "sr" }, "Confidence "),
          meter(m.confidence),
        ),
        m.updated_at ? h("span", { class: "rv-time" }, time(m.updated_at)) : null,
      ),
      h("h2", { class: "rv-title" }, toggle),
      reason,
      whyEl,
      slot,
      actions,
      status,
    );
    const li = h("li", { class: "rv-card", "data-uid": m.uid }, inner);
    Object.assign(c, { li, inner, toggle, whyEl, status, actions, slot });
    toggle.addEventListener("click", () => setOpen(c, toggle.getAttribute("aria-expanded") !== "true"));
    li.addEventListener("focusin", () => loadWhy(c));
    cards.set(m.uid, c);
    return c;
  }

  function loadWhy(c) {
    if (c.why) return c.why;
    c.whyEl.replaceChildren(h("p", { class: "muted small" }, "Loading why it's here…"));
    c.why = get(`/api/memories/${enc(c.m.uid)}`, null, ctx.signal)
      .then((d) => {
        c.detail = d;
        if (!c.m.stale_reason && d.relations?.some((r) => r.relation.startsWith("contradict")))
          c.reasonText.textContent = "Disagrees with another memory.";
        c.whyEl.replaceChildren(...whyBody(d, ctx.signal));
      })
      .catch((e) => {
        if (e.name === "AbortError") return;
        c.why = null;
        c.whyEl.replaceChildren(errorState(e, () => loadWhy(c)));
      });
    return c.why;
  }

  function setOpen(c, open) {
    c.toggle.setAttribute("aria-expanded", String(open));
    c.whyEl.hidden = !open;
    c.li.classList.toggle("open", open);
    if (open) loadWhy(c);
  }

  // ----------------------------------------------------------- focus
  const live = () => [...cards.values()].filter((c) => !c.done && c.li.isConnected);
  const current = () => {
    const li = document.activeElement?.closest?.(".rv-card");
    const c = li && cards.get(li.dataset.uid);
    return c && !c.done ? c : null;
  };
  function focusCard(c) {
    c.toggle.focus({ preventScroll: true });
    c.li.scrollIntoView({ block: "nearest", behavior: reduceMotion.matches ? "auto" : "smooth" });
  }
  function move(step) {
    const all = live();
    if (!all.length) return false;
    const at = all.indexOf(current());
    focusCard(all[at < 0 ? 0 : Math.min(all.length - 1, Math.max(0, at + step))]);
    return true;
  }

  // ---------------------------------------------------------- resolve
  /** Marks a card handled, moves focus on, animates it out (or keeps it as a note). */
  function resolve(c, { keepNote = false } = {}) {
    const all = live();
    const i = all.indexOf(c);
    const next = all[i + 1] ?? all[i - 1];
    c.done = true;
    if (count != null) count = Math.max(0, count - 1);
    paintCount();
    if (next) focusCard(next);
    if (keepNote) return afterResolve(next);
    const gone = () => {
      c.li.remove();
      cards.delete(c.m.uid);
      afterResolve(next);
    };
    if (reduceMotion.matches) return gone();
    c.li.classList.add("leaving");
    setTimeout(gone, LEAVE_MS);
  }
  function afterResolve(next) {
    if (remaining()) return;
    const loadMore = list.el.querySelector(".load-more");
    if (more && loadMore) return loadMore.click();
    cards.clear();
    holder.replaceChildren(allClear());
    if (!next) head.querySelector("h1").focus();
  }

  async function act(c, name, path, body) {
    if (c.busy) return null;
    c.busy = true;
    for (const b of c.li.querySelectorAll("button")) b.disabled = true;
    c.status.set(`${name}…`);
    try {
      return await post(path, body);
    } catch (e) {
      if (e.status !== 401) c.status.set(`${name} failed: ${e.message}`, "err");
      return null;
    } finally {
      c.busy = false;
      if (!c.done) for (const b of c.li.querySelectorAll("button")) b.disabled = false;
    }
  }

  async function keep(c) {
    const r = await act(c, "Keeping", `/api/memories/${enc(c.m.uid)}/confirm`);
    if (!r) return;
    resolve(c);
    toast("Kept. Agents keep getting it, and its code links were re-checked.");
  }

  async function forget(c) {
    const r = await act(c, "Forgetting", `/api/memories/${enc(c.m.uid)}/forget`);
    if (!r) return;
    resolve(c);
    toast("Forgotten. Agents won't receive it any more.", "ok", {
      label: "Undo",
      run: async () => {
        try {
          await post(`/api/memories/${enc(c.m.uid)}/restore`);
          toast("Restored. Agents will receive it again.");
        } catch (e) {
          if (e.status !== 401) toast(`Undo failed: ${e.message}`, "err");
        }
      },
    });
  }

  // ------------------------------------------------------------ panels
  function closePanel(c, refocus = true) {
    c.panel?.remove();
    c.panel = null;
    c.actions.hidden = false;
    if (refocus) c.toggle.focus();
  }
  function showPanel(c, panel, first) {
    closePanel(c, false);
    c.panel = panel;
    c.actions.hidden = true;
    c.status.clear();
    c.slot.append(panel);
    panel.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        closePanel(c);
      }
    });
    first.focus();
  }

  function openEdit(c) {
    const cid = `rv-${c.m.uid}-n`;
    const area = h("textarea", {
      class: "input rv-area",
      rows: 3,
      maxlength: MAX_TEXT,
      "aria-label": "Memory text",
      "aria-describedby": cid,
      value: c.m.text,
    });
    const counter = h("span", { id: cid, class: "small muted num" });
    const paint = () => {
      const n = area.value.length;
      counter.textContent = `${num(n)} / ${num(MAX_TEXT)} characters`;
      counter.classList.toggle("rv-near", n > MAX_TEXT * 0.9);
    };
    area.addEventListener("input", paint);
    paint();
    const save = h("button", { type: "submit", class: "btn sm primary" }, icon("check", 14), "Save");
    const form = h(
      "form",
      { class: "rv-panel", "aria-label": "Edit memory" },
      area,
      h(
        "div",
        { class: "rv-panel-foot" },
        counter,
        h(
          "div",
          { class: "cluster" },
          h("span", { class: "small muted rv-keys" }, kbd("Ctrl"), kbd("Enter"), " save ", kbd("Esc"), " cancel"),
          h("button", { type: "button", class: "btn sm ghost", onclick: () => closePanel(c) }, "Cancel"),
          save,
        ),
      ),
    );
    area.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        form.requestSubmit();
      }
    });
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const text = area.value.trim();
      if (!text) return c.status.set("Write the memory before saving, or forget it instead.", "err");
      if (text === c.m.text.trim()) return c.status.set("Nothing changed. Use Keep if it's still true.", "err");
      area.readOnly = true;
      const row = await act(c, "Saving", `/api/memories/${enc(c.m.uid)}/edit`, { text });
      area.readOnly = false;
      if (!row) return area.focus();
      edited(c, row);
    });
    showPanel(c, form, area);
    area.setSelectionRange(area.value.length, area.value.length);
  }

  /** The original is superseded; the card now shows the new memory. */
  function edited(c, row) {
    c.panel = null;
    c.li.classList.add("resolved");
    c.li.dataset.uid = row.uid;
    c.inner.replaceChildren(
      h("div", { class: "rv-meta" }, flag("Edited", "ok"), kindDot(row.kind), projectChip(row.scope_project)),
      h("p", { class: "rv-title rv-text-body" }, inline(row.text)),
      h(
        "p",
        { class: "small muted" },
        "Saved as a new memory; the old wording is kept as replaced. ",
        h("a", { href: memoryHref(row.uid) }, "Open it"),
      ),
    );
    resolve(c, { keepNote: true });
    toast("Edited. Agents get the new wording from now on.");
  }

  async function openPromote(c) {
    if (c.m.kind === "task")
      return c.status.set("Tasks belong to one piece of work, so they can't be promoted.", "err");
    const global = !c.m.scope_project;
    const toAll = h(
      "button",
      {
        type: "button",
        class: "btn sm",
        disabled: global,
        onclick: () => promote(c, { scope: "global" }, "every project"),
      },
      icon("globe", 14),
      "Every project",
    );
    const proj = select("project", [["", "Loading projects…"]], "", "Project");
    proj.classList.add("sm");
    const toProj = h("button", { type: "button", class: "btn sm", disabled: true }, icon("folder", 14), "Move here");
    toProj.addEventListener("click", () => promote(c, { scope: "project", project: proj.value }, baseName(proj.value)));
    proj.addEventListener("change", () => (toProj.disabled = !proj.value));
    const panel = h(
      "div",
      { class: "rv-panel", role: "group", "aria-label": "Promote memory" },
      h(
        "p",
        { class: "small" },
        global
          ? "It already applies to every project. Pick one project to narrow it to."
          : "Every project makes it apply everywhere you work, not just here. The original is replaced.",
      ),
      h(
        "div",
        { class: "rv-panel-foot" },
        h("div", { class: "cluster" }, toAll, proj, toProj),
        h("button", { type: "button", class: "btn sm ghost", onclick: () => closePanel(c) }, "Cancel"),
      ),
    );
    showPanel(c, panel, global ? proj : toAll);
    try {
      projectList ??= await projects(get);
      const opts = projectList.filter((p) => p.project !== c.m.scope_project);
      proj.replaceChildren(
        h("option", { value: "" }, opts.length ? "Another project…" : "No other projects"),
        ...opts.map((p) => h("option", { value: p.project, title: p.project }, baseName(p.project))),
      );
    } catch (e) {
      proj.replaceChildren(h("option", { value: "" }, "Projects unavailable"));
      if (e.status !== 401) c.status.set(`Couldn't load projects: ${e.message}`, "err");
    }
  }

  async function promote(c, body, where) {
    const row = await act(c, "Promoting", `/api/memories/${enc(c.m.uid)}/promote`, body);
    if (!row) return;
    resolve(c);
    toast(`Promoted. It now applies to ${body.scope === "global" ? where : where || "that project"}.`);
  }

  // --------------------------------------------------------- keyboard
  function onKey(e) {
    const t = e.target;
    if (e.ctrlKey || e.metaKey || e.altKey || document.querySelector("dialog[open]")) return;
    if (t.closest?.("input, textarea, select, [contenteditable], .rv-panel")) return;
    if (e.key === "g") return (lastG = Date.now());
    const afterG = Date.now() - lastG < 1200 && GO_KEYS.has(e.key);
    lastG = 0;
    if (afterG) return; // "g c", "g p": app.js navigates
    const onCard = t.classList?.contains("rv-text");
    if (e.key === "j" || (onCard && e.key === "ArrowDown")) {
      if (move(1)) e.preventDefault();
      return;
    }
    if (e.key === "k" || (onCard && e.key === "ArrowUp")) {
      if (move(-1)) e.preventDefault();
      return;
    }
    const run = { c: keep, f: forget, e: openEdit, p: openPromote }[e.key];
    if (!run) return;
    const c = current();
    e.preventDefault();
    if (c) run(c);
    else move(1); // nothing picked yet: pick the first, act on the next press
  }
  document.addEventListener("keydown", onKey);
  ctx.onCleanup(() => document.removeEventListener("keydown", onKey));

  return { el };
}
