// Shared components: badges, states, paged lists, dialogs, copy, token bars.
import { h, icon, copyText } from "./dom.js";

// ---- messages ----
export function toast(text, tone = "ok") {
  const box = document.getElementById("toasts");
  const el = h("div", { class: `toast ${tone}` }, text);
  box.append(el);
  setTimeout(() => el.remove(), tone === "err" ? 9000 : 4000);
}

/** Inline polite live region: `.set(text, "ok"|"err")`, `.clear()`. */
export function statusLine() {
  const el = h("p", { class: "status", role: "status", "aria-live": "polite" });
  el.set = (text, tone = "ok") => {
    el.className = `status ${tone}`;
    el.textContent = text;
  };
  el.clear = () => {
    el.className = "status";
    el.textContent = "";
  };
  return el;
}

// ---- small pieces ----
export const badge = (text, tone = "neutral") => h("span", { class: `badge ${tone}` }, text);

const STATUS = {
  active: ["Active", "ok"],
  needs_reverify: ["Needs review", "warn"],
  superseded: ["Superseded", "neutral"],
  archived: ["Archived", "neutral"],
};
export const statusBadge = (s) => badge(...(STATUS[s] ?? [s, "neutral"]));
export const KINDS = ["decision", "fact", "procedure", "gotcha", "preference", "task"];
export const STATUSES = Object.keys(STATUS);
export const STATUS_LABEL = Object.fromEntries(Object.entries(STATUS).map(([k, v]) => [k, v[0]]));
export const kindBadge = (k) => h("span", { class: `kind kind-${k}` }, k);

export function confidence(c) {
  const pct = Math.round((c ?? 0) * 100);
  return h("span", { class: "conf", title: `Confidence ${c?.toFixed?.(2)}` },
    h("span", { class: "conf-bar", "aria-hidden": "true" }, h("span", { css: { "--w": `${pct}%` } })),
    h("span", { class: "num" }, `${pct}%`),
    h("span", { class: "sr" }, " confidence"));
}

export function kv(pairs, cls = "") {
  const dl = h("dl", { class: `kv ${cls}` });
  for (const [k, v] of pairs) {
    if (v == null || v === "") continue;
    dl.append(h("dt", null, k), h("dd", null, v));
  }
  return dl;
}

export const pageHead = (title, sub, ...actions) =>
  h("header", { class: "page-head" },
    h("div", null, h("h1", { tabindex: "-1" }, title), sub ? h("p", { class: "sub" }, sub) : null),
    actions.length ? h("div", { class: "head-actions" }, ...actions) : null);

export const panel = (title, body, extra) =>
  h("section", { class: "panel" },
    title ? h("div", { class: "panel-head" }, h("h2", null, title), extra) : null, body);

export const link = (href, ...c) => h("a", { href }, ...c);

// ---- states ----
export function skeleton(rows = 6) {
  return h("div", { class: "skel-list", "aria-hidden": "true" },
    Array.from({ length: rows }, () => h("div", { class: "skel" })));
}

export function emptyState(title, body, action) {
  return h("div", { class: "empty" }, h("h3", null, title), body ? h("p", null, body) : null, action);
}

export function errorState(err, retry) {
  return h("div", { class: "error-box", role: "alert" },
    icon("alert"),
    h("div", null, h("strong", null, "Couldn't load this."), h("p", null, err.message),
      retry ? h("button", { type: "button", class: "btn", onclick: retry }, "Try again") : null));
}

// ---- paged list with a Load more button ----
/**
 * fetchPage(cursor) -> {items, next}. render(item) -> <li>. Returns
 * {el, reload(), count()}. New items get focus on "Load more" so keyboard users
 * continue where they were.
 */
export function pagedList({ fetchPage, render, empty, label, tag = "ul", rowClass = "rows" }) {
  const root = h("div", { class: "paged" });
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
      root.replaceChildren(skeleton(), status);
    } else {
      more.disabled = true;
      more.textContent = "Loading…";
    }
    try {
      const page = await fetchPage(append ? cursor : null);
      if (my !== gen) return;
      if (!append) {
        if (!page.items.length) {
          root.replaceChildren(empty ?? emptyState("Nothing here yet"), status);
          return;
        }
        list = h(tag, { class: rowClass, "aria-label": label });
        root.replaceChildren(list, status);
      }
      const firstNew = list.children.length;
      for (const it of page.items) list.append(render(it));
      total += page.items.length;
      cursor = page.next;
      more?.remove();
      more = null;
      if (cursor != null) {
        more = h("button", { type: "button", class: "btn load-more", onclick: () => load(true) }, "Load more");
        root.insertBefore(more, status);
      }
      status.textContent = append ? `Loaded ${page.items.length} more. ${total} shown.` : "";
      if (append) list.children[firstNew]?.querySelector("a, button")?.focus();
    } catch (e) {
      if (my !== gen || e.name === "AbortError") return;
      const box = errorState(e, () => load(append));
      if (append) {
        more.disabled = false;
        more.textContent = "Load more";
        root.insertBefore(box, status);
      } else root.replaceChildren(box, status);
    }
  }
  load(false);
  return { el: root, reload: () => load(false), count: () => total };
}

// ---- clipboard ----
export function copyButton(getText, label = "Copy") {
  const btn = h("button", { type: "button", class: "btn small", "aria-label": label },
    icon("copy", 15), h("span", null, "Copy"));
  btn.addEventListener("click", async () => {
    const ok = await copyText(getText());
    toast(ok ? "Copied to clipboard." : "Couldn't copy. Select the text and copy it manually.", ok ? "ok" : "err");
    if (ok) {
      btn.querySelector("span").textContent = "Copied";
      setTimeout(() => (btn.querySelector("span").textContent = "Copy"), 1800);
    }
  });
  return btn;
}

export function codeBlock(text, label, { copy = true, tall = false } = {}) {
  return h("div", { class: "code" },
    copy ? h("div", { class: "code-bar" }, h("span", null, label), copyButton(() => text, `Copy ${label}`)) : null,
    h("pre", { class: tall ? "tall" : null, tabindex: "0", "aria-label": label }, text));
}

// ---- token bar: this pack vs all candidates ----
export function tokenBar(pack, candidates) {
  if (candidates == null || pack == null) {
    return h("p", { class: "muted" }, "Candidate tokens weren't recorded for this injection.");
  }
  const max = Math.max(pack, candidates, 1);
  const row = (name, v, cls) =>
    h("div", { class: "tb-row" }, h("span", { class: "tb-name" }, name),
      h("span", { class: "tb-track", "aria-hidden": "true" },
        h("span", { class: `tb-fill ${cls}`, css: { "--w": `${(v / max) * 100}%` } })),
      h("span", { class: "num tb-val" }, `${v.toLocaleString("en-US")} tokens`));
  return h("div", { class: "tokenbar" }, row("This pack", pack, "pack"), row("All candidates", candidates, "cand"));
}

// ---- confirmation dialog ----
export function confirmDialog({ title, body, confirm, danger = false }) {
  const opener = document.activeElement;
  return new Promise((resolve) => {
    const dlg = h("dialog", { class: "dlg", "aria-labelledby": "dlg-title" },
      h("form", { method: "dialog" },
        h("h2", { id: "dlg-title" }, title), h("p", null, body),
        h("div", { class: "dlg-actions" },
          h("button", { type: "submit", value: "cancel", class: "btn ghost", autofocus: true }, "Cancel"),
          h("button", { type: "submit", value: "ok", class: `btn ${danger ? "danger" : ""}` }, confirm))));
    dlg.addEventListener("close", () => {
      resolve(dlg.returnValue === "ok");
      dlg.remove();
      opener?.isConnected && opener.focus();
    });
    document.body.append(dlg);
    dlg.showModal();
  });
}

// ---- filter controls ----
export function field(labelText, control, cls = "") {
  return h("label", { class: `field ${cls}` }, h("span", null, labelText), control);
}
export function select(name, options, value = "") {
  return h("select", { name }, options.map(([v, t]) => h("option", { value: v, selected: v === value }, t)));
}
export function checkGroup(name, legend, options, selected) {
  return h("fieldset", { class: "chips" }, h("legend", null, legend),
    options.map(([v, t]) =>
      h("label", { class: "chip" },
        h("input", { type: "checkbox", name, value: v, checked: selected.includes(v) }), h("span", null, t))));
}
