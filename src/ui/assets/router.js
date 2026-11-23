// Hash routing. Filters live in the hash query (#/memories?kind=fact) so views
// are linkable and the Back button works.
export function parseHash() {
  const raw = location.hash.replace(/^#\/?/, "");
  const [path, qs = ""] = raw.split("?");
  const segments = path.split("/").filter(Boolean).map((s) => {
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
  const qs = query ? [...query].filter(([, v]) => v !== "").map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`).join("&") : "";
  return `#/${path}${qs ? `?${qs}` : ""}`;
}

/** Navigates within the app; `replace` avoids a history entry (typing in search). */
export function go(segments, query, replace = false) {
  const target = href(segments, query);
  if (replace) history.replaceState(null, "", target);
  else location.hash = target;
  if (replace) window.dispatchEvent(new HashChangeEvent("hashchange"));
}

export const sessionHref = (harness, id) => href(["sessions", harness, id]);
export const memoryHref = (uid) => href(["memories", uid]);
export const injectionHref = (id) => href(["injections", id]);

const VIEWS = {
  overview: () => import("./views/overview.js"),
  sessions: () => import("./views/sessions.js"),
  memories: () => import("./views/memories.js"),
  injections: () => import("./views/injections.js"),
  connect: () => import("./views/connect.js"),
};

/** Maps path segments to {nav, load} or null for unknown routes. */
export function match(segments) {
  const [first = "overview", ...rest] = segments;
  const detail = { sessions: 2, memories: 1, injections: 1 }[first];
  const ok = first === "overview" || first === "connect" ? rest.length === 0 : rest.length === 0 || rest.length === detail;
  return VIEWS[first] && ok ? { nav: first, load: VIEWS[first] } : null;
}
