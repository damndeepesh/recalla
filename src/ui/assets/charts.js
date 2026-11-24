// Small hand-built charts (inline SVG via createElementNS, no libraries).
// Each returns an element with a text alternative; colours come from CSS.
import { h, num } from "./dom.js";

const NS = "http://www.w3.org/2000/svg";
function svg(tag, attrs = {}, ...kids) {
  const el = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) if (v != null) el.setAttribute(k, String(v));
  for (const c of kids.flat()) if (c) el.append(c);
  return el;
}
const frame = (w, hgt, cls, label) =>
  svg("svg", {
    viewBox: `0 0 ${w} ${hgt}`,
    class: cls,
    role: "img",
    "aria-label": label,
    preserveAspectRatio: "none",
  });

/** Half-ring gauge for a 0..100 share. */
export function gauge(pct, label) {
  const p = Math.max(0, Math.min(100, pct ?? 0));
  const r = 80;
  const len = Math.PI * r;
  const arc = "M 20 100 A 80 80 0 0 1 180 100";
  const root = svg("svg", {
    viewBox: "0 0 200 112",
    class: "gauge",
    role: "img",
    "aria-label": label,
  });
  root.append(
    svg("path", { d: arc, class: "gauge-track", pathLength: len }),
    svg("path", {
      d: arc,
      class: "gauge-fill",
      pathLength: len,
      "stroke-dasharray": `${(p / 100) * len} ${len}`,
    }),
  );
  return root;
}

/** Status ring: a full circle with `done` of `total` segments lit. */
export function ring(done, total, label) {
  const root = svg("svg", {
    viewBox: "0 0 44 44",
    class: "ring",
    role: "img",
    "aria-label": label,
  });
  const c = 2 * Math.PI * 18;
  const gap = total > 1 ? 3 : 0;
  const seg = c / total - gap;
  for (let i = 0; i < total; i++) {
    root.append(
      svg("circle", {
        cx: 22,
        cy: 22,
        r: 18,
        class: i < done ? "ring-on" : "ring-off",
        "stroke-dasharray": `${seg} ${c - seg}`,
        "stroke-dashoffset": -(i * (seg + gap)) + c / 4,
      }),
    );
  }
  return root;
}

/** Column chart of daily values with an optional line overlay. */
export function columns(series, { value, line, label }) {
  const w = 600;
  const hgt = 140;
  const n = series.length;
  const max = Math.max(1, ...series.map(value));
  const lmax = line ? Math.max(1, ...series.map(line)) : 1;
  const step = w / n;
  const bw = Math.max(2, step * 0.62);
  const root = frame(w, hgt, "cols-chart", label);
  for (let i = 0; i < 4; i++)
    root.append(
      svg("line", {
        x1: 0,
        x2: w,
        y1: (hgt / 4) * i + 0.5,
        y2: (hgt / 4) * i + 0.5,
        class: "grid",
      }),
    );
  series.forEach((d, i) => {
    const v = value(d);
    const bh = v ? Math.max(3, (v / max) * (hgt - 8)) : 0;
    const x = i * step + (step - bw) / 2;
    if (bh)
      root.append(
        svg(
          "rect",
          {
            x,
            y: hgt - bh,
            width: bw,
            height: bh,
            rx: Math.min(3, bw / 2),
            class: "col",
          },
          svg(
            "title",
            {},
            document.createTextNode(
              `${new Date(d.day).toLocaleDateString(undefined, { month: "short", day: "numeric" })}: ${num(v)}`,
            ),
          ),
        ),
      );
  });
  if (line) {
    const pts = series.map((d, i) => `${(i + 0.5) * step},${hgt - 6 - (line(d) / lmax) * (hgt - 20)}`);
    root.append(
      svg("polyline", {
        points: pts.join(" "),
        class: "line",
        "vector-effect": "non-scaling-stroke",
      }),
    );
  }
  return root;
}

/** Tiny sparkline. */
export function sparkline(values, label) {
  const w = 120;
  const hgt = 32;
  const max = Math.max(1, ...values);
  const step = values.length > 1 ? w / (values.length - 1) : w;
  const pts = values.map((v, i) => `${i * step},${hgt - 3 - (v / max) * (hgt - 6)}`);
  const root = frame(w, hgt, "spark", label);
  root.append(
    svg("polygon", {
      points: `0,${hgt} ${pts.join(" ")} ${w},${hgt}`,
      class: "spark-area",
    }),
    svg("polyline", {
      points: pts.join(" "),
      class: "spark-line",
      "vector-effect": "non-scaling-stroke",
    }),
  );
  return root;
}

/** Paired bars per pack: everything (light) vs what was sent (dark). */
export function pairedBars(items, label) {
  const w = 600;
  const hgt = 120;
  const n = Math.max(items.length, 1);
  const max = Math.max(1, ...items.map((p) => Math.max(p.candidate_tokens ?? 0, p.est_tokens)));
  const step = w / n;
  const bw = Math.max(3, step * 0.56);
  const root = frame(w, hgt, "pairs-chart", label);
  items.forEach((p, i) => {
    const x = i * step + (step - bw) / 2;
    const known = p.candidate_tokens != null && p.candidate_tokens >= p.est_tokens;
    const ch = known ? (p.candidate_tokens / max) * (hgt - 4) : 0;
    const ph = Math.max(2, (p.est_tokens / max) * (hgt - 4));
    const t = svg(
      "title",
      {},
      document.createTextNode(
        `Pack ${p.id}: ${num(p.est_tokens)} of ${p.candidate_tokens == null ? "unknown" : num(p.candidate_tokens)} tokens`,
      ),
    );
    if (ch)
      root.append(
        svg("rect", {
          x,
          y: hgt - ch,
          width: bw,
          height: ch,
          rx: 2,
          class: "cand",
        }),
      );
    root.append(
      svg(
        "rect",
        {
          x,
          y: hgt - ph,
          width: bw,
          height: ph,
          rx: 2,
          class: "sent",
        },
        t,
      ),
    );
  });
  return root;
}

/** One bar split into coloured parts: [{key, value, label}]. */
export function segBar(parts, label) {
  const total = parts.reduce((n, p) => n + p.value, 0) || 1;
  return h(
    "div",
    { class: "segbar", role: "img", "aria-label": label },
    parts
      .filter((p) => p.value)
      .map((p) =>
        h("span", {
          class: `seg kind-${p.key}`,
          css: { "--w": `${(p.value / total) * 100}%` },
          title: `${p.label}: ${num(p.value)}`,
        }),
      ),
  );
}

/** Spot illustrations for empty states (decorative). */
export function illustration(kind = "empty") {
  const root = svg("svg", {
    viewBox: "0 0 160 110",
    class: "illus",
    "aria-hidden": "true",
    focusable: "false",
  });
  const card = (x, y, cls) => svg("rect", { x, y, width: 96, height: 58, rx: 10, class: cls });
  const lines = (x, y) => [
    svg("rect", {
      x: x + 14,
      y: y + 16,
      width: 52,
      height: 7,
      rx: 3.5,
      class: "il-ink",
    }),
    svg("rect", {
      x: x + 14,
      y: y + 31,
      width: 68,
      height: 5,
      rx: 2.5,
      class: "il-dim",
    }),
    svg("rect", {
      x: x + 14,
      y: y + 41,
      width: 40,
      height: 5,
      rx: 2.5,
      class: "il-dim",
    }),
  ];
  root.append(card(44, 14, "il-back"), card(30, 28, "il-mid"), card(16, 42, "il-front"), ...lines(16, 42));
  if (kind === "search")
    root.append(
      svg("circle", { cx: 122, cy: 70, r: 14, class: "il-lens" }),
      svg("path", { d: "M132 80l12 12", class: "il-stroke" }),
    );
  else if (kind === "plug")
    root.append(
      svg("circle", { cx: 128, cy: 40, r: 16, class: "il-accent" }),
      svg("path", { d: "M122 40h12M128 34v12", class: "il-on-accent" }),
    );
  else if (kind === "done")
    root.append(
      svg("circle", { cx: 128, cy: 40, r: 16, class: "il-accent" }),
      svg("path", { d: "M121 40.5l5 5 9-10", class: "il-on-accent" }),
    );
  else root.append(svg("circle", { cx: 128, cy: 40, r: 9, class: "il-accent" }));
  return root;
}
