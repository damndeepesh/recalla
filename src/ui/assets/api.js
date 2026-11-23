// fetch wrapper: JSON in/out, CSRF on every POST, uniform errors.
export class ApiError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

let csrf = null;
let version = "";
const expired = new EventTarget();
export const onExpired = (fn) => expired.addEventListener("expired", fn);

const GENERIC = {
  400: "The server rejected that request.",
  403: "The server refused that request. Reload the page and try again.",
  404: "Not found.",
  409: "That conflicts with the current state.",
  503: "The database is busy or needs upgrading. Try again in a moment.",
};

async function request(method, path, { params, body, signal } = {}) {
  const url = new URL(path, location.origin);
  for (const [k, v] of Object.entries(params ?? {})) {
    if (v !== "" && v != null) url.searchParams.set(k, v);
  }
  const headers = {};
  if (method === "POST") {
    headers["X-Recalla-Csrf"] = csrf ?? (await session()).csrf;
    if (body !== undefined) headers["Content-Type"] = "application/json";
  }
  let res;
  try {
    res = await fetch(url, {
      method,
      headers,
      signal,
      credentials: "same-origin",
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch (e) {
    if (e.name === "AbortError") throw e;
    throw new ApiError(0, "Cannot reach the dashboard server. Is `recalla ui` still running?");
  }
  const raw = await res.text();
  let data = null;
  try {
    data = raw ? JSON.parse(raw) : null;
  } catch {
    data = null; // plain-text error from the HTTP core
  }
  if (res.status === 401) {
    expired.dispatchEvent(new Event("expired"));
    throw new ApiError(401, "Session ended.");
  }
  if (!res.ok) {
    const msg =
      typeof data?.error === "string" ? data.error : (GENERIC[res.status] ?? `Request failed (${res.status}).`);
    const err = new ApiError(res.status, msg);
    err.data = data;
    throw err;
  }
  return data;
}

export const get = (path, params, signal) => request("GET", path, { params, signal });
export const post = (path, body, signal) => request("POST", path, { body, signal });
export const enc = encodeURIComponent;

export async function session() {
  const s = await request("GET", "/api/session");
  csrf = s.csrf;
  version = s.version;
  return s;
}
export const appVersion = () => version;
