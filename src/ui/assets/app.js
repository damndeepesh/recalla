import { h, icon } from "./dom.js";
import { session, onExpired, appVersion, get } from "./api.js";
import { parseHash, match, go, ALIASES } from "./router.js";
import { emptyState } from "./ui.js";
import { openPalette } from "./palette.js";

const main = document.getElementById("main");
let current = null;
let token = 0;

function teardown() {
  current?.abort.abort();
  for (const fn of current?.cleanups ?? []) fn();
  current = null;
}

/** Views own their first segment; a view with update() handles its own sub-routes. */
async function render(first = false) {
  const { segments, query } = parseHash();
  if (ALIASES[segments[0]]) {
    segments[0] = ALIASES[segments[0]];
    go(segments, query, true);
    return;
  }
  const route = match(segments) ?? { nav: "", load: null };
  if (current?.nav === route.nav && current.view?.update && route.load) {
    current.view.update(segments, query);
    return;
  }
  const my = ++token;
  teardown();
  const abort = new AbortController();
  const cleanups = [];
  current = { nav: route.nav, abort, cleanups, view: null };
  for (const a of document.querySelectorAll("[data-nav]")) {
    if (a.dataset.nav === route.nav) a.setAttribute("aria-current", "page");
    else a.removeAttribute("aria-current");
  }
  const ctx = {
    segments,
    query,
    signal: abort.signal,
    onCleanup: (fn) => cleanups.push(fn),
    title: (t) => (document.title = t ? `${t} · Recalla` : "Recalla"),
    refreshSetupFlag,
  };
  if (!route.load) {
    ctx.title("Not found");
    main.replaceChildren(
      h(
        "div",
        { class: "view" },
        h("h1", { tabindex: "-1" }, "Page not found"),
        emptyState("That page doesn't exist.", null, h("a", { class: "btn", href: "#/" }, "Go to Home")),
      ),
    );
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
    main.replaceChildren(
      h("div", { class: "view" }, h("h1", { tabindex: "-1" }, "Something went wrong"), h("p", null, e.message)),
    );
  }
}

// ---- Setup needs attention? A dot on the nav item. ----
async function refreshSetupFlag() {
  try {
    const c = await get("/api/connect");
    const missing = c.settings_error || c.hooks.some((x) => !x.installed);
    for (const a of document.querySelectorAll('[data-nav="setup"]')) {
      a.querySelector(".nav-flag")?.remove();
      if (missing)
        a.append(
          h(
            "span",
            { class: "nav-flag", title: "Setup needs attention" },
            h("span", { class: "sr" }, " (needs attention)"),
          ),
        );
    }
  } catch {
    // The flag is a hint; Setup itself reports errors.
  }
}

// ---- theme ----
const themeBtn = document.getElementById("theme-btn");
const prefersDark = matchMedia("(prefers-color-scheme: dark)");
const effective = () => document.documentElement.dataset.theme ?? (prefersDark.matches ? "dark" : "light");
function paintThemeButton() {
  const dark = effective() === "dark";
  themeBtn.replaceChildren(icon(dark ? "sun" : "moon", 18));
  const label = `Switch to ${dark ? "light" : "dark"} theme`;
  themeBtn.setAttribute("aria-label", label);
  themeBtn.title = label;
}
export function toggleTheme() {
  const next = effective() === "dark" ? "light" : "dark";
  document.documentElement.dataset.theme = next;
  try {
    localStorage.setItem("recalla-theme", next);
  } catch {
    // Not persisted; applies for this visit.
  }
  paintThemeButton();
}
themeBtn.addEventListener("click", toggleTheme);
prefersDark.addEventListener("change", paintThemeButton);
paintThemeButton();

// ---- nav icons, skip link, search ----
const NAV_KEY = { home: "h", memories: "m", sessions: "s", packs: "p", setup: "c" };
for (const a of document.querySelectorAll("[data-nav]")) {
  a.prepend(icon(a.dataset.nav, 17));
  a.append(h("span", { class: "nav-key", "aria-hidden": "true" }, h("kbd", null, "g"), h("kbd", null, NAV_KEY[a.dataset.nav])));
}
document.getElementById("search-btn").prepend(icon("search", 16));
document.getElementById("search-btn").addEventListener("click", () => openPalette({ toggleTheme }));
document.getElementById("skip").addEventListener("click", (e) => {
  e.preventDefault(); // a hash link would be read as a route
  main.focus();
});

// ---- keyboard ----
const SHORTCUTS = [
  [
    "Anywhere",
    [
      ["/", "Search or jump to"],
      ["Ctrl K", "Search or jump to"],
      ["?", "Show this help"],
    ],
  ],
  [
    "Go to",
    [
      ["g h", "Home"],
      ["g m", "Memories"],
      ["g s", "Sessions"],
      ["g p", "Context packs"],
      ["g c", "Setup"],
    ],
  ],
  [
    "Lists",
    [
      ["j", "Next item"],
      ["k", "Previous item"],
      ["Enter", "Open"],
      ["Esc", "Close the detail pane"],
    ],
  ],
];
const GO = {
  h: [],
  o: [],
  m: ["memories"],
  s: ["sessions"],
  p: ["packs"],
  i: ["packs"],
  c: ["setup"],
};

function showHelp() {
  const dlg = h(
    "dialog",
    { "aria-labelledby": "help-title", closedby: "any" },
    h(
      "form",
      { method: "dialog", class: "dlg" },
      h("h2", { id: "help-title" }, "Keyboard shortcuts"),
      h(
        "dl",
        { class: "keys" },
        SHORTCUTS.map(([group, rows]) => [
          h("h3", null, group),
          ...rows.flatMap(([k, t]) => [
            h(
              "dt",
              null,
              k
                .split(" ")
                .map((p, i) => [
                  i && k.startsWith("g") ? h("span", { class: "muted small" }, "then") : null,
                  h("kbd", null, p),
                ]),
            ),
            h("dd", null, t),
          ]),
        ]),
      ),
      h("div", { class: "dlg-actions" }, h("button", { class: "btn", autofocus: true }, "Close")),
    ),
  );
  dlg.addEventListener("close", () => dlg.remove());
  document.body.append(dlg);
  dlg.showModal();
}
document.getElementById("help-btn").append(icon("help", 18));
document.getElementById("help-btn").addEventListener("click", showHelp);

/** j/k (and arrows when a row has focus) walk the visible list's row links. */
function moveInList(step) {
  const rows = [...document.querySelectorAll("[data-rows] .row-title")].filter((a) => a.offsetParent);
  if (!rows.length) return false;
  const at = rows.indexOf(document.activeElement);
  const sel = rows.findIndex((a) => a.closest(".row")?.getAttribute("aria-current") === "true");
  const from = at >= 0 ? at : sel;
  const next = rows[from < 0 ? 0 : Math.min(rows.length - 1, Math.max(0, from + step))];
  next.focus();
  next.scrollIntoView({ block: "nearest" });
  return true;
}

let pendingG = 0;
document.addEventListener("keydown", (e) => {
  const t = e.target;
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
    e.preventDefault();
    if (!document.querySelector("dialog[open]")) openPalette({ toggleTheme });
    return;
  }
  if (e.ctrlKey || e.metaKey || e.altKey || document.querySelector("dialog[open]")) return;
  if (t.closest?.("input, textarea, select, [contenteditable]")) {
    if (e.key === "Escape" && t.matches("[data-search]")) t.blur();
    return;
  }
  const inRow = t.classList?.contains("row-title");
  if (pendingG && Date.now() - pendingG < 1200 && GO[e.key]) {
    pendingG = 0;
    e.preventDefault();
    go(GO[e.key]);
  } else if (e.key === "g") pendingG = Date.now();
  else if (e.key === "/") {
    e.preventDefault();
    openPalette({ toggleTheme });
  } else if (e.key === "?") {
    e.preventDefault();
    showHelp();
  } else if (e.key === "j" || (inRow && e.key === "ArrowDown")) {
    if (moveInList(1)) e.preventDefault();
  } else if (e.key === "k" || (inRow && e.key === "ArrowUp")) {
    if (moveInList(-1)) e.preventDefault();
  } else if (e.key === "Escape") {
    const close = document.querySelector("[data-close-pane]");
    if (close) {
      e.preventDefault();
      close.click();
    }
  } else pendingG = 0;
});

// ---- boot ----
function sessionEnded() {
  teardown();
  const app = document.querySelector(".app");
  app.classList.add("is-ended");
  app.replaceChildren(
    h(
      "main",
      { class: "ended", id: "main" },
      h("h1", null, "Session ended"),
      h("p", null, "Run ", h("code", null, "recalla ui"), " again to get a new login link."),
      h("p", { class: "muted" }, "The link works once and the session stops when the server does."),
    ),
  );
  document.title = "Session ended · Recalla";
}
onExpired(sessionEnded);
window.addEventListener("hashchange", () => render());

try {
  await session();
  document.getElementById("version").textContent = `Version ${appVersion()}`;
  refreshSetupFlag();
  await render(true);
} catch (e) {
  if (e.status !== 401) {
    main.replaceChildren(h("div", { class: "view" }, h("h1", null, "Can't reach Recalla"), h("p", null, e.message)));
  }
}
