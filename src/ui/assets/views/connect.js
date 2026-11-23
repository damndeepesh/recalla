import { h, fill, icon, num, bytes, time, absTime, savedText } from "../dom.js";
import { get, post } from "../api.js";
import { pageHead, panel, skeleton, errorState, kv, codeBlock, statusLine, tokenBar, kindBadge, field } from "../ui.js";
import { memoryHref } from "../router.js";

const label = (k) => k.replaceAll("_", " ").replace(/^./, (c) => c.toUpperCase());

function claudeCard(c, result, onResult) {
  const status = statusLine();
  const installed = c.hooks.filter((x) => x.installed).length;
  if (result) status.set(result.changed ? "Hooks updated." : "Nothing to change.");
  const act = (which, btns) => async () => {
    btns.forEach((b) => (b.disabled = true));
    status.set(which === "install" ? "Installing hooks…" : "Removing hooks…");
    try {
      await onResult(await post(`/api/hooks/${which}`));
    } catch (e) {
      if (e.status === 401) return;
      btns.forEach((b) => (b.disabled = false));
      status.set(`${which === "install" ? "Install" : "Remove"} failed: ${e.message}`, "err");
    }
  };
  const install = h("button", { type: "button", class: "btn" }, installed ? "Reinstall hooks" : "Install hooks");
  const remove = h("button", { type: "button", class: "btn ghost", disabled: !installed }, "Remove hooks");
  install.addEventListener("click", act("install", [install, remove]));
  remove.addEventListener("click", act("uninstall", [install, remove]));
  return panel("Claude Code", h("div", { class: "stack" },
    h("ul", { class: "checks" }, c.hooks.map((x) => h("li", { class: x.installed ? "ok" : "warn" }, icon(x.installed ? "check" : "alert", 16), h("span", null, `${x.event}: ${x.installed ? "installed" : "not installed"}`)))),
    c.settings_error ? h("p", { class: "status err" }, `Can't read the settings file: ${c.settings_error}`) : null,
    h("p", { class: "muted" }, "Hooks edit ", h("code", null, c.settings_path ?? "~/.claude/settings.json"), " to capture sessions and inject context. A backup is kept next to it before every change."),
    h("div", { class: "actions" }, install, remove), status,
    result ? h("div", { class: "result" }, result.changes.length ? h("ul", { class: "plain" }, result.changes.map((x) => h("li", null, x))) : h("p", null, "No changes were needed."),
      result.backup ? h("p", { class: "muted" }, "Backup: ", h("code", null, result.backup)) : null) : null,
    h("div", null, h("h3", null, "MCP server"), h("p", { class: "muted" }, "Lets Claude search and write memories on demand."),
      codeBlock(c.mcp.claude_code_command, "Claude Code MCP command"))));
}

const codexCard = (c) => panel("Codex", h("div", { class: "stack" },
  h("p", { class: "muted" }, "Add this to ", h("code", null, "~/.codex/config.toml"), " to give Codex the Recalla MCP tools."),
  codeBlock(c.mcp.codex_toml, "Codex config snippet")));

const dataCard = (c) => panel("Data", h("div", { class: "stack" },
  kv([["Binary", h("code", null, c.binary_path)], ["Data directory", h("code", null, c.data_dir)], ["Markdown mirror", h("code", null, c.mirror_dir)],
    ["Database", c.db.exists ? `${bytes(c.db.bytes)}, schema ${c.db.schema_version} (expected ${c.db.expected_schema_version})` : "Not created yet"],
    ["Last capture", c.last_capture_at ? [time(c.last_capture_at), ` (${absTime(c.last_capture_at)})`] : "Nothing captured yet"],
    ["Last injection", c.last_injection ? [time(c.last_injection.at), ` via ${c.last_injection.via}`] : "None yet"]], "stacked"),
  h("div", null, h("h3", null, "Hook log"),
    c.hook_log_tail.length ? h("pre", { class: "log", tabindex: "0", "aria-label": "Hook log, last lines" }, c.hook_log_tail.join("\n")) : h("p", { class: "muted" }, "No hook activity logged."))));

function testRecall() {
  const out = h("div", { class: "stack", "aria-live": "polite" });
  const status = statusLine();
  const list = h("datalist", { id: "test-recall-projects" });
  get("/api/projects").then((r) => list.replaceChildren(...r.items.map((p) =>
    h("option", { value: p.project }, `${num(p.memory_count)} memories`)))).catch(() => {});
  const form = h("form", { class: "filters" },
    field("Project", h("input", { name: "project", list: "test-recall-projects", placeholder: "Pick or type a project path (empty = global only)", autocomplete: "off" }), "grow"),
    list,
    field("Query", h("input", { name: "query", placeholder: "What is the agent working on? (optional)", autocomplete: "off" }), "grow"),
    field("Budget (tokens)", h("input", { name: "budget", type: "number", min: "100", max: "8000", step: "100", value: "1500" })),
    h("button", { class: "btn", type: "submit" }, "Run test"));
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
        out.replaceChildren(h("p", { class: "muted" }, "No memories matched. Nothing would be injected for that project and query."));
      } else {
        fill(out,
          h("p", null, h("strong", { class: "num" }, `${num(r.est_tokens)} tokens`), ` from ${num(r.memories.length)} memories. Versus all candidates: ${savedText(r.saved_pct)}.`),
          tokenBar(r.est_tokens, r.candidate_tokens),
          codeBlock(r.markdown, "test pack", { tall: true }),
          h("ul", { class: "plain" }, r.memories.map((m) => h("li", null, kindBadge(m.kind), " ", h("a", { href: memoryHref(m.uid) }, m.text)))));
      }
    } catch (err) {
      if (err.status === 401) return;
      out.replaceChildren();
      status.set(`Test failed: ${err.message}`, "err");
    }
    btn.disabled = false;
  });
  return panel("Test recall", h("div", { class: "stack" },
    h("p", { class: "muted" }, "Builds a real pack without recording an injection, so you can see what an agent working in that project would get."), form, status, out));
}

function flatten(obj, prefix = "") {
  return Object.entries(obj ?? {}).flatMap(([k, v]) => {
    const name = prefix ? `${prefix} ${k}` : k;
    if (typeof v === "number") return [[label(name), num(v)]];
    if (v && typeof v === "object" && !Array.isArray(v)) return flatten(v, name);
    return [];
  });
}

const pick = (obj, name) => flatten(obj, "").find(([k]) => k === name)?.[1];

function summary(r) {
  const all = [...flatten(r?.ingest, "ingest"), ...flatten(r?.extract, "extract")];
  const total = (re) => all.filter(([k]) => re.test(k)).reduce((n, [, v]) => n + Number(v.replaceAll(",", "")), 0);
  const head = kv([
    ["Files read", pick(r?.ingest, "Files seen")], ["New files", pick(r?.ingest, "Files new")],
    ["Events added", pick(r?.ingest, "Events added")],
    ["Memories added", num(total(/^Extract memories new/))],
    ["Secrets redacted", num(total(/^Ingest redactions by rule/))]]);
  return h("div", { class: "stack" }, head,
    all.length ? h("details", { class: "more" }, h("summary", null, "All counts"), kv(all)) : null);
}

function importHistory(ctx, onDone) {
  const status = statusLine();
  const out = h("div");
  const btn = h("button", { type: "button", class: "btn" }, "Import history");
  let timer = 0;
  const bar = h("div", { class: "indeterminate", hidden: true, "aria-hidden": "true" }, h("span"));

  function show(st) {
    if (st.state === "running") {
      btn.disabled = true;
      bar.hidden = false;
      status.set(`Importing… ${Math.max(0, Math.round((Date.now() - (st.started_at ?? Date.now())) / 1000))}s`);
      return;
    }
    btn.disabled = false;
    bar.hidden = true;
    if (st.state === "failed") status.set(`Import failed: ${st.error ?? "unknown error"}`, "err");
    else if (st.state === "done") {
      status.set("Import finished.");
      fill(out, summary(st.report), st.report?.mirror ? h("p", { class: "muted" }, "Mirror: ", h("code", null, st.report.mirror)) : null);
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
  // Resume if a job is already running (e.g. after navigating away and back).
  get("/api/sync/status", null, ctx.signal).then((st) => { if (st.state === "running") poll(); }).catch(() => {});
  return panel("Import history", h("div", { class: "stack" },
    h("p", { class: "muted" }, "Reads your existing Claude Code and Codex sessions, extracts memories and refreshes the mirror. Safe to run again: unchanged files are skipped."),
    h("div", { class: "actions" }, btn), bar, status, out));
}

export function mount(ctx) {
  ctx.title("Connect");
  const cards = h("div", { class: "stack" }, skeleton(5));
  const el = h("div", { class: "view" }, pageHead("Connect", "Link agents to Recalla and check that everything is wired up."), cards);
  let hookResult = null;
  async function load() {
    try {
      const c = await get("/api/connect", null, ctx.signal);
      const keep = el.querySelector("[data-keep]");
      cards.replaceChildren(
        h("div", { class: "cards3" }, claudeCard(c, hookResult, async (r) => { hookResult = r; await load(); }), codexCard(c), dataCard(c)),
        keep ?? h("div", { class: "cols", "data-keep": true }, testRecall(), importHistory(ctx, () => load())));
    } catch (e) {
      if (e.name === "AbortError" || e.status === 401) return;
      cards.replaceChildren(errorState(e, () => load()));
    }
  }
  load();
  return { el };
}
