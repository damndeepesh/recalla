// Home: answers "is Recalla working?" first, then what needs a look.
import { h, icon, num, rel, time, baseName } from "../dom.js";
import { get, post } from "../api.js";
import { href } from "../router.js";
import { section, errorState, memoryRow, packRow, toast, loading, KINDS, KIND_PLURAL, savedWords } from "../ui.js";

const POLL_MS = 5000;
const MAX_BACKOFF_MS = 60000;

/** The one status sentence, its explanation and the single next action. */
function status(o, c) {
  const hooksOk = c.hooks.every((x) => x.installed);
  const schemaOk = c.db.exists && c.db.schema_version === c.db.expected_schema_version;
  if (c.settings_error) {
    return {
      tone: "err",
      mark: "alert",
      title: "Recalla can't read your Claude Code settings",
      body: `${c.settings_error}. Hooks can't be checked or installed until the file is valid JSON.`,
      action: ["Open Setup", href(["setup"], new URLSearchParams({ step: "hooks" }))],
    };
  }
  if (!schemaOk) {
    return {
      tone: "err",
      mark: "alert",
      title: "The database needs attention",
      body: c.db.exists
        ? `Schema ${c.db.schema_version ?? "unknown"} found, ${c.db.expected_schema_version} expected. Restart recalla ui to upgrade it.`
        : "No database yet. Restart recalla ui to create it.",
      action: ["Open Setup", "#/setup"],
    };
  }
  if (!o.sessions && !o.memories.total) {
    return {
      tone: "fresh",
      mark: "download",
      title: "Welcome. Import your history to get started",
      body: "Recalla reads the Claude Code and Codex sessions already on this machine and turns them into memories your agents get at the start of each session. Nothing leaves this computer.",
      action: ["Import history", href(["setup"], new URLSearchParams({ step: "import" }))],
      second: hooksOk ? null : ["Install hooks first", href(["setup"], new URLSearchParams({ step: "hooks" }))],
    };
  }
  if (!hooksOk) {
    const missing = c.hooks.filter((x) => !x.installed).map((x) => x.event);
    return {
      tone: "attn",
      mark: "plug",
      title: "Almost there: Claude Code isn't connected yet",
      body: `Recalla knows ${num(o.memories.total)} memories, but ${missing.join(", ")} ${missing.length === 1 ? "isn't" : "aren't"} installed, so new sessions aren't captured and agents don't get context automatically.`,
      install: true,
      second: ["What does this change?", href(["setup"], new URLSearchParams({ step: "hooks" }))],
    };
  }
  return {
    tone: "ok",
    mark: "check",
    title: "Recalla is connected to Claude Code",
    body: "Sessions are captured when they end, and every new session starts with a context pack built from what Recalla knows about the project.",
  };
}

function hero(o, c, onChange) {
  const s = status(o, c);
  const inj = o.injections;
  const m = o.memories;
  const actions = h("div", { class: "hero-actions" });
  if (s.install) {
    const btn = h("button", { type: "button", class: "btn attn" }, icon("plug", 16), "Install hooks");
    btn.addEventListener("click", async () => {
      btn.disabled = true;
      btn.lastChild.textContent = "Installing…";
      try {
        const r = await post("/api/hooks/install");
        toast(r.backup ? `Hooks installed. Your previous settings were backed up as ${r.backup}.` : "Hooks installed.");
        onChange();
      } catch (e) {
        if (e.status === 401) return;
        btn.disabled = false;
        btn.lastChild.textContent = "Install hooks";
        toast(`Install failed: ${e.message}`, "err");
      }
    });
    actions.append(btn);
  }
  if (s.action)
    actions.append(
      h(
        "a",
        {
          class: `btn ${s.tone === "fresh" ? "primary" : ""}`,
          href: s.action[1],
        },
        s.action[0],
      ),
    );
  if (s.second) actions.append(h("a", { class: "btn ghost", href: s.second[1] }, s.second[0]));
  const proof = (label, value, sub) =>
    h("div", null, h("dt", null, label), h("dd", null, value, sub ? h("small", null, sub) : null));
  return h(
    "section",
    { class: `hero ${s.tone}`, "aria-labelledby": "status-title" },
    h("span", { class: "hero-mark", "aria-hidden": "true" }, icon(s.mark, 20)),
    h(
      "div",
      { class: "hero-body" },
      h("h1", { id: "status-title", tabindex: "-1" }, s.title),
      h("p", null, s.body),
      actions.childElementCount ? actions : null,
      h(
        "dl",
        { class: "proof" },
        proof(
          "Last session captured",
          c.last_capture_at ? rel(c.last_capture_at) : "Never",
          o.sessions ? `${num(o.sessions)} sessions in total` : null,
        ),
        proof(
          "Last context pack",
          inj.last_at ? rel(inj.last_at) : "None yet",
          inj.count ? `${num(inj.count)} sent so far` : "Sent when a session starts",
        ),
        proof(
          "Memories",
          num(m.by_status.active ?? 0),
          m.stale ? `${num(m.stale)} need review` : `${num(m.total)} in total`,
        ),
        proof(
          "Context saved",
          inj.saved_pct == null ? "Not measured" : savedWords(inj.saved_pct),
          inj.saved_pct == null ? "Needs packs with a baseline" : "than sending every memory",
        ),
      ),
    ),
  );
}

function lists(o, signal) {
  const left = h("div", { class: "stack-lg" });
  const right = h("div", { class: "stack-lg" });
  const m = o.memories;
  const listBox = (title, path, params, render, aside, empty) => {
    const box = h("div", { class: "card list-card" }, loading());
    get(path, params, signal)
      .then((r) => {
        box.replaceChildren(
          r.items.length
            ? h("ul", { class: "rows", "aria-label": title, "data-rows": "" }, r.items.map(render))
            : empty,
        );
      })
      .catch((e) => {
        if (e.name !== "AbortError" && e.status !== 401) box.replaceChildren(errorState(e));
      });
    return section(title, box, aside);
  };
  if (m.stale) {
    left.append(
      listBox(
        "Needs review",
        "/api/memories",
        { status: "needs_reverify", limit: 5 },
        (x) => memoryRow(x),
        h(
          "a",
          {
            href: href(["memories"], new URLSearchParams({ status: "needs_reverify" })),
          },
          `Review all ${num(m.stale)}`,
        ),
      ),
    );
  }
  if (o.injections.count) {
    left.append(
      listBox(
        "Recent context packs",
        "/api/injections",
        { limit: 5 },
        packRow,
        h("a", { href: "#/packs" }, "All packs"),
      ),
    );
  }
  if (m.total) {
    left.append(
      listBox(
        "Recently learned",
        "/api/memories",
        { sort: "created", limit: 5 },
        (x) => memoryRow(x),
        h("a", { href: "#/memories" }, "All memories"),
      ),
    );
    const kinds = KINDS.filter((k) => m.by_kind[k]).map((k) =>
      h(
        "a",
        { href: href(["memories"], new URLSearchParams({ kind: k })) },
        h("span", { class: `kind kind-${k}` }, KIND_PLURAL[k], " ", h("span", { class: "n num" }, num(m.by_kind[k]))),
      ),
    );
    right.append(section("By kind", h("div", { class: "kinds card pad" }, kinds)));
    const proj = h("div", { class: "card list-card" }, loading());
    get("/api/projects", null, signal)
      .then((r) => {
        proj.replaceChildren(
          h(
            "ul",
            { class: "rows projects", "aria-label": "Projects" },
            r.items.slice(0, 8).map((p) =>
              h(
                "li",
                { class: "row" },
                h(
                  "a",
                  {
                    class: "row-title",
                    href: href(["memories"], new URLSearchParams({ project: p.project })),
                    title: p.project,
                  },
                  baseName(p.project),
                ),
                h("div", { class: "row-side num" }, `${num(p.memory_count)} memories`),
              ),
            ),
          ),
        );
      })
      .catch((e) => {
        if (e.name !== "AbortError" && e.status !== 401) proj.replaceChildren(errorState(e));
      });
    right.append(section("Projects", proj));
  }
  return left.childElementCount ? h("div", { class: "home-grid" }, left, right) : null;
}

export function mount(ctx) {
  ctx.title("Home");
  const top = h("div", null, loading("Loading status"));
  const below = h("div");
  const note = h("p", { class: "muted small foot-note", role: "status" });
  const el = h("div", { class: "view" }, top, below, note);
  let delay = POLL_MS;
  let timer = 0;
  let last = "";
  let lastLists = "";

  async function refresh() {
    try {
      const [o, c] = await Promise.all([get("/api/overview", null, ctx.signal), get("/api/connect", null, ctx.signal)]);
      delay = POLL_MS;
      const sig = JSON.stringify([{ ...o, generated_at: 0 }, c]);
      if (sig !== last) {
        const hadFocus = top.contains(document.activeElement);
        last = sig;
        top.replaceChildren(
          hero(o, c, () => {
            ctx.refreshSetupFlag();
            refresh();
          }),
        );
        if (hadFocus) top.querySelector("h1")?.focus();
        const listSig = JSON.stringify([o.memories, o.injections.count, o.sessions]);
        if (listSig !== lastLists) {
          lastLists = listSig;
          below.replaceChildren(lists(o, ctx.signal) ?? "");
        }
      }
      note.textContent = `Checked ${time(o.generated_at).textContent}. Updates every few seconds while this tab is open.`;
    } catch (e) {
      if (e.name === "AbortError" || e.status === 401) return;
      delay = Math.min(delay * 2, MAX_BACKOFF_MS);
      if (!last)
        top.replaceChildren(
          h("h1", { class: "sr", tabindex: "-1" }, "Home"),
          errorState(e, () => {
            clearTimeout(timer);
            refresh();
          }),
        );
      note.textContent = `Updates paused: ${e.message} Retrying in ${delay / 1000} seconds.`;
    }
    schedule();
  }
  function schedule() {
    clearTimeout(timer);
    if (!document.hidden && !ctx.signal.aborted) timer = setTimeout(refresh, delay);
  }
  const onVis = () => {
    clearTimeout(timer);
    if (!document.hidden) refresh();
  };
  document.addEventListener("visibilitychange", onVis);
  ctx.onCleanup(() => {
    clearTimeout(timer);
    document.removeEventListener("visibilitychange", onVis);
  });
  refresh();
  return { el };
}
