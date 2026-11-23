// Tiny DOM helpers. All data is rendered through createElement + textContent;
// there is deliberately no HTML-string path anywhere in the app.
const SVG_NS = "http://www.w3.org/2000/svg";
const PROPS = new Set(["value", "checked", "disabled", "hidden", "selected", "open", "tabIndex", "textContent"]);

export function h(tag, props, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props ?? {})) {
    if (v == null || v === false) continue;
    if (k === "class") el.className = v;
    else if (k === "dataset") Object.assign(el.dataset, v);
    else if (k === "css") for (const [n, val] of Object.entries(v)) el.style.setProperty(n, val);
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (PROPS.has(k)) el[k] = v;
    else el.setAttribute(k, v === true ? "" : String(v));
  }
  append(el, children);
  return el;
}

export function append(el, children) {
  for (const c of children.flat(Infinity)) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}

/** Replaces an element's children, skipping null/false (replaceChildren would print "null"). */
export function fill(el, ...children) {
  el.replaceChildren();
  return append(el, children);
}

const ICONS = {
  overview: "M3 3h7v7H3zM14 3h7v4h-7zM14 11h7v10h-7zM3 14h7v7H3z",
  sessions: "M4 6h16M4 12h16M4 18h10",
  memories: "M12 3 3 8l9 5 9-5-9-5zM3 13l9 5 9-5M3 17.5l9 5 9-5",
  injections: "M4 12h11M11 7l5 5-5 5M20 4v16",
  connect: "M9 7V3m6 4V3M6 7h12v4a6 6 0 0 1-12 0V7zM12 17v4",
  sun: "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM12 2v2m0 16v2M4.9 4.9l1.4 1.4m11.4 11.4 1.4 1.4M2 12h2m16 0h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4",
  moon: "M20 14.5A8 8 0 0 1 9.5 4 8 8 0 1 0 20 14.5z",
  copy: "M9 9h11v11H9zM5 15V4h11",
  check: "M5 12.5 10 17.5 19 7",
  help: "M9.5 9a2.5 2.5 0 1 1 3.5 2.3c-.7.4-1 .9-1 1.7M12 17.5v.01M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z",
  alert: "M12 9v4m0 4v.01M10.3 3.9 2.4 18a2 2 0 0 0 1.7 3h15.8a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z",
  search: "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM21 21l-4.3-4.3",
  x: "M6 6l12 12M18 6 6 18",
};

export function icon(name, size = 18) {
  const s = document.createElementNS(SVG_NS, "svg");
  for (const [k, v] of Object.entries({
    viewBox: "0 0 24 24", width: size, height: size, fill: "none", stroke: "currentColor",
    "stroke-width": 1.8, "stroke-linecap": "round", "stroke-linejoin": "round",
    "aria-hidden": "true", focusable: "false", class: "icon",
  })) s.setAttribute(k, v);
  const p = document.createElementNS(SVG_NS, "path");
  p.setAttribute("d", ICONS[name]);
  s.append(p);
  return s;
}

// ---- formatting ----
const nf = new Intl.NumberFormat("en-US");
export const num = (n) => (n == null ? "n/a" : nf.format(n));

export function bytes(n) {
  if (n == null) return "n/a";
  const u = ["B", "KB", "MB", "GB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${i === 0 ? v : v.toFixed(v < 10 ? 1 : 0)} ${u[i]}`;
}

const UNITS = [[31536000, "year"], [2592000, "month"], [86400, "day"], [3600, "hour"], [60, "minute"]];
export function rel(ts, now = Date.now()) {
  if (ts == null) return "never";
  const s = Math.round((now - ts) / 1000);
  if (s < 45) return "just now";
  const [d, n] = UNITS.find(([d]) => s >= d * 0.75) ?? UNITS[4];
  const q = Math.max(1, Math.round(s / d));
  return `${q} ${n}${q === 1 ? "" : "s"} ago`;
}

const dtf = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
export const absTime = (ts) => dtf.format(new Date(ts));

/** `<time>` showing a relative label with the exact time as its title. */
export function time(ts) {
  if (ts == null) return h("span", { class: "muted" }, "never");
  return h("time", { datetime: new Date(ts).toISOString(), title: absTime(ts) }, rel(ts));
}

export const short = (id) => (id ? String(id).slice(0, 8) : "");
export const lastSegments = (p, n = 2) =>
  p ? p.split(/[\\/]/).filter(Boolean).slice(-n).join("/") || p : "";

/** Positive saved_pct = fewer tokens; negative = the pack was larger. */
export function savedText(pct) {
  if (pct == null) return "not recorded";
  return pct >= 0 ? `${pct}% saved` : `${Math.abs(pct)}% larger`;
}

export async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}
