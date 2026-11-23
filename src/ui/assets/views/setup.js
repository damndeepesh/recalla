// Setup: a short checklist. Each step shows whether it's done and expands to
// the one thing to do. Test recall shows the result first.
import { h, icon, num, bytes, time, absTime, baseName } from "../dom.js";
import { get, post } from "../api.js";
import { memoryHref } from "../router.js";
import {
  errorState,
  loading,
  facts,
  flag,
  kindDot,
  pathChip,
  statusLine,
  codeBlock,
  ruler,
  field,
  select,
  projects,
  resetProjects,
  section,
  toast,
  savedWords,
} from "../ui.js";

const HOOK_HELP = {
  SessionStart: "gives each new session a context pack",
  Stop: "saves the session when it ends",
  PreCompact: "saves it before Claude Code compacts the conversation",
};

function step(key, n, title, sub) {
  const mark = h("span", { class: "step-mark", "aria-hidden": "true" }, String(n));
  const state = h("span", { class: "flag" });
  const small = h("small", null, sub);
  const body = h("div", { class: "step-body" });
  const det = h(
    "details",
    { name: "setup-step" },
    h("summary", null, mark, h("span", { class: "step-title" }, title, small), state, icon("chev", 16)),
    body,
  );
  const li = h("li", { class: "step", id: `step-${key}` }, det);
  li.set = (tone, label, text) => {
    li.className = `step ${tone}`;
    mark.replaceChildren(tone === "done" ? icon("check", 14) : tone === "bad" ? icon("x", 14) : String(n));
    state.className = `flag ${tone === "done" ? "ok" : tone === "todo" ? "attn" : tone === "bad" ? "err" : ""}`;
    state.textContent = label;
    if (text) small.textContent = text;
  };
  li.body = body;
  li.det = det;
  return li;
}

// ---- 1. hooks ----
function hooksBody(c, result, onResult) {
  const status = statusLine();
  const installed = c.hooks.filter((x) => x.installed).length;
  const act = (which, btns) => async () => {
    btns.forEach((b) => (b.disabled = true));
    status.set(which === "install" ? "Installing hooks…" : "Removing hooks…");
    try {
      const r = await post(`/api/hooks/${which}`);
      toast(which === "install" ? "Hooks installed." : "Hooks removed.");
      await onResult(r);
    } catch (e) {
      if (e.status === 401) return;
      btns.forEach((b) => (b.disabled = false));
      status.set(`${which === "install" ? "Install" : "Remove"} failed: ${e.message}`, "err");
    }
  };
  const install = h(
    "button",
    {
      type: "button",
      class: `btn ${installed < c.hooks.length ? "attn" : ""}`,
    },
    installed === c.hooks.length ? "Reinstall hooks" : "Install hooks",
  );
  const remove = h("button", { type: "button", class: "btn ghost", disabled: !installed }, "Remove hooks");
  install.addEventListener("click", act("install", [install, remove]));
  remove.addEventListener("click", act("uninstall", [install, remove]));
  return [
    h(
      "p",
      null,
      "Three small hooks in your Claude Code settings let Recalla capture sessions and hand each new one the context it needs. They never block Claude Code; problems go to the hook log.",
    ),
    c.settings_error
      ? h("p", { class: "notice" }, icon("alert", 16), `Can't read the settings file: ${c.settings_error}`)
      : null,
    h(
      "ul",
      { class: "plain" },
      c.hooks.map((x) =>
        h(
          "li",
          { class: "cluster" },
          x.installed ? flag("Installed", "ok") : flag("Not installed", "attn"),
          h("strong", null, x.event),
          h("span", { class: "muted" }, HOOK_HELP[x.event] ?? ""),
        ),
      ),
    ),
    h(
      "div",
      { class: "cluster small" },
      h("span", { class: "muted" }, "Settings file"),
      pathChip(c.settings_path ?? "~/.claude/settings.json", "settings path"),
      h("span", { class: "muted" }, "A backup is saved next to it before every change."),
    ),
    h("div", { class: "cluster" }, install, remove),
    status,
    result
      ? h(
          "div",
          { class: "card pad stack small" },
          result.changes.length
            ? h(
                "ul",
                { class: "plain" },
                result.changes.map((x) => h("li", null, x)),
              )
            : h("p", null, "Nothing needed changing."),
          result.backup
            ? h(
                "p",
                { class: "cluster" },
                h("span", { class: "muted" }, "Backup of your previous settings:"),
                h("code", null, result.backup),
              )
            : null,
        )
      : null,
  ];
}

// ---- 2. MCP ----
const mcpBody = (c) => [
  h(
    "p",
    null,
    "Optional. With the MCP server an agent can search memories mid-session and save new ones. Run this once in a terminal:",
  ),
  codeBlock(c.mcp.claude_code_command, "Claude Code command"),
  h("p", null, "For Codex, add this to ", h("code", null, "~/.codex/config.toml"), ":"),
  codeBlock(c.mcp.codex_toml, "Codex config"),
];

// ---- 3. import ----
function flatten(obj, prefix = "") {
  return Object.entries(obj ?? {}).flatMap(([k, v]) => {
    const name = prefix ? `${prefix} ${k}` : k;
    if (typeof v === "number") return [[name.replaceAll("_", " "), v]];
    if (v && typeof v === "object" && !Array.isArray(v)) return flatten(v, name);
    return [];
  });
}

function importBody(ctx, onDone) {
  const status = statusLine();
  const out = h("div");
  const btn = h("button", { type: "button", class: "btn primary" }, icon("download", 16), "Import history");
  const bar = h("div", { class: "progress", hidden: true, "aria-hidden": "true" }, h("span"));
  let timer = 0;
  const sum = (obj) => Object.values(obj ?? {}).reduce((n, v) => n + (typeof v === "number" ? v : 0), 0);

  function show(st) {
    if (st.state === "running") {
      btn.disabled = true;
      bar.hidden = false;
      status.set(`Importing… ${Math.max(0, Math.round((Date.now() - (st.started_at ?? Date.now())) / 1000))} s`);
      return;
    }
    btn.disabled = false;
    bar.hidden = true;
    if (st.state === "failed") status.set(`Import failed: ${st.error ?? "unknown error"}`, "err");
    else if (st.state === "done") {
      const r = st.report ?? {};
      const files = r.ingest?.files ?? {};
      status.set("Import finished.", "ok");
      out.replaceChildren(
        h(
          "div",
          { class: "stack" },
          facts([
            ["Files read", num(files.seen)],
            ["New or changed", num((files.new ?? 0) + (files.resumed ?? 0) + (files.restarted ?? 0))],
            ["Events added", num(r.ingest?.events_added)],
            ["New memories", num(sum(r.extract?.memories_new))],
            ["Secrets redacted", num(sum(r.ingest?.redactions_by_rule))],
          ]),
          h(
            "details",
            null,
            h("summary", { class: "link-btn" }, "All counts"),
            facts(flatten(r, "").map(([k, v]) => [k, num(v)])),
          ),
          r.mirror
            ? h(
                "p",
                { class: "cluster small" },
                h("span", { class: "muted" }, "Markdown mirror"),
                pathChip(r.mirror, "mirror path"),
              )
            : null,
        ),
      );
    }
  }
  async function poll() {
    try {
      const st = await get("/api/sync/status", null, ctx.signal);
      show(st);
      if (st.state === "running") timer = setTimeout(poll, 1000);
      else if (st.state === "done") onDone();
    } catch (e) {
      if (e.name === "AbortError" || e.status === 401) return;
      btn.disabled = false;
      bar.hidden = true;
      status.set(`Can't read import progress: ${e.message}`, "err");
    }
  }
  ctx.onCleanup(() => clearTimeout(timer));
  btn.addEventListener("click", async () => {
    btn.disabled = true;
    out.replaceChildren();
    status.set("Starting import…");
    try {
      await post("/api/sync");
    } catch (e) {
      if (e.status === 401) return;
      if (e.status !== 409) {
        btn.disabled = false;
        status.set(`Couldn't start the import: ${e.message}`, "err");
        return;
      }
    }
    poll();
  });
  get("/api/sync/status", null, ctx.signal)
    .then((st) => {
      if (st.state === "running") poll();
    })
    .catch(() => {});
  return [
    h(
      "p",
      null,
      "Reads the Claude Code and Codex sessions already on this machine, redacts secrets, and extracts memories. Safe to run again: unchanged files are skipped.",
    ),
    h("div", { class: "cluster" }, btn),
    bar,
    status,
    out,
  ];
}

// ---- 4. test recall ----
const item = (m) => h("li", null, kindDot(m.kind), h("a", { href: memoryHref(m.uid) }, m.text));

function testBody() {
  const out = h("div", { class: "stack", "aria-live": "polite" });
  const status = statusLine();
  const proj = select("project", [["", "No project (global memories only)"]], "", null);
  projects(get)
    .then((items) => {
      proj.replaceChildren(
        h("option", { value: "" }, "No project (global memories only)"),
        ...items.map((p, i) =>
          h(
            "option",
            { value: p.project, selected: i === 0, title: p.project },
            `${baseName(p.project)} (${num(p.memory_count)})`,
          ),
        ),
      );
    })
    .catch(() => {});
  const form = h(
    "form",
    { class: "recall-form" },
    field("Project", proj),
    field(
      "Working on",
      h("input", {
        name: "query",
        class: "input",
        placeholder: "e.g. rate limits",
        autocomplete: "off",
      }),
      "(optional)",
    ),
    field(
      "Budget",
      h("input", {
        name: "budget",
        class: "input",
        type: "number",
        min: "100",
        max: "8000",
        step: "100",
        value: "1500",
        inputmode: "numeric",
      }),
    ),
    h("button", { class: "btn primary", type: "submit" }, icon("play", 15), "Build pack"),
  );
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    const fd = Object.fromEntries(new FormData(form));
    const body = {};
    if (fd.project) body.project = fd.project;
    if (fd.query) body.query = fd.query;
    if (fd.budget) body.budget = Number(fd.budget);
    const btn = form.querySelector("button");
    btn.disabled = true;
    status.set("Building pack…");
    try {
      const r = await post("/api/test-recall", body);
      status.clear();
      if (!r.memories.length) {
        out.replaceChildren(
          h(
            "p",
            { class: "muted" },
            "No memories matched, so an agent in that project would get nothing. Pick another project or import more history.",
          ),
        );
      } else {
        out.replaceChildren(
          h(
            "p",
            null,
            h("strong", { class: "num" }, `${num(r.est_tokens)} tokens`),
            ` from ${num(r.memories.length)} of ${num(r.candidate_count)} candidate memories`,
            r.saved_pct == null ? "." : `, ${savedWords(r.saved_pct)} than sending them all.`,
          ),
          ruler(r.est_tokens, r.candidate_tokens),
          h("ul", { class: "side-list recall-list" }, r.memories.slice(0, 6).map(item)),
          r.memories.length > 6
            ? h("details", null, h("summary", { class: "link-btn" }, `Show all ${num(r.memories.length)} memories`),
                h("ul", { class: "side-list recall-list" }, r.memories.slice(6).map(item)))
            : null,
          h("details", null, h("summary", { class: "link-btn" }, "Show the pack text"), codeBlock(r.markdown, "test pack", { tall: true })),
        );
      }
    } catch (err) {
      if (err.status === 401) return;
      out.replaceChildren();
      status.set(`Test failed: ${err.message}`, "err");
    }
    btn.disabled = false;
  });
  return [
    h("p", null, "See exactly what an agent would receive, without recording it as a real pack."),
    form,
    status,
    out,
  ];
}

function dataCard(c, o) {
  return section(
    "Where your data lives",
    h(
      "div",
      { class: "card pad stack" },
      facts([
        ["Recalla binary", pathChip(c.binary_path, "binary path")],
        ["Data folder", pathChip(c.data_dir, "data folder")],
        ["Markdown mirror", pathChip(c.mirror_dir, "mirror folder")],
        [
          "Database",
          c.db.exists
            ? `${bytes(c.db.bytes)}, schema ${c.db.schema_version} of ${c.db.expected_schema_version}`
            : "Not created yet",
        ],
        ["Session archive", o ? bytes(o.archive_bytes) : null],
        [
          "Last capture",
          c.last_capture_at
            ? h(
                "span",
                null,
                time(c.last_capture_at),
                h("span", { class: "muted" }, ` (${absTime(c.last_capture_at)})`),
              )
            : "Nothing captured yet",
        ],
        [
          "Last context pack",
          c.last_injection ? h("span", null, time(c.last_injection.at), ` via ${c.last_injection.via}`) : "None yet",
        ],
      ]),
      h(
        "details",
        null,
        h("summary", { class: "link-btn" }, `Hook log (${c.hook_log_tail.length ? "last lines" : "empty"})`),
        c.hook_log_tail.length
          ? h(
              "pre",
              {
                class: "code log",
                tabindex: "0",
                "aria-label": "Hook log, last lines",
              },
              c.hook_log_tail.join("\n"),
            )
          : h("p", { class: "muted small" }, "No hook activity logged."),
      ),
    ),
  );
}

export function mount(ctx) {
  ctx.title("Setup");
  const content = h("div", null, loading("Loading setup"));
  const el = h(
    "div",
    { class: "view" },
    h(
      "header",
      { class: "page-head" },
      h(
        "div",
        null,
        h("h1", { tabindex: "-1" }, "Setup"),
        h("p", { class: "sub" }, "Four steps to connect your agents. Everything stays on this machine."),
      ),
    ),
    content,
  );
  const steps = {
    hooks: step("hooks", 1, "Connect Claude Code", "Install three hooks"),
    mcp: step("mcp", 2, "Let agents search memories", "Add the MCP server"),
    import: step("import", 3, "Import your history", "Read past sessions"),
    test: step("test", 4, "Test recall", "Preview what an agent would get"),
  };
  steps.mcp.set("", "Optional");
  steps.test.set("", "Try it");
  let hookResult = null;
  let built = false;

  async function load() {
    try {
      const [c, o] = await Promise.all([get("/api/connect", null, ctx.signal), get("/api/overview", null, ctx.signal)]);
      const n = c.hooks.filter((x) => x.installed).length;
      if (c.settings_error) steps.hooks.set("bad", "Problem", "Settings file can't be read");
      else
        steps.hooks.set(
          n === c.hooks.length ? "done" : "todo",
          n === c.hooks.length ? "Done" : "To do",
          `${n} of ${c.hooks.length} hooks installed`,
        );
      steps.hooks.body.replaceChildren(
        ...hooksBody(c, hookResult, async (r) => {
          hookResult = r;
          ctx.refreshSetupFlag();
          await load();
        }).filter(Boolean),
      );
      steps.import.set(
        o.sessions ? "done" : "todo",
        o.sessions ? "Done" : "To do",
        o.sessions ? `${num(o.sessions)} sessions, ${num(o.memories.total)} memories` : "Nothing imported yet",
      );
      if (!built) {
        built = true;
        steps.mcp.body.replaceChildren(...mcpBody(c));
        steps.import.body.replaceChildren(
          ...importBody(ctx, () => {
            resetProjects();
            load();
          }),
        );
        steps.test.body.replaceChildren(...testBody());
        const want = ctx.query.get("step");
        const first =
          want && steps[want] ? steps[want] : Object.values(steps).find((s) => s.classList.contains("todo"));
        if (first) first.det.open = true;
        content.replaceChildren(
          h("ol", { class: "steps", "aria-label": "Setup steps" }, Object.values(steps)),
          h("div", { class: "foot-note" }, dataCard(c, o)),
        );
        if (want && steps[want]) queueMicrotask(() => steps[want].scrollIntoView({ block: "start" }));
      } else content.lastChild.replaceChildren(dataCard(c, o));
    } catch (e) {
      if (e.name === "AbortError" || e.status === 401) return;
      content.replaceChildren(errorState(e, () => load()));
    }
  }
  load();
  return { el };
}
