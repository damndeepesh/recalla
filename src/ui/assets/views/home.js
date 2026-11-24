// Home: a bento of blocks. The status block answers "is Recalla working?"
// first; the rest show what it knows, what agents got, and what needs a look.
import { h, icon, num, rel, time, baseName } from "../dom.js";
import { get, post } from "../api.js";
import { href, packHref, sessionHref } from "../router.js";
import {
  errorState,
  memoryRow,
  toast,
    KINDS,
  KIND_PLURAL,
  AGENT_LABEL,
  VIA_LABEL,
  savedWords,
  block,
  skeleton,
} from "../ui.js";
import {
  gauge,
  ring,
  columns,
  pairedBars,
  segBar,
  illustration,
} from "../charts.js";

const POLL_MS = 5000;
const MAX_BACKOFF_MS = 60000;
const q = (o) => new URLSearchParams(o);

/** The one status sentence, its explanation and the single next action. */
function status(o, c) {
  const hooksOk = c.hooks.every((x) => x.installed);
  const schemaOk =
    c.db.exists && c.db.schema_version === c.db.expected_schema_version;
  if (c.settings_error) {
    return {
      tone: "err",
      title: "Recalla can't read your Claude Code settings",
      body: `${c.settings_error}. Hooks can't be checked or installed until the file is valid JSON.`,
      action: ["Open Setup", href(["setup"], q({ step: "hooks" }))],
    };
  }
  if (!schemaOk) {
    return {
      tone: "err",
      title: "The database needs attention",
      body: c.db.exists
        ? `Schema ${c.db.schema_version ?? "unknown"} found, ${c.db.expected_schema_version} expected. Restart recalla ui to upgrade it.`
        : "No database yet. Restart recalla ui to create it.",
      action: ["Open Setup", "#/setup"],
    };
  }
  if (!o.sessions && !o.memories.total) return { tone: "fresh" };
  if (!hooksOk) {
    const missing = c.hooks.filter((x) => !x.installed).length;
    return {
      tone: "attn",
      title: "Almost there: connect Claude Code",
      body: `Recalla knows ${num(o.memories.total)} memories from your history, but ${missing === c.hooks.length ? "no hooks are" : `${missing} of ${c.hooks.length} hooks aren't`} installed. New sessions aren't captured and agents don't get context on their own yet.`,
      install: true,
      second: ["What does this change?", href(["setup"], q({ step: "hooks" }))],
    };
  }
  return {
    tone: "ok",
    title: "Recalla is connected and working",
    body: "Sessions are captured when they end, and every new Claude Code session starts with a context pack built from what Recalla knows about the project.",
  };
}

function installButton(onChange, label = "Install hooks", cls = "btn attn") {
  const btn = h(
    "button",
    { type: "button", class: cls },
    icon("plug", 16),
    h("span", null, label),
  );
  btn.addEventListener("click", async () => {
    btn.disabled = true;
    btn.lastChild.textContent = "Installing…";
    try {
      const r = await post("/api/hooks/install");
      toast(
        r.backup
          ? `Hooks installed. Previous settings backed up as ${r.backup}.`
          : "Hooks installed.",
      );
      onChange();
    } catch (e) {
      if (e.status === 401) return;
      btn.disabled = false;
      btn.lastChild.textContent = label;
      toast(`Install failed: ${e.message}`, "err");
    }
  });
  return btn;
}

function hero(o, c, s, onChange) {
  const inj = o.injections;
  const m = o.memories;
  const actions = h("div", { class: "hero-actions" });
  if (s.install) actions.append(installButton(onChange));
  if (s.action)
    actions.append(h("a", { class: "btn", href: s.action[1] }, s.action[0]));
  if (s.second)
    actions.append(
      h("a", { class: "btn ghost", href: s.second[1] }, s.second[0]),
    );
  const installed = c.hooks.filter((x) => x.installed).length;
  const stat = (label, value, sub) =>
    h(
      "div",
      null,
      h("dt", null, label),
      h(
        "dd",
        null,
        h("span", { class: "stat-n" }, value),
        sub ? h("small", null, sub) : null,
      ),
    );
  return h(
    "section",
    { class: `block hero ${s.tone} span-8`, "aria-labelledby": "status-title" },
    h(
      "div",
      { class: "hero-top" },
      h(
        "div",
        { class: "hero-ring" },
        ring(
          s.tone === "ok" ? 3 : installed,
          3,
          `${installed} of 3 hooks installed`,
        ),
        h(
          "span",
          { class: "hero-glyph" },
          icon(
            s.tone === "ok" ? "check" : s.tone === "err" ? "alert" : "plug",
            18,
          ),
        ),
      ),
      h(
        "div",
        { class: "hero-body" },
        h(
          "p",
          { class: "eyebrow-status" },
          h("span", { class: "pulse" }),
          s.tone === "ok"
            ? "Healthy"
            : s.tone === "err"
              ? "Problem"
              : "Needs setup",
        ),
        h("h2", { id: "status-title", class: "hero-title" }, s.title),
        h("p", { class: "hero-text" }, s.body),
        actions.childElementCount ? actions : null,
      ),
    ),
    h(
      "dl",
      { class: "proof" },
      stat(
        "Last session captured",
        c.last_capture_at ? rel(c.last_capture_at) : "Never",
        `${num(o.sessions)} sessions in total`,
      ),
      stat(
        "Last context pack",
        inj.last_at ? rel(inj.last_at) : "None yet",
        inj.count
          ? `${num(inj.count)} sent so far`
          : "Sent when a session starts",
      ),
      stat(
        "Active memories",
        num(m.by_status.active ?? 0),
        m.stale ? `${num(m.stale)} need review` : `${num(m.total)} in total`,
      ),
    ),
  );
}

function firstRun(c, onChange) {
  const hooksOk = c.hooks.every((x) => x.installed);
  const card = (n, title, body, action) =>
    h(
      "li",
      { class: "onboard-step" },
      h("span", { class: "onboard-n" }, String(n)),
      h("h3", { class: "onboard-title" }, title),
      h("p", null, body),
      action,
    );
  return h(
    "section",
    { class: "block span-12 first-run", "aria-labelledby": "status-title" },
    h(
      "div",
      { class: "first-run-head" },
      illustration("empty"),
      h(
        "div",
        null,
        h(
          "p",
          { class: "eyebrow-status" },
          h("span", { class: "pulse" }),
          "New here",
        ),
        h(
          "h2",
          { id: "status-title", class: "hero-title" },
          "Give your coding agents a memory",
        ),
        h(
          "p",
          { class: "hero-text" },
          "Recalla reads the Claude Code and Codex sessions already on this machine, keeps the decisions, gotchas and preferences worth remembering, and hands each new session a short context pack. Nothing leaves this computer.",
        ),
      ),
    ),
    h(
      "ol",
      { class: "onboard" },
      card(
        1,
        "Import your history",
        "Read past sessions and extract the first memories. Takes seconds.",
        h(
          "a",
          {
            class: "btn primary",
            href: href(["setup"], q({ step: "import" })),
          },
          icon("download", 16),
          "Import history",
        ),
      ),
      card(
        2,
        "Connect Claude Code",
        "Three hooks capture new sessions and deliver context when one starts.",
        hooksOk
          ? h("span", { class: "flag ok" }, icon("check", 12), "Connected")
          : installButton(onChange, "Install hooks", "btn"),
      ),
      card(
        3,
        "See what an agent gets",
        "Preview a context pack for any project before an agent ever sees it.",
        h(
          "a",
          { class: "btn", href: href(["setup"], q({ step: "test" })) },
          icon("play", 15),
          "Test recall",
        ),
      ),
    ),
  );
}

function health(o, c) {
  const installed = c.hooks.filter((x) => x.installed).length;
  const schemaOk =
    c.db.exists && c.db.schema_version === c.db.expected_schema_version;
  const row = (tone, title, sub, link) =>
    h(
      "li",
      { class: `health-row ${tone}` },
      h("span", { class: "dot", "aria-hidden": "true" }),
      h("div", null, h("strong", null, title), h("span", null, sub)),
      link,
    );
  return block(
    "Connections",
    "plug",
    h(
      "ul",
      { class: "health" },
      row(
        installed === c.hooks.length ? "ok" : "attn",
        "Claude Code hooks",
        `${installed} of ${c.hooks.length} installed`,
        h(
          "a",
          {
            class: "btn sm ghost",
            href: href(["setup"], q({ step: "hooks" })),
          },
          installed === c.hooks.length ? "Manage" : "Fix",
        ),
      ),
      row(
        c.last_capture_at ? "ok" : "idle",
        "Session capture",
        c.last_capture_at
          ? `Last ${rel(c.last_capture_at)}`
          : "Nothing captured yet",
      ),
      row(
        o.injections.count ? "ok" : "idle",
        "Context delivery",
        o.injections.last_at
          ? `Last pack ${rel(o.injections.last_at)}${o.injections.last_via ? `, ${VIA_LABEL[o.injections.last_via]?.toLowerCase() ?? o.injections.last_via}` : ""}`
          : "No pack sent yet",
      ),
      row(
        schemaOk ? "ok" : "err",
        "Local database",
        schemaOk
          ? `Schema ${c.db.schema_version}, up to date`
          : "Needs upgrading",
      ),
    ),
    { span: 4 },
  );
}

function savings(o) {
  const inj = o.injections;
  if (inj.saved_pct == null) {
    return block(
      "Context saved",
      "bolt",
      h(
        "div",
        { class: "savings" },
        h("p", { class: "big-n muted" }, "–"),
        h(
          "p",
          { class: "muted small" },
          inj.count
            ? "These packs were sent before Recalla measured candidates."
            : "Appears after the first context pack.",
        ),
      ),
      { span: 4 },
    );
  }
  const pct = inj.saved_pct;
  return block(
    "Context saved",
    "bolt",
    h(
      "div",
      { class: "savings" },
      h(
        "div",
        { class: "gauge-wrap" },
        gauge(
          pct > 0 ? pct : 0,
          `${savedWords(pct)} than sending every memory`,
        ),
        h(
          "div",
          { class: "gauge-label" },
          h(
            "span",
            { class: "big-n num" },
            pct >= 0 ? `${Math.round(pct)}%` : `+${Math.round(-pct)}%`,
          ),
          h("span", null, pct >= 0 ? "fewer tokens" : "larger"),
        ),
      ),
      h(
        "p",
        { class: "small muted center" },
        h("strong", { class: "num" }, num(inj.baselined_est_tokens)),
        " tokens sent instead of ",
        h("strong", { class: "num" }, num(inj.candidate_tokens)),
        ", across ",
        num(inj.count),
        " packs.",
      ),
    ),
    { span: 4, aside: h("a", { href: "#/packs" }, "Packs") },
  );
}

function activity(o) {
  const a = o.activity ?? [];
  const sum = (k) => a.reduce((n, d) => n + d[k], 0);
  const stat = (cls, label, v) =>
    h(
      "div",
      { class: `act-stat ${cls}` },
      h("span", { class: "key", "aria-hidden": "true" }),
      h("span", { class: "stat-n num" }, num(v)),
      h("span", null, label),
    );
  const first =
    a[0] &&
    new Date(a[0].day).toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
    });
  return block(
    "Last 30 days",
    "clock",
    h(
      "div",
      { class: "activity" },
      h(
        "div",
        { class: "act-stats" },
        stat("k-mem", "memories learned", sum("memories")),
        stat("k-none", "packs sent", sum("packs")),
        stat("k-ses", "sessions", sum("sessions")),
      ),
      h(
        "div",
        { class: "chart" },
        columns(a, {
          value: (d) => d.memories,
          label: `Memories learned per day since ${first}`,
        }),
        h(
          "div",
          { class: "chart-axis", "aria-hidden": "true" },
          h("span", null, first),
          h("span", null, "Today"),
        ),
      ),
    ),
    { span: 8 },
  );
}

function kinds(o) {
  const m = o.memories;
  const parts = KINDS.map((k) => ({
    key: k,
    value: m.by_kind[k] ?? 0,
    label: KIND_PLURAL[k],
  }));
  return block(
    "What it knows",
    "memories",
    h(
      "div",
      { class: "kinds-block" },
      h(
        "p",
        null,
        h("span", { class: "big-n num" }, num(m.total)),
        h("span", { class: "muted" }, " memories"),
      ),
      segBar(
        parts,
        KINDS.map((k) => `${KIND_PLURAL[k]} ${m.by_kind[k] ?? 0}`).join(", "),
      ),
      h(
        "ul",
        { class: "legend" },
        parts
          .filter((p) => p.value)
          .sort((a, b) => b.value - a.value)
          .map((p) =>
            h(
              "li",
              null,
              h(
                "a",
                { href: href(["memories"], q({ kind: p.key })) },
                h("span", { class: `kind kind-${p.key}` }, p.label),
                h("span", { class: "num" }, num(p.value)),
                h(
                  "span",
                  { class: "pct num" },
                  `${Math.round((p.value / (m.total || 1)) * 100)}%`,
                ),
              ),
            ),
          ),
      ),
    ),
    { span: 4 },
  );
}

function loadBlock(title, ic, opts, loader) {
  const body = h("div", { class: "block-body" }, skeleton(3));
  loader()
    .then((el) => body.replaceChildren(el))
    .catch((e) => {
      if (e.name !== "AbortError" && e.status !== 401)
        body.replaceChildren(errorState(e));
    });
  return block(title, ic, body, opts);
}

function perPack(signal) {
  return loadBlock(
    "Tokens per pack",
    "packs",
    { span: 8, aside: h("a", { href: "#/packs" }, "All packs") },
    async () => {
      const r = await get("/api/injections", { limit: 30 }, signal);
      const items = r.items.slice().reverse();
      return h(
        "div",
        { class: "pairs" },
        h(
          "div",
          { class: "legend-row" },
          h("span", { class: "lg cand" }, "Every candidate memory"),
          h("span", { class: "lg sent" }, "What the agent got"),
        ),
        pairedBars(
          items,
          `Last ${items.length} packs: tokens sent compared with sending every candidate`,
        ),
        h(
          "div",
          { class: "chart-axis", "aria-hidden": "true" },
          h("span", null, "Older"),
          h("span", null, "Latest"),
        ),
      );
    },
  );
}

function needsReview(o, signal) {
  return loadBlock(
    "Needs review",
    "alert",
    {
      span: 7,
      aside: h(
        "a",
        { href: href(["review"]) },
        `Review all ${num(o.memories.stale)}`,
      ),
    },
    async () => {
      const r = await get(
        "/api/memories",
        { status: "needs_reverify", limit: 5 },
        signal,
      );
      return h(
        "ul",
        {
          class: "rows",
          "aria-label": "Memories that need review",
          "data-rows": "",
        },
        r.items.map((x) => memoryRow(x)),
      );
    },
  );
}

function projectsBlock(signal) {
  return loadBlock("Projects", "folder", { span: 5 }, async () => {
    const r = await get("/api/projects", null, signal);
    const max = Math.max(1, ...r.items.map((p) => p.memory_count));
    return h(
      "ul",
      { class: "proj-list" },
      r.items
        .slice(0, 7)
        .map((p) =>
          h(
            "li",
            null,
            h(
              "a",
              {
                href: href(["memories"], q({ project: p.project })),
                title: p.project,
              },
              h("span", { class: "proj-name" }, baseName(p.project)),
              h("span", { class: "num muted" }, num(p.memory_count)),
              h(
                "span",
                { class: "proj-bar", "aria-hidden": "true" },
                h("span", {
                  css: { "--w": `${(p.memory_count / max) * 100}%` },
                }),
              ),
            ),
          ),
        ),
    );
  });
}

function timeline(signal, span = 7) {
  return loadBlock("Recent activity", "clock", { span }, async () => {
    const [ses, packs] = await Promise.all([
      get("/api/sessions", { limit: 6 }, signal),
      get("/api/injections", { limit: 6 }, signal),
    ]);
    const ev = [
      ...ses.items.map((s) => ({
        at: s.last_ts,
        kind: "ses",
        title: `${AGENT_LABEL[s.harness] ?? s.harness} session in ${s.project ? baseName(s.project) : "no project"}`,
        sub: `${num(s.event_count)} events${s.memory_count ? `, ${num(s.memory_count)} memories` : ""}`,
        href: sessionHref(s.harness, s.session_id),
      })),
      ...packs.items.map((p) => ({
        at: p.at,
        kind: "pack",
        title: `Pack sent to ${p.project ? baseName(p.project) : "an agent"}`,
        sub: `${num(p.est_tokens)} tokens, ${num(p.memory_count)} memories, ${VIA_LABEL[p.via]?.toLowerCase() ?? p.via}`,
        href: packHref(p.id),
      })),
    ]
      .sort((a, b) => (b.at ?? 0) - (a.at ?? 0))
      .slice(0, 8);
    if (!ev.length) return h("p", { class: "muted small" }, "Nothing yet.");
    return h(
      "ol",
      { class: "timeline" },
      ev.map((e) =>
        h(
          "li",
          { class: `tl ${e.kind}` },
          h(
            "span",
            { class: "tl-dot", "aria-hidden": "true" },
            icon(e.kind === "ses" ? "sessions" : "packs", 13),
          ),
          h(
            "div",
            null,
            h("a", { href: e.href }, e.title),
            h("span", { class: "muted small" }, e.sub),
          ),
          h("span", { class: "muted small tl-time" }, time(e.at)),
        ),
      ),
    );
  });
}

function recent(signal, span = 5) {
  return loadBlock(
    "Recently learned",
    "memories",
    { span, aside: h("a", { href: "#/memories" }, "All memories") },
    async () => {
      const r = await get(
        "/api/memories",
        { sort: "created", limit: 5 },
        signal,
      );
      return h(
        "ul",
        {
          class: "rows compact",
          "aria-label": "Recently learned",
          "data-rows": "",
        },
        r.items.map((x) => memoryRow(x)),
      );
    },
  );
}

export function mount(ctx) {
  ctx.title("Home");
  const grid = h(
    "div",
    { class: "bento" },
    h("div", { class: "block span-8 sk-hero" }, skeleton(3)),
    h("div", { class: "block span-4" }, skeleton(3)),
  );
  const note = h("p", { class: "muted small foot-note", role: "status" });
  const el = h(
    "div",
    { class: "view" },
    h("h1", { class: "sr", tabindex: "-1" }, "Home"),
    grid,
    note,
  );
  let delay = POLL_MS;
  let timer = 0;
  let last = "";

  async function refresh() {
    try {
      const [o, c] = await Promise.all([
        get("/api/overview", null, ctx.signal),
        get("/api/connect", null, ctx.signal),
      ]);
      delay = POLL_MS;
      const sig = JSON.stringify([{ ...o, generated_at: 0 }, c]);
      if (sig !== last) {
        last = sig;
        const hadFocus = grid.contains(document.activeElement);
        const s = status(o, c);
        const onChange = () => {
          ctx.refreshSetupFlag();
          refresh();
        };
        if (s.tone === "fresh") grid.replaceChildren(firstRun(c, onChange));
        else {
          grid.replaceChildren(
            ...[
            hero(o, c, s, onChange),
            health(o, c),
            activity(o),
            savings(o),
            o.injections.count ? perPack(ctx.signal) : null,
            kinds(o),
            o.memories.stale ? needsReview(o, ctx.signal) : recent(ctx.signal, 7),
            projectsBlock(ctx.signal),
            timeline(ctx.signal, o.memories.stale ? 7 : 12),
            o.memories.stale ? recent(ctx.signal) : null,
            ].filter(Boolean),
          );
          if (!o.injections.count)
            grid
              .querySelector(".kinds-block")
              ?.closest(".block")
              .classList.replace("span-4", "span-12");
        }
        if (hadFocus) el.querySelector("h1")?.focus();
      }
      note.textContent = `Checked ${time(o.generated_at).textContent}. Updates every few seconds while this tab is open.`;
    } catch (e) {
      if (e.name === "AbortError" || e.status === 401) return;
      delay = Math.min(delay * 2, MAX_BACKOFF_MS);
      if (!last)
        grid.replaceChildren(
          h(
            "div",
            { class: "span-12" },
            errorState(e, () => {
              clearTimeout(timer);
              refresh();
            }),
          ),
        );
      note.textContent = `Updates paused: ${e.message} Retrying in ${delay / 1000} seconds.`;
    }
    schedule();
  }
  function schedule() {
    clearTimeout(timer);
    if (!document.hidden && !ctx.signal.aborted)
      timer = setTimeout(refresh, delay);
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

