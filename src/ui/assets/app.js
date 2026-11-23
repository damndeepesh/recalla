import { h, icon } from "./dom.js";
import { session, onExpired, appVersion } from "./api.js";
import { parseHash, match, go } from "./router.js";
import { emptyState } from "./ui.js";

const main = document.getElementById("main");
let current = null;
let token = 0;

function teardown() {
  current?.abort.abort();
  for (const fn of current?.cleanups ?? []) fn();
  current = null;
}

async function render(first = false) {
  const { segments, query } = parseHash();
  const route = match(segments) ?? { nav: "", load: null };
  const key = segments.join("/");
  if (current?.key === key && current.view?.update) {
    current.view.update(query);
    return;
  }
  const my = ++token;
  teardown();
  const abort = new AbortController();
  const cleanups = [];
  current = { key, abort, cleanups, view: null };
  for (const a of document.querySelectorAll("[data-nav]")) {
    if (a.dataset.nav === route.nav) a.setAttribute("aria-current", "page");
    else a.removeAttribute("aria-current");
  }
  const ctx = {
    segments, query, signal: abort.signal,
    onCleanup: (fn) => cleanups.push(fn),
    title: (t) => (document.title = t ? `${t} · Recalla` : "Recalla"),
  };
  if (!route.load) {
    ctx.title("Not found");
    main.replaceChildren(h("h1", { tabindex: "-1" }, "Page not found"),
      emptyState("That page doesn't exist.", null, h("a", { class: "btn", href: "#/" }, "Back to overview")));
    return;
  }
  try {
    const mod = await route.load();
    if (my !== token) return;
    const view = mod.mount(ctx);
    current.view = view;
    main.replaceChildren(view.el);
    window.scrollTo(0, 0);
    if (!first) main.querySelector("h1")?.focus({ preventScroll: true });
  } catch (e) {
    if (my !== token) return;
    main.replaceChildren(h("h1", { tabindex: "-1" }, "Something went wrong"), h("p", null, e.message));
  }
}

// ---- theme ----
const themeBtn = document.getElementById("theme-btn");
const prefersDark = matchMedia("(prefers-color-scheme: dark)");
const effective = () => document.documentElement.dataset.theme ?? (prefersDark.matches ? "dark" : "light");
function paintThemeButton() {
  const dark = effective() === "dark";
  themeBtn.replaceChildren(icon(dark ? "sun" : "moon", 20));
  const label = `Switch to ${dark ? "light" : "dark"} theme`;
  themeBtn.setAttribute("aria-label", label);
  themeBtn.title = label;
}
themeBtn.addEventListener("click", () => {
  const next = effective() === "dark" ? "light" : "dark";
  document.documentElement.dataset.theme = next;
  try {
    localStorage.setItem("recalla-theme", next);
  } catch {
    // Not persisted; applies for this visit.
  }
  paintThemeButton();
});
prefersDark.addEventListener("change", paintThemeButton);
paintThemeButton();

// ---- nav icons, skip link ----
for (const a of document.querySelectorAll("[data-nav]")) a.prepend(icon(a.dataset.nav, 16));
document.getElementById("skip").addEventListener("click", (e) => {
  e.preventDefault(); // a hash link would be read as a route
  main.focus();
});

// ---- keyboard shortcuts ----
const SHORTCUTS = [["/", "Focus search"], ["?", "Show this help"], ["g o", "Go to Overview"], ["g s", "Go to Sessions"],
  ["g m", "Go to Memories"], ["g i", "Go to What Claude got"], ["g c", "Go to Connect"]];
const GO = { o: [], s: ["sessions"], m: ["memories"], i: ["injections"], c: ["connect"] };

function showHelp() {
  const opener = document.activeElement;
  const dlg = h("dialog", { class: "dlg", "aria-labelledby": "help-title" },
    h("form", { method: "dialog" }, h("h2", { id: "help-title" }, "Keyboard shortcuts"),
      h("dl", { class: "kv keys" }, SHORTCUTS.flatMap(([k, t]) => [
        h("dt", null, k.split(" ").map((p, i) => [i ? " then " : "", h("kbd", null, p)])), h("dd", null, t)])),
      h("div", { class: "dlg-actions" }, h("button", { class: "btn", autofocus: true }, "Close"))));
  dlg.addEventListener("close", () => { dlg.remove(); opener?.isConnected && opener.focus(); });
  document.body.append(dlg);
  dlg.showModal();
}
document.getElementById("help-btn").append(icon("help", 20));
document.getElementById("help-btn").addEventListener("click", showHelp);

let pendingG = 0;
document.addEventListener("keydown", (e) => {
  const t = e.target;
  if (e.ctrlKey || e.metaKey || e.altKey || document.querySelector("dialog[open]")) return;
  if (t.closest?.("input, textarea, select, [contenteditable]")) return;
  if (pendingG && Date.now() - pendingG < 1200 && GO[e.key]) {
    pendingG = 0;
    e.preventDefault();
    go(GO[e.key]);
  } else if (e.key === "g") pendingG = Date.now();
  else if (e.key === "/") {
    const s = document.querySelector("[data-search]");
    if (s) { e.preventDefault(); s.focus(); s.select?.(); }
  } else if (e.key === "?") { e.preventDefault(); showHelp(); }
  else pendingG = 0;
});

// ---- boot ----
function sessionEnded() {
  teardown();
  const app = document.querySelector(".app");
  app.classList.add("is-ended");
  app.replaceChildren(
    h("main", { class: "ended", id: "main" },
      h("h1", null, "Session ended"),
      h("p", null, "Run ", h("code", null, "recalla ui"), " again to get a new login link."),
      h("p", { class: "muted" }, "The link works once and the session stops when the server does.")));
  document.title = "Session ended · Recalla";
}
onExpired(sessionEnded);
window.addEventListener("hashchange", () => render());

try {
  await session();
  document.getElementById("version").textContent = `Recalla ${appVersion()}`;
  await render(true);
} catch (e) {
  if (e.status !== 401) {
    main.replaceChildren(h("h1", null, "Can't reach Recalla"), h("p", null, e.message));
  }
}
