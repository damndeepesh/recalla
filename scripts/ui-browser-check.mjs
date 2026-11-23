// Real-browser check of the `recalla ui` login hand-off. Driven by
// scripts/ui-browser-check.sh; needs playwright-core (not a repo dependency).
// argv: <redirect page path> <login url> <screenshot path|-> ; env CHROME_PATH
import { createRequire } from "node:module";

const [page_path, login_url, shot] = process.argv.slice(2);
const require = createRequire(process.env.PLAYWRIGHT_DIR ? `${process.env.PLAYWRIGHT_DIR}/` : import.meta.url);
const { chromium } = require("playwright-core");

let failed = 0;
const check = (name, ok, extra = "") => {
  console.log(`${ok ? "ok   " : "FAIL "} ${name}${extra ? ` (${extra})` : ""}`);
  if (!ok) failed++;
};

const browser = await chromium.launch({ executablePath: process.env.CHROME_PATH || undefined });
const ctx = await browser.newContext();
const tab = await ctx.newPage();
const chain = [];
tab.on("response", (r) => {
  if (["document", "xhr", "fetch"].includes(r.request().resourceType()))
    chain.push(`${r.status()} ${r.url().replace(/\?c=[0-9a-f]+/, "?c=<code>")}`);
});

await tab.goto(`file://${page_path}`);
await tab.waitForURL(/^http:\/\/127\.0\.0\.1:\d+\/$/, { timeout: 10000 }).catch(() => {});
await tab.waitForLoadState("networkidle").catch(() => {});
console.log("request chain after the redirect page:\n  " + chain.join("\n  "));
const base = new URL(login_url).origin;
const body = await tab.locator("body").innerText();
check("redirect page lands on the dashboard (no Unauthorized)", !/Unauthorized/.test(body) && tab.url() === `${base}/`, tab.url());
const ov = await tab.evaluate(async () => (await fetch("/api/overview")).status);
check("/api/overview is 200 from the page", ov === 200, String(ov));
if (shot && shot !== "-") await tab.screenshot({ path: shot });

await tab.reload();
check("reload keeps the session", !/Unauthorized/.test(await tab.locator("body").innerText()));

const tab2 = await ctx.newPage();
await tab2.goto(`${base}/`);
check("second tab shares the session", !/Unauthorized/.test(await tab2.locator("body").innerText()));

const again = await ctx.newPage();
const resp = await again.goto(login_url);
check("the code link a second time is 401", resp.status() === 401, String(resp.status()));

await browser.close();
process.exit(failed ? 1 : 0);
