// Shared components. One of each: row, flag, kind dot, path chip, token ruler,
// states, paged list, dialogs, copy. Everything renders through dom.h().
import { h, icon, copyText, num, baseName, midPath, time } from "./dom.js";
import { memoryHref, packHref } from "./router.js";

// ---- messages ----
/** Toast with an optional action ({label, run}); errors stay longer. */
export function toast(text, tone = "ok", action = null) {
  const box = document.getElementById("toasts");
  const el = h("div", { class: `toast ${tone}` }, h("span", null, text));
  const close = () => el.remove();
  if (action) {
    el.append(
      h(
        "button",
        {
          type: "button",
          class: "btn sm",
          onclick: () => {
            close();
            action.run();
          },
        },
        icon(action.icon ?? "undo", 14),
        action.label,
      ),
    );
  }
  box.append(el);
  setTimeout(close, action ? 10000 : tone === "err" ? 9000 : 4000);
}

/** Inline polite live region: `.set(text, "ok"|"err")`, `.clear()`. */
export function statusLine() {
  const el = h("p", { class: "status", role: "status", "aria-live": "polite" });
  el.set = (text, tone = "") => {
    el.className = `status ${tone}`;
    el.textContent = text;
  };
  el.clear = () => el.set("");
  return el;
}

// ---- vocabulary ----
export const KINDS = ["decision", "fact", "procedure", "gotcha", "preference", "task"];
export const KIND_LABEL = {
  decision: "Decision",
  fact: "Fact",
  procedure: "Procedure",
  gotcha: "Gotcha",
  preference: "Preference",
  task: "Task",
};
export const KIND_PLURAL = {
  decision: "Decisions",
  fact: "Facts",
  procedure: "Procedures",
  gotcha: "Gotchas",
  preference: "Preferences",
  task: "Tasks",
};
export const STATUS_LABEL = {
  active: "Active",
  needs_reverify: "Needs review",
  superseded: "Replaced",
  archived: "Forgotten",
};
export const STATUSES = Object.keys(STATUS_LABEL);
export const AGENT_LABEL = {
  "claude-code": "Claude Code",
  codex: "Codex",
  mcp: "Agent (MCP)",
};
export const VIA_LABEL = {
  hook: "Session start",
  mcp: "Agent asked",
  cli: "Command line",
};

// ---- small pieces ----
export const flag = (text, tone = "") => h("span", { class: `flag ${tone}` }, text);
export const kindDot = (k) => h("span", { class: `kind kind-${k}` }, KIND_LABEL[k] ?? k);

/** Status only when it needs a look: active memories show nothing. */
export function statusFlag(status) {
  if (status === "active") return null;
  return flag(STATUS_LABEL[status] ?? status, status === "needs_reverify" ? "attn" : "");
}

/** A project as people know it: its folder name, full path on hover. */
export const projectChip = (p) =>
  p
    ? h("span", { class: "chip", title: p }, icon("folder", 12), h("span", null, baseName(p)))
    : h("span", { class: "chip" }, icon("globe", 12), h("span", null, "All projects"));

/** Short middle-truncated path with a copy button; full path in the title. */
export function pathChip(path, label = "path") {
  if (!path) return h("span", { class: "muted" }, "not set");
  return h(
    "span",
    { class: "path", title: path },
    h("code", null, midPath(path)),
    h("span", { class: "sr" }, ` (full ${label}: ${path})`),
    copyIcon(() => path, `Copy ${label}`),
  );
}

export function pageHead(title, sub, ...actions) {
  return h(
    "header",
    { class: "page-head" },
    h("div", null, h("h1", { tabindex: "-1" }, title), sub ? h("p", { class: "sub" }, sub) : null),
    actions.length ? h("div", { class: "cluster" }, ...actions) : null,
  );
}

export const section = (title, body, aside) =>
  h("section", { class: "section" }, h("div", { class: "section-head" }, h("h2", null, title), aside), body);

export function facts(pairs) {
  const dl = h("dl", { class: "facts" });
  for (const [k, v] of pairs) {
    if (v == null || v === "") continue;
    dl.append(h("dt", null, k), h("dd", null, v));
  }
  return dl;
}

export function meter(v) {
  const pct = Math.round((v ?? 0) * 100);
  return h(
    "span",
    { class: "meter" },
    h("i", { "aria-hidden": "true", css: { "--w": `${pct}%` } }),
    h("span", { class: "num" }, `${pct}%`),
  );
}

// ---- states ----
export function skeleton(rows = 6) {
  return h(
    "div",
    { class: "skel-list card", "aria-hidden": "true" },
    Array.from({ length: rows }, () => h("div", { class: "skel" })),
  );
}

export function loading(label = "Loading") {
  return h("div", { role: "status", "aria-label": label }, skeleton(5));
}

export function emptyState(title, body, action) {
  return h("div", { class: "empty" }, h("h3", null, title), body ? h("p", null, body) : null, action);
}

export function errorState(err, retry) {
  return h(
    "div",
    { class: "error-box", role: "alert" },
    icon("alert"),
    h(
      "div",
      null,
      h("strong", null, "Couldn't load this."),
      h("p", null, err.message),
      retry ? h("button", { type: "button", class: "btn sm", onclick: retry }, "Try again") : null,
    ),
  );
}

// ---- token ruler: what was sent vs everything that could have been ----
/** Saved share as words. Positive pct = smaller than sending every candidate. */
export function savedWords(pct) {
  if (pct == null) return "not recorded";
  if (pct >= 0) return `${Math.round(pct)}% smaller`;
  return `${Math.round(-pct)}% larger`;
}

export function ruler(pack, candidates, { mini = false, labels = true } = {}) {
  if (pack == null || candidates == null) {
    return mini
      ? h("span", { class: "muted small" }, "no baseline")
      : h("p", { class: "muted" }, "Candidate totals weren't recorded for this pack.");
  }
  const max = Math.max(pack, candidates, 1);
  const over = pack > candidates;
  const track = h(
    "span",
    { class: "ruler-track", "aria-hidden": "true" },
    h("span", {
      class: `ruler-fill${over ? " over" : ""}`,
      css: { "--w": `${(pack / max) * 100}%` },
    }),
  );
  const sr = h("span", { class: "sr" }, `${num(pack)} tokens sent of ${num(candidates)} tokens in all candidates`);
  if (mini)
    return h(
      "span",
      {
        class: "ruler mini",
        title: `${num(pack)} of ${num(candidates)} tokens`,
      },
      track,
      sr,
    );
  return h(
    "div",
    { class: "ruler" },
    track,
    labels
      ? h(
          "div",
          { class: "ruler-legend", "aria-hidden": "true" },
          h("span", null, h("b", { class: "num" }, num(pack)), " tokens sent"),
          h("span", null, h("b", { class: "num" }, num(candidates)), " tokens if every candidate memory were sent"),
        )
      : null,
    sr,
  );
}

// ---- paged list (cursor + Load more) ----
/**
 * fetchPage(cursor) -> {items, next}; render(item) -> <li>. Returns
 * {el, reload()}. After "Load more", focus moves to the first new row.
 */
export function pagedList({ fetchPage, render, empty, label, head, onPage }) {
  const root = h("div", { class: "card list-card" });
  const status = h("p", { class: "sr", role: "status", "aria-live": "polite" });
  let cursor = null;
  let total = 0;
  let gen = 0;
  let list = null;
  let more = null;

  async function load(append) {
    const my = ++gen;
    if (!append) {
      total = 0;
      cursor = null;
      root.className = "list-card";
      root.replaceChildren(skeleton(6), status);
    } else {
      more.disabled = true;
      more.textContent = "Loading…";
    }
    try {
      const page = await fetchPage(append ? cursor : null);
      if (my !== gen) return;
      if (!append) {
        if (!page.items.length) {
          root.className = "list-card";
          root.replaceChildren(empty, status);
          onPage?.(0, false);
          return;
        }
        root.className = "card list-card";
        list = h("ul", { class: "rows", "aria-label": label, "data-rows": "" });
        root.replaceChildren(...[head?.(), list, status].filter(Boolean));
      }
      const firstNew = list.children.length;
      for (const it of page.items) list.append(render(it));
      total += page.items.length;
      cursor = page.next;
      more?.remove();
      more = null;
      if (cursor != null) {
        more = h("button", { type: "button", class: "load-more", onclick: () => load(true) }, "Load more");
        root.insertBefore(more, status);
      }
      onPage?.(total, cursor != null);
      status.textContent = append ? `Loaded ${page.items.length} more. ${total} shown.` : "";
      if (append) list.children[firstNew]?.querySelector("a")?.focus();
    } catch (e) {
      if (my !== gen || e.name === "AbortError" || e.status === 401) return;
      const box = errorState(e, () => load(append));
      if (append) {
        more.disabled = false;
        more.textContent = "Load more";
        root.insertBefore(box, status);
      } else {
        root.className = "list-card";
        root.replaceChildren(box, status);
      }
    }
  }
  load(false);
  return { el: root, reload: () => load(false) };
}

// ---- clipboard ----
async function doCopy(text, btn, labelEl) {
  const ok = await copyText(text);
  toast(ok ? "Copied to clipboard." : "Couldn't copy. Select the text and copy it manually.", ok ? "ok" : "err");
  if (ok && labelEl) {
    labelEl.textContent = "Copied";
    setTimeout(() => (labelEl.textContent = "Copy"), 1800);
  }
}

export function copyButton(getText, label = "Copy") {
  const text = h("span", null, "Copy");
  const btn = h("button", { type: "button", class: "btn sm", "aria-label": label }, icon("copy", 14), text);
  btn.addEventListener("click", () => doCopy(getText(), btn, text));
  return btn;
}

export function copyIcon(getText, label) {
  const btn = h("button", { type: "button", class: "icon-btn", "aria-label": label, title: label }, icon("copy", 13));
  btn.addEventListener("click", (e) => {
    e.preventDefault();
    e.stopPropagation();
    doCopy(getText(), btn);
  });
  return btn;
}

export function codeBlock(text, label, { copy = true, tall = false } = {}) {
  return h(
    "div",
    { class: "code" },
    copy
      ? h(
          "div",
          { class: "code-bar" },
          h("span", null, label),
          copyButton(() => text, `Copy ${label}`),
        )
      : null,
    h("pre", { class: tall ? "tall" : null, tabindex: "0", "aria-label": label }, text),
  );
}

// ---- dialogs ----
/** Modal confirm. Resolves true on confirm. Focus returns to the opener natively. */
export function confirmDialog({ title, body, confirm, danger = false }) {
  return new Promise((resolve) => {
    const dlg = h(
      "dialog",
      { "aria-labelledby": "dlg-title", closedby: "any" },
      h(
        "form",
        { method: "dialog", class: "dlg" },
        h("h2", { id: "dlg-title" }, title),
        h("p", null, body),
        h(
          "div",
          { class: "dlg-actions" },
          h("button", { type: "submit", value: "cancel", class: "btn", autofocus: true }, "Cancel"),
          h(
            "button",
            {
              type: "submit",
              value: "ok",
              class: `btn ${danger ? "solid-danger" : "primary"}`,
            },
            confirm,
          ),
        ),
      ),
    );
    dlg.addEventListener("close", () => {
      resolve(dlg.returnValue === "ok");
      dlg.returnValue = "";
      dlg.remove();
    });
    document.body.append(dlg);
    dlg.showModal();
  });
}

// ---- form controls ----
export function field(labelText, control, hint) {
  return h(
    "label",
    { class: "field" },
    h("span", null, labelText, hint ? h("span", { class: "hint" }, ` ${hint}`) : null),
    control,
  );
}
export function select(name, options, value = "", label) {
  return h(
    "select",
    { name, class: "select", "aria-label": label },
    options.map(([v, t]) => h("option", { value: v, selected: v === value }, t)),
  );
}
export function searchInput(name, placeholder, value = "") {
  return h(
    "div",
    { class: "search-field" },
    icon("search", 16),
    h("input", {
      type: "search",
      name,
      class: "input",
      placeholder,
      "aria-label": placeholder,
      value,
      autocomplete: "off",
      "data-search": "",
    }),
  );
}

/** Projects for pickers, fetched once per page load. */
let projectsPromise = null;
export function projects(get) {
  projectsPromise ??= get("/api/projects")
    .then((r) => r.items)
    .catch((e) => {
      projectsPromise = null;
      throw e;
    });
  return projectsPromise;
}
export const resetProjects = () => (projectsPromise = null);

// ---- rows (one pattern for every list) ----
/** A memory: text first; kind as a dot; status only when it needs a look. */
export function memoryRow(m, { link, selected = false, showProject = true } = {}) {
  return h(
    "li",
    { class: "row", "aria-current": selected ? "true" : null, "data-uid": m.uid },
    h("a", { class: "row-title clamp", href: link ?? memoryHref(m.uid) }, m.text),
    h(
      "div",
      { class: "row-meta" },
      kindDot(m.kind),
      statusFlag(m.status),
      showProject && "scope_project" in m ? projectChip(m.scope_project) : null,
    ),
    m.updated_at ? h("div", { class: "row-side" }, time(m.updated_at)) : null,
  );
}

/** A context pack: where it went, how big it was against everything. */
export function packRow(p) {
  return h(
    "li",
    { class: "row" },
    h(
      "a",
      { class: "row-title", href: packHref(p.id) },
      p.project ? baseName(p.project) : "No project",
      h("span", { class: "sr" }, `, pack ${p.id}`),
    ),
    h(
      "div",
      { class: "row-meta" },
      h("span", null, VIA_LABEL[p.via] ?? p.via),
      h("span", { class: "num" }, `${num(p.memory_count)} ${p.memory_count === 1 ? "memory" : "memories"}`),
      h("span", { class: "num" }, `${num(p.est_tokens)} tokens`),
      time(p.at),
    ),
    h(
      "div",
      { class: "row-side" },
      ruler(p.est_tokens, p.candidate_tokens, { mini: true }),
      h("div", { class: "small num" }, savedWords(p.saved_pct)),
    ),
  );
}
