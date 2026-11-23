import { h, icon, num, bytes, time, savedText } from "../dom.js";
import { get } from "../api.js";
import { pageHead, panel, skeleton, errorState, emptyState, tokenBar, kindBadge } from "../ui.js";
import { href } from "../router.js";

const POLL_MS = 5000;
const MAX_BACKOFF_MS = 60000;

const stat = (label, value, sub) =>
  h("div", { class: "stat" }, h("dt", null, label), h("dd", { class: "stat-v num" }, value), h("dd", { class: "stat-sub" }, sub));

function kindBars(byKind, total) {
  const entries = Object.entries(byKind).sort((a, b) => b[1] - a[1]);
  if (!entries.length) return emptyState("No memories yet", "Memories appear after history is imported or an agent remembers something.");
  const max = Math.max(...entries.map(([, n]) => n));
  return h("ul", { class: "bars", "aria-label": "Memories by kind" }, entries.map(([k, n]) =>
    h("li", null,
      h("a", { class: "bar-label", href: href(["memories"], new URLSearchParams({ kind: k })) }, kindBadge(k)),
      h("span", { class: "bar-track", "aria-hidden": "true" }, h("span", { class: "bar-fill", css: { "--w": `${(n / max) * 100}%` } })),
      h("span", { class: "num bar-n" }, num(n), h("span", { class: "sr" }, ` of ${total} memories`)))));
}

function proof(inj) {
  if (!inj.count) {
    return emptyState("Nothing injected yet",
      "Once hooks or MCP hand a pack to an agent, you can inspect every one here.",
      h("a", { class: "btn", href: "#/connect" }, "Set up the connector"));
  }
  const saved = inj.saved_pct;
  let verdict;
  if (saved == null) verdict = h("p", { class: "proof-note" }, "Savings: not recorded. These injections predate baseline tracking.");
  else if (saved > 0) verdict = h("p", { class: "proof-big" }, h("span", { class: "num" }, `${saved}%`), " fewer tokens than sending every candidate memory");
  else verdict = h("p", { class: "proof-note" }, `The packs were larger than the candidates on this small data set (${savedText(saved)}), so there is no saving to show yet. It grows as memories pile up.`);
  return h("div", { class: "proof" },
    verdict,
    inj.baselined_est_tokens ? tokenBar(inj.baselined_est_tokens, inj.candidate_tokens) : null,
    h("dl", { class: "mini" },
      h("div", null, h("dt", null, "Injections"), h("dd", { class: "num" }, num(inj.count))),
      h("div", null, h("dt", null, "Tokens injected"), h("dd", { class: "num" }, num(inj.est_tokens))),
      h("div", null, h("dt", null, "Last"), h("dd", null, time(inj.last_at), inj.last_via ? ` via ${inj.last_via}` : ""))),
    h("a", { href: "#/injections" }, "See exactly what Claude received"));
}

function health(c) {
  const ok = c.hooks.filter((x) => x.installed).length;
  const row = (good, text) => h("li", { class: good ? "ok" : "warn" }, icon(good ? "check" : "alert", 16), h("span", null, text));
  return h("div", { class: "stack" },
    c.settings_error ? h("p", { class: "status err" }, `Can't read Claude settings: ${c.settings_error}`) : null,
    h("ul", { class: "checks" },
      c.hooks.map((x) => row(x.installed, `${x.event} hook ${x.installed ? "installed" : "not installed"}`)),
      row(c.db.exists && c.db.schema_version === c.db.expected_schema_version,
        c.db.exists ? `Database schema ${c.db.schema_version ?? "unknown"} of ${c.db.expected_schema_version}` : "No database yet")),
    h("p", { class: "muted" }, `${ok} of ${c.hooks.length} hooks installed. Last capture: `, time(c.last_capture_at), "."),
    h("a", { href: "#/connect" }, "Manage on Connect"));
}

function body(o, c) {
  const m = o.memories;
  const active = m.by_status.active ?? 0;
  const fresh = !o.sessions && !m.total;
  return h("div", { class: "stack" },
    fresh ? h("div", { class: "banner" }, h("div", null, h("strong", null, "No history imported yet."),
      h("p", null, "Run Import history on the Connect page to read your existing Claude Code and Codex sessions.")),
      h("a", { class: "btn", href: "#/connect" }, "Import history")) : null,
    h("dl", { class: "stats" },
      stat("Memories", num(active), m.stale ? `${num(m.stale)} need review, ${num(m.total)} total` : `${num(m.total)} total`),
      stat("Sessions", num(o.sessions), `${num(o.sources)} source files`),
      stat("Events", num(o.events), o.last_event_ts ? ["Last ", time(o.last_event_ts)] : "None captured"),
      stat("Database", bytes(o.db_bytes), `${bytes(o.archive_bytes)} archive`)),
    h("div", { class: "cols" },
      panel("Memories by kind", kindBars(m.by_kind, m.total)),
      panel("Context Claude actually received", proof(o.injections))),
    panel("Connector health", health(c)));
}

export function mount(ctx) {
  ctx.title("Overview");
  const content = h("div", null, skeleton(5));
  const note = h("p", { class: "sub", role: "status" });
  const el = h("div", { class: "view" }, pageHead("Overview", note), content);
  let delay = POLL_MS;
  let timer = 0;
  let last = "";

  async function refresh() {
    try {
      const [o, c] = await Promise.all([get("/api/overview", null, ctx.signal), get("/api/connect", null, ctx.signal)]);
      delay = POLL_MS;
      const sig = JSON.stringify([{ ...o, generated_at: 0 }, c]);
      if (sig !== last) {
        last = sig;
        content.replaceChildren(body(o, c));
      }
      note.textContent = `Updated ${new Date(o.generated_at).toLocaleTimeString()}. Refreshes every 5 seconds while this tab is visible.`;
    } catch (e) {
      if (e.name === "AbortError" || e.status === 401) return;
      delay = Math.min(delay * 2, MAX_BACKOFF_MS);
      if (!last) content.replaceChildren(errorState(e, () => { clearTimeout(timer); refresh(); }));
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
  ctx.onCleanup(() => { clearTimeout(timer); document.removeEventListener("visibilitychange", onVis); });
  refresh();
  return { el };
}
