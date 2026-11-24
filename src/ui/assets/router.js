// Hash routing. Filters live in the hash query (#/memories?kind=fact) so views
// are linkable and the Back button works.
export function parseHash() {
  const raw = location.hash.replace(/^#\/?/, "");
  const [path, qs = ""] = raw.split("?");
  const segments = path
    .split("/")
    .filter(Boolean)
    .map((s) => {
      try {
        return decodeURIComponent(s);
      } catch {
        return s;
      }
    });
  return { segments, query: new URLSearchParams(qs) };
}

export function href(segments = [], query) {
  const path = segments.map(encodeURIComponent).join("/");
  const qs = query
    ? [...query]
        .filter(([, v]) => v !== "")
        .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`)
        .join("&")
    : "";
  return `#/${path}${qs ? `?${qs}` : ""}`;
}

/** Navigates within the app; `replace` avoids a history entry (typing in search). */
export function go(segments, query, replace = false) {
  const target = href(segments, query);
  if (replace) {
    history.replaceState(null, "", target);
    window.dispatchEvent(new HashChangeEvent("hashchange"));
  } else location.hash = target;
}

export const sessionHref = (harness, id) => href(["sessions", harness, id]);
export const memoryHref = (uid, query) => href(["memories", uid], query);
export const packHref = (id) => href(["packs", String(id)]);

const VIEWS = {
  home: () => import("./views/home.js"),
  memories: () => import("./views/memories.js"),
  review: () => import("./views/review.js"),
  sessions: () => import("./views/sessions.js"),
  packs: () => import("./views/packs.js"),
  setup: () => import("./views/setup.js"),
};
/** Old route names (bookmarks from 0.2) map to the new ones. */
export const ALIASES = {
  overview: "home",
  injections: "packs",
  connect: "setup",
};
const DETAIL = { memories: 1, sessions: 2, packs: 1 };

/** Maps path segments to {nav, load} or null for unknown routes. */
export function match(segments) {
  const [first = "home", ...rest] = segments;
  const ok = rest.length === 0 || rest.length === DETAIL[first];
  return VIEWS[first] && ok ? { nav: first, load: VIEWS[first] } : null;
}
