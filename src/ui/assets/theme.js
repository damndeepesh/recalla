// Runs before first paint (classic script, no inline code allowed by CSP).
try {
  const t = localStorage.getItem("recalla-theme");
  if (t === "light" || t === "dark") document.documentElement.dataset.theme = t;
} catch {
  // Storage can be blocked; the system theme applies.
}
