// Filter forms whose state lives in the hash query.
import { h } from "./dom.js";
import { go } from "./router.js";

/** Form -> URLSearchParams; multi-value controls become comma lists. */
export function readForm(form, defaults = {}) {
  const fd = new FormData(form);
  const q = new URLSearchParams();
  for (const name of new Set(fd.keys())) {
    const v = fd.getAll(name).filter(Boolean).join(",");
    if (v && v !== defaults[name]) q.set(name, v);
  }
  return q;
}

/** Query -> form (so Back/Forward and shared links restore the controls). */
export function syncForm(form, query, defaults = {}) {
  for (const el of form.elements) {
    if (!el.name) continue;
    const v = query.get(el.name) ?? defaults[el.name] ?? "";
    if (el.type === "checkbox") el.checked = v.split(",").includes(el.value);
    else el.value = v;
  }
}

export function filterForm(segments, controls, { defaults = {}, label }) {
  const form = h("form", { class: "filters", role: "search", "aria-label": label }, controls,
    h("a", { class: "btn ghost small clear", href: `#/${segments.join("/")}` }, "Clear filters"));
  const apply = () => go(segments, readForm(form, defaults));
  form.addEventListener("change", apply);
  form.addEventListener("submit", (e) => { e.preventDefault(); apply(); });
  return form;
}
