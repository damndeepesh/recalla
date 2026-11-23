// Command bar ("/" or Ctrl/Cmd-K): jump to pages, projects, memories and
// sessions. ARIA combobox + listbox; arrows move, Enter opens, Esc closes.
import { h, icon, baseName, rel } from "./dom.js";
import { get } from "./api.js";
import { href } from "./router.js";
import { projects, KIND_LABEL, AGENT_LABEL } from "./ui.js";

const PAGES = [
  ["Home", "home", ["home"], ""],
  ["Memories", "memories", ["memories"], ""],
  ["Sessions", "sessions", ["sessions"], ""],
  ["Context packs", "packs", ["packs"], "What each agent was given"],
  ["Setup", "setup", ["setup"], ""],
  ["Memories that need review", "alert", ["memories"], "", { status: "needs_reverify" }],
  ["Install Claude Code hooks", "plug", ["setup"], "Setup", { step: "hooks" }],
  ["Import history", "download", ["setup"], "Setup", { step: "import" }],
  ["Test recall", "play", ["setup"], "Setup", { step: "test" }],
];

let open = false;

export function openPalette({ toggleTheme }) {
  if (open) return;
  open = true;
  const input = h("input", {
    type: "text",
    role: "combobox",
    "aria-expanded": "true",
    "aria-controls": "pal-list",
    "aria-autocomplete": "list",
    "aria-label": "Search or jump to",
    placeholder: "Search memories, sessions, projects or pages",
    autocomplete: "off",
    spellcheck: "false",
  });
  const list = h("ul", {
    class: "pal-list",
    id: "pal-list",
    role: "listbox",
    "aria-label": "Results",
  });
  const live = h("p", { class: "sr", role: "status", "aria-live": "polite" });
  const dlg = h(
    "dialog",
    { class: "palette", "aria-label": "Search or jump to", closedby: "any" },
    h("div", { class: "pal-input" }, icon("search", 18), input),
    list,
    live,
    h(
      "div",
      { class: "pal-foot", "aria-hidden": "true" },
      h("span", null, h("kbd", null, "↑"), " ", h("kbd", null, "↓"), " move"),
      h("span", null, h("kbd", null, "Enter"), " open"),
      h("span", null, h("kbd", null, "Esc"), " close"),
    ),
  );

  let items = [];
  let active = 0;
  let gen = 0;
  let timer = 0;
  const ac = new AbortController();

  const extra = [
    {
      group: "Actions",
      title: "Switch light or dark theme",
      ic: "moon",
      run: () => toggleTheme(),
    },
  ];

  function paint() {
    list.replaceChildren();
    let group = "";
    items.forEach((it, i) => {
      if (it.group !== group) {
        group = it.group;
        list.append(h("li", { class: "pal-group", role: "presentation" }, group));
      }
      const opt = h(
        "li",
        {
          class: "pal-opt",
          role: "option",
          id: `pal-${i}`,
          "aria-selected": String(i === active),
        },
        icon(it.ic, 16),
        h("span", { class: "t" }, it.title),
        it.meta ? h("span", { class: "m" }, it.meta) : null,
      );
      opt.addEventListener("click", () => choose(i));
      opt.addEventListener("mousemove", () => {
        if (active !== i) setActive(i);
      });
      list.append(opt);
    });
    if (!items.length)
      list.append(h("li", { class: "pal-empty", role: "presentation" }, "Nothing matches. Try fewer words."));
    input.setAttribute("aria-activedescendant", items.length ? `pal-${active}` : "");
  }
  function setActive(i) {
    active = i;
    for (const o of list.querySelectorAll("[role=option]"))
      o.setAttribute("aria-selected", String(o.id === `pal-${i}`));
    input.setAttribute("aria-activedescendant", `pal-${i}`);
    document.getElementById(`pal-${i}`)?.scrollIntoView({ block: "nearest" });
  }
  function choose(i) {
    const it = items[i];
    if (!it) return;
    dlg.close();
    if (it.run) it.run();
    else location.hash = it.href;
  }

  const matches = (q, s) => s.toLowerCase().includes(q);
  async function search() {
    const q = input.value.trim();
    const ql = q.toLowerCase();
    const my = ++gen;
    const pages = PAGES.filter(([t, , , m]) => !ql || matches(ql, t) || matches(ql, m)).map(
      ([t, ic, seg, meta, query]) => ({
        group: "Go to",
        title: t,
        ic,
        meta,
        href: href(seg, query && new URLSearchParams(query)),
      }),
    );
    const base = [...pages, ...extra.filter((x) => !ql || matches(ql, x.title))];
    let found = [];
    try {
      const projs = (await projects(get))
        .filter((p) => !ql || matches(ql, p.project))
        .slice(0, ql ? 5 : 4)
        .map((p) => ({
          group: "Projects",
          title: baseName(p.project),
          ic: "folder",
          meta: `${p.memory_count} memories`,
          href: href(["memories"], new URLSearchParams({ project: p.project })),
        }));
      found = projs;
      if (q.length >= 2) {
        const [mem, ses] = await Promise.all([
          get("/api/memories", { q, limit: 6 }, ac.signal),
          get("/api/sessions", { q, limit: 4 }, ac.signal),
        ]);
        if (my !== gen) return;
        found = [
          {
            group: "Memories",
            title: `Search all memories for “${q}”`,
            ic: "search",
            href: href(["memories"], new URLSearchParams({ q })),
          },
          ...mem.items.map((m) => ({
            group: "Memories",
            title: m.text,
            ic: "memories",
            meta: KIND_LABEL[m.kind],
            href: href(["memories", m.uid]),
          })),
          ...ses.items.map((s) => ({
            group: "Sessions",
            title: s.project ? baseName(s.project) : "No project",
            ic: "sessions",
            meta: `${AGENT_LABEL[s.harness] ?? s.harness}, ${rel(s.last_ts)}`,
            href: href(["sessions", s.harness, s.session_id]),
          })),
          ...projs,
        ];
      }
    } catch (e) {
      if (e.name === "AbortError") return;
      found = [
        {
          group: "Search",
          title: `Search failed: ${e.message}`,
          ic: "alert",
          run: () => {},
        },
      ];
    }
    if (my !== gen) return;
    items = ql ? [...found, ...base] : [...base, ...found];
    active = 0;
    paint();
    live.textContent = `${items.length} results`;
  }

  input.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(search, 140);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (items.length) setActive((active + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length);
    } else if (e.key === "Enter") {
      e.preventDefault();
      choose(active);
    } else if (e.key === "Home" && items.length) setActive(0);
    else if (e.key === "End" && items.length) setActive(items.length - 1);
  });
  dlg.addEventListener("close", () => {
    open = false;
    clearTimeout(timer);
    ac.abort();
    dlg.remove();
  });
  document.body.append(dlg);
  dlg.showModal();
  input.focus();
  search();
}
