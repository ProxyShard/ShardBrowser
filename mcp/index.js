#!/usr/bin/env node
// MCP server for the ShardX Launcher.
//
// Bridges an MCP client (Claude, Cursor, …) to:
//   1. the launcher's local automation HTTP API (profiles, proxies,
//      fingerprints, cookies, folders), and
//   2. a launched profile's browser over CDP — driven with **patchright**
//      (a stealth-patched Playwright) so the automation stays undetected.
//
// Config via env:
//   SHARDX_API    base URL of the launcher API  (default http://127.0.0.1:40325)
//   SHARDX_TOKEN  Bearer token from Settings → Automation API  (required)

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { execFileSync } from "node:child_process";
import { z } from "zod";
import { chromium } from "patchright";

function readWindowsUserEnv(name) {
  if (process.platform !== "win32") return "";
  try {
    const out = execFileSync("reg", ["query", "HKCU\\Environment", "/v", name], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
      windowsHide: true,
    });
    const line = out.split(/\r?\n/).find((l) => l.trim().startsWith(name));
    return line?.trim().split(/\s{2,}/).slice(2).join(" ").trim() || "";
  } catch {
    return "";
  }
}

const env = (name) => process.env[name] || readWindowsUserEnv(name);
const API = (env("SHARDX_API") || "http://127.0.0.1:40325").replace(/\/+$/, "");
const TOKEN = env("SHARDX_TOKEN") || "";

// ---------- HTTP API helper ----------

async function api(path, { method = "GET", body } = {}) {
  const res = await fetch(API + path, {
    method,
    headers: {
      ...(TOKEN ? { Authorization: `Bearer ${TOKEN}` } : {}),
      ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  let data;
  try { data = text ? JSON.parse(text) : null; } catch { data = text; }
  if (!res.ok) {
    const msg = data && data.error ? data.error : `HTTP ${res.status}`;
    throw new Error(`${method} ${path} → ${msg}`);
  }
  return data;
}

// ---------- CDP (patchright) connection cache ----------

const browsers = new Map(); // profile_id → patchright Browser
const activePage = new Map(); // profile_id → active Page

async function cdpEndpoint(profileId, { autostart = true, headless = false } = {}) {
  const running = await api("/running");
  let entry = running.find((r) => r.profile_id === profileId);
  if (!entry?.cdp && autostart) {
    const started = await api(`/profiles/${profileId}/start`, {
      method: "POST",
      body: { headless },
    });
    entry = { cdp: started.cdp };
  }
  const cdp = entry?.cdp;
  if (!cdp?.http_url) {
    throw new Error(`profile ${profileId} is not running with CDP (start it first)`);
  }
  return cdp;
}

async function browserFor(profileId, opts) {
  let b = browsers.get(profileId);
  if (!b || !b.isConnected()) {
    const cdp = await cdpEndpoint(profileId, opts);
    b = await chromium.connectOverCDP(cdp.http_url);
    browsers.set(profileId, b);
  }
  return b;
}

async function contextFor(profileId, opts) {
  const b = await browserFor(profileId, opts);
  return b.contexts()[0] ?? (await b.newContext());
}

// The "active" page for a profile — the one tab tools/actions operate on.
// Persists across calls; falls back to the first real page (or a new one).
async function pageFor(profileId, opts) {
  const ctx = await contextFor(profileId, opts);
  const cur = activePage.get(profileId);
  if (cur && !cur.isClosed() && cur.context() === ctx) return cur;
  const pages = ctx.pages().filter((p) => !p.url().startsWith("devtools://"));
  const p = pages[0] ?? (await ctx.newPage());
  activePage.set(profileId, p);
  return p;
}

// Locator with a default timeout, shared by element actions.
const loc = (page, selector) => page.locator(selector).first();
const TIMEOUT = 15000;

// ---------- Motion: human pointer and keystrokes ----------
//
// The `Motion` domain lives on the BROWSER target, so it needs a browser-level
// CDP session. These wrappers only turn a selector into the coordinates it wants.

const motion = new Map(); // profile_id → { browser, session, pointer }

async function motionFor(profileId, opts) {
  const b = await browserFor(profileId, opts);
  const cur = motion.get(profileId);
  if (cur && cur.browser === b && b.isConnected()) return cur;
  const m = { browser: b, session: await b.newBrowserCDPSession(), pointer: false };
  motion.set(profileId, m);
  return m;
}

// Resting cursor position. Never the target — a glide starting on top of what
// it aims at has no trajectory and no duration.
async function ensurePointer(m, page) {
  if (m.pointer) return;
  const [w, h] = await page
    .evaluate(() => [window.innerWidth, window.innerHeight])
    .catch(() => [1280, 800]);
  await m.session.send("Motion.createPointer", {
    x: Math.round(w * 0.15),
    y: Math.round(h * 0.8),
  });
  m.pointer = true;
}

// Selector → viewport point. Scrolled into view first; `width` travels along
// because it feeds Fitts's law in the core.
async function targetOf(page, selector, { timeout = TIMEOUT, dx, dy } = {}) {
  const l = loc(page, selector);
  await l.waitFor({ state: "visible", timeout });
  await l.scrollIntoViewIfNeeded({ timeout });
  const box = await l.boundingBox({ timeout });
  if (!box) throw new Error(`element is not rendered, so it has no coordinates: ${selector}`);
  return {
    x: Math.round(box.x + (typeof dx === "number" ? dx : box.width / 2)),
    y: Math.round(box.y + (typeof dy === "number" ? dy : box.height / 2)),
    width: Math.round(box.width),
    height: Math.round(box.height),
  };
}

// Either a selector or an explicit point, resolved the same way.
async function pointOf(page, { selector, x, y, offset_x, offset_y }) {
  if (selector) {
    return targetOf(page, selector, { dx: offset_x, dy: offset_y });
  }
  if (typeof x !== "number" || typeof y !== "number") {
    throw new Error("give either a selector or both x and y");
  }
  return { x: Math.round(x), y: Math.round(y), width: 32, height: 32 };
}

async function glide(m, page, target) {
  await ensurePointer(m, page);
  const r = await m.session.send("Motion.glideTo", {
    x: target.x,
    y: target.y,
    targetWidth: target.width,
  });
  return r?.durationMs ?? 0;
}

// ---------- helpers ----------

const text = (v) => ({
  content: [{ type: "text", text: typeof v === "string" ? v : JSON.stringify(v, null, 2) }],
});

function profileSummary(profile, match) {
  return {
    id: profile.id,
    name: profile.name,
    folder: profile.folder,
    notes: profile.notes,
    running: !!profile.running,
    cdp: profile.cdp,
    ...(match ? { match } : {}),
  };
}

function matchProfile(profile, query, exact = false) {
  const q = query.trim().toLowerCase();
  if (!q) return null;
  const id = String(profile.id || "");
  const name = String(profile.name || "");
  const lower = name.toLowerCase();
  if (id === query) return "id";
  if (exact ? lower === q : lower.includes(q)) return exact ? "name_exact" : "name";
  return null;
}

async function findProfiles(query, { exact = false, limit = 10 } = {}) {
  const profiles = await api("/profiles");
  const matches = [];
  for (const profile of profiles) {
    const match = matchProfile(profile, query, exact);
    if (match) matches.push(profileSummary(profile, match));
    if (matches.length >= limit) break;
  }
  return matches;
}

async function resolveProfile({ profile_id, profile_query, exact = false }) {
  if (profile_id) return { id: profile_id, match: "id" };
  const matches = await findProfiles(profile_query || "", { exact, limit: 2 });
  if (matches.length !== 1) {
    throw new Error(`expected exactly 1 profile match, got ${matches.length}`);
  }
  return matches[0];
}

function assertHttpUrl(url) {
  const parsed = new URL(url);
  if (!["http:", "https:"].includes(parsed.protocol)) {
    throw new Error("safe_open_url only supports http(s) URLs");
  }
  return parsed.href;
}

const server = new McpServer({ name: "shardx", version: "0.1.0" });

// ================= API tools =================

server.tool(
  "health_check",
  "Check that the ShardX Launcher API is reachable and, when SHARDX_TOKEN is set, authenticated.",
  {},
  async () => {
    const health = await api("/health");
    if (!TOKEN) {
      return text({
        ok: false,
        api: API,
        launcher: health,
        token_present: false,
        authenticated: false,
      });
    }
    try {
      const [profiles, running] = await Promise.all([api("/profiles"), api("/running")]);
      return text({
        ok: true,
        api: API,
        launcher: health,
        token_present: true,
        authenticated: true,
        profiles_count: profiles.length,
        running_count: running.length,
      });
    } catch (error) {
      return text({
        ok: false,
        api: API,
        launcher: health,
        token_present: true,
        authenticated: false,
        error: String(error?.message || error),
      });
    }
  },
);

server.tool(
  "list_profiles",
  "List persistent profiles with their running state and CDP endpoint.",
  {},
  async () => text(await api("/profiles")),
);

server.tool(
  "find_profile_by_name",
  "Find profiles by id or profile name. Returns safe profile summaries, not full fingerprint config.",
  {
    query: z.string(),
    exact: z.boolean().optional(),
    limit: z.number().int().positive().max(50).optional(),
  },
  async ({ query, exact, limit }) =>
    text(await findProfiles(query, { exact: !!exact, limit: limit || 10 })),
);

server.tool(
  "ensure_profile_started",
  "Start a profile only if needed and return its CDP endpoint. Accepts a profile id or a unique profile name/query.",
  {
    profile_id: z.string().optional(),
    profile_query: z.string().optional(),
    exact: z.boolean().optional(),
    headless: z.boolean().optional(),
  },
  async ({ profile_id, profile_query, exact, headless }) => {
    const profile = await resolveProfile({ profile_id, profile_query, exact });
    const running = await api("/running");
    let entry = running.find((r) => r.profile_id === profile.id);
    if (!entry?.cdp) {
      const started = await api(`/profiles/${profile.id}/start`, {
        method: "POST",
        body: { headless: !!headless },
      });
      entry = { profile_id: profile.id, cdp: started.cdp, started: true };
    }
    return text({
      profile: profileSummary(profile),
      running: true,
      started: !!entry.started,
      cdp: entry.cdp,
    });
  },
);

server.tool(
  "safe_open_url",
  "Resolve/start a profile, open an http(s) URL, wait for DOM content, and return title/url.",
  {
    profile_id: z.string().optional(),
    profile_query: z.string().optional(),
    exact: z.boolean().optional(),
    url: z.string(),
    headless: z.boolean().optional(),
  },
  async ({ profile_id, profile_query, exact, url, headless }) => {
    const profile = await resolveProfile({ profile_id, profile_query, exact });
    const page = await pageFor(profile.id, { headless: !!headless });
    await page.goto(assertHttpUrl(url), { waitUntil: "domcontentloaded", timeout: 60000 });
    return text({ profile: profileSummary(profile), url: page.url(), title: await page.title() });
  },
);

server.tool(
  "get_profile",
  "Get a profile's full stored config by id.",
  { id: z.string() },
  async ({ id }) => text(await api(`/profiles/${id}`)),
);

server.tool(
  "new_fingerprint",
  "Generate a fresh uniquified fingerprint (random platform_version, host-aware CPU/RAM, clamped screen). Not persisted.",
  { platform: z.enum(["Windows", "macOS", "Linux"]).optional() },
  async ({ platform }) =>
    text(await api(platform ? `/fingerprint/new/${platform}` : "/fingerprint/new")),
);

server.tool(
  "create_profile",
  "Create a persistent profile. If `fingerprint` is omitted, a new one is generated for `platform` (or the host OS). `proxy` is a string added to the store; `folder` files it.",
  {
    name: z.string().optional(),
    notes: z.string().optional(),
    folder: z.string().optional(),
    proxy: z.string().optional(),
    proxy_id: z.string().optional(),
    platform: z.enum(["Windows", "macOS", "Linux"]).optional(),
    // Icon and omnibox-pill accent. Omit to derive it from the name.
    color: z.string().optional(),
    // Extension-library ids; see list_extensions.
    extensions: z.array(z.string()).optional(),
    fingerprint: z.any().optional(),
  },
  async ({ name, notes, folder, proxy, proxy_id, platform, color, extensions, fingerprint }) => {
    if (!fingerprint) {
      const fp = await api(platform ? `/fingerprint/new/${platform}` : "/fingerprint/new");
      fingerprint = fp.fingerprint;
    }
    const path = folder ? `/folders/${encodeURIComponent(folder)}/profiles` : "/profiles";
    const body = { name, notes, proxy, proxy_id, color, extensions, fingerprint };
    if (folder) delete body.folder; // folder comes from the path
    return text(await api(path, { method: "POST", body }));
  },
);

server.tool(
  "create_temporary_profile",
  "Create a TEMPORARY profile (hidden from the list, auto-deleted on close). Random/specified fingerprint, optional inline proxy string and noise.",
  {
    fingerprint_id: z.string().optional(),
    platform: z.enum(["Windows", "macOS", "Linux"]).optional(),
    proxy: z.string().optional(),
    name: z.string().optional(),
    folder: z.string().optional(),
    // `{canvas: true}` or a full block; omitted vectors stay off.
    noise: z
      .record(
        z.enum(["canvas", "webgl", "audio", "client_rects", "sensors", "fonts"]),
        z.union([
          z.boolean(),
          z.object({
            enabled: z.boolean().optional(),
            seed: z.number().int().optional(),
            intensity: z.number().optional(),
            max_offset: z.number().optional(),
          }),
        ]),
      )
      .optional(),
  },
  async (args) => text(await api("/profiles/temporary", { method: "POST", body: args })),
);

server.tool(
  "edit_profile",
  "Edit a profile. Only provided fields change; `fingerprint` replaces it verbatim; folder:'' unfiles; proxy_id:'' unbinds; color:'' goes back to the name-derived one; `extensions` replaces the whole list.",
  {
    id: z.string(),
    name: z.string().optional(),
    notes: z.string().optional(),
    folder: z.string().optional(),
    proxy_id: z.string().optional(),
    proxy: z.string().optional(),
    color: z.string().optional(),
    extensions: z.array(z.string()).optional(),
    fingerprint: z.any().optional(),
  },
  async ({ id, ...body }) => text(await api(`/profiles/${id}`, { method: "PATCH", body })),
);

server.tool(
  "delete_profile",
  "Move a profile to the trash, restorable for 7 days (see list_trash / restore_profile).",
  { id: z.string() },
  async ({ id }) => text(await api(`/profiles/${id}`, { method: "DELETE" })),
);

server.tool(
  "start_profile",
  "Launch a profile with CDP. Returns { pid, cdp:{ web_socket_debugger_url, http_url } }. Set headless to run without a window.",
  { id: z.string(), headless: z.boolean().optional() },
  async ({ id, headless }) =>
    text(await api(`/profiles/${id}/start`, { method: "POST", body: { headless: !!headless } })),
);

server.tool(
  "stop_profile",
  "Stop a profile's browser (graceful).",
  { id: z.string() },
  async ({ id }) => {
    const b = browsers.get(id);
    if (b) { try { await b.close(); } catch {} browsers.delete(id); }
    motion.delete(id);
    return text(await api(`/profiles/${id}/stop`, { method: "POST" }));
  },
);

server.tool(
  "list_running",
  "List running profiles with pid and CDP endpoint.",
  {},
  async () => text(await api("/running")),
);

server.tool("list_fingerprints", "List the fingerprint library entries.", {}, async () =>
  text(await api("/fingerprints")),
);

server.tool("list_folders", "List folder tags.", {}, async () => text(await api("/folders")));

server.tool(
  "rename_folder",
  "Rename a folder (retags its profiles).",
  { folder: z.string(), name: z.string() },
  async ({ folder, name }) =>
    text(await api(`/folders/${encodeURIComponent(folder)}`, { method: "PATCH", body: { name } })),
);

server.tool(
  "delete_folder",
  "Delete a folder. delete_profiles=true removes its profiles; false unfiles them.",
  { folder: z.string(), delete_profiles: z.boolean().optional() },
  async ({ folder, delete_profiles }) =>
    text(
      await api(
        `/folders/${encodeURIComponent(folder)}?delete_profiles=${delete_profiles ? "true" : "false"}`,
        { method: "DELETE" },
      ),
    ),
);

server.tool("list_proxies", "List stored proxies (no credentials).", {}, async () =>
  text(await api("/proxies")),
);

server.tool(
  "add_proxy",
  "Add a proxy to the store. Pass `proxy` as a string (scheme://user:pass@host:port) or explicit fields.",
  {
    proxy: z.string().optional(),
    kind: z.enum(["socks5", "http", "https"]).optional(),
    host: z.string().optional(),
    port: z.number().optional(),
    username: z.string().optional(),
    password: z.string().optional(),
    name: z.string().optional(),
    country: z.string().optional(),
    notes: z.string().optional(),
  },
  async (args) => text(await api("/proxies", { method: "POST", body: args })),
);

server.tool(
  "delete_proxy",
  "Delete a stored proxy by id.",
  { id: z.string() },
  async ({ id }) => text(await api(`/proxies/${id}`, { method: "DELETE" })),
);

// ---- extensions ----

server.tool(
  "list_extensions",
  "Extensions in the library, with ids to pass to create_profile / edit_profile.",
  {},
  async () => text(await api("/extensions")),
);

server.tool(
  "add_extension",
  "Add an extension. `url` takes a Web Store page, a bare extension id, or a direct .crx/.zip link — the launcher downloads it. `path` takes a local file or unpacked folder.",
  { url: z.string().optional(), path: z.string().optional() },
  async (args) => text(await api("/extensions", { method: "POST", body: args })),
);

server.tool(
  "delete_extension",
  "Remove an extension from the library. Profiles that named it stop loading it on their next start.",
  { id: z.string() },
  async ({ id }) => text(await api(`/extensions/${id}`, { method: "DELETE" })),
);

// ---- bookmarks ----

server.tool(
  "list_bookmarks",
  "Folder-scoped bookmarks pushed into profiles.",
  {},
  async () => text(await api("/bookmarks")),
);

server.tool(
  "save_bookmark",
  "Add or update a bookmark. Bound to a folder it reaches every profile in it; folder '' means every profile. Applied on each profile's next launch.",
  {
    id: z.string().optional(),
    url: z.string(),
    title: z.string().optional(),
    folder: z.string().optional(),
  },
  async (args) => text(await api("/bookmarks", { method: "POST", body: args })),
);

server.tool(
  "delete_bookmark",
  "Delete a bookmark; it leaves its profiles on their next launch.",
  { id: z.string() },
  async ({ id }) => text(await api(`/bookmarks/${id}`, { method: "DELETE" })),
);

// ---- trash ----

server.tool(
  "list_trash",
  "Deleted profiles still restorable, with the day each one expires.",
  {},
  async () => text(await api("/trash")),
);

server.tool(
  "restore_profile",
  "Bring a deleted profile back under its own id, with its cookies and logins.",
  { id: z.string() },
  async ({ id }) => text(await api(`/trash/${id}/restore`, { method: "POST" })),
);

server.tool(
  "purge_profile",
  "Delete a trashed profile for good. There is nothing after this.",
  { id: z.string() },
  async ({ id }) => text(await api(`/trash/${id}`, { method: "DELETE" })),
);

server.tool(
  "export_cookies",
  "Export a profile's cookies (decrypted).",
  { id: z.string() },
  async ({ id }) => text(await api(`/profiles/${id}/cookies`)),
);

server.tool(
  "import_cookies",
  "Import cookies into a STOPPED profile.",
  { id: z.string(), cookies: z.array(z.any()) },
  async ({ id, cookies }) =>
    text(await api(`/profiles/${id}/cookies`, { method: "POST", body: { cookies } })),
);

// ================= Automation projects =================
// A project drives its own browsers through the Motion domain, not CDP, so these
// work whether or not a profile is running.

server.tool(
  "list_automation_projects",
  "List automation projects with their blocks and run settings.",
  {},
  async () => text(await api("/automation/projects")),
);

server.tool(
  "get_automation_project",
  "Get one automation project by id.",
  { id: z.string() },
  async ({ id }) => text(await api(`/automation/projects/${id}`)),
);

server.tool(
  "create_automation_project",
  "Create an empty automation project. Add steps with save_automation_project.",
  { name: z.string().optional() },
  async ({ name }) =>
    text(await api("/automation/projects", { method: "POST", body: { name } })),
);

server.tool(
  "save_automation_project",
  "Replace a project whole — send it back the way get_automation_project returned it, with `blocks` and `run` edited. The id in the path wins.",
  { id: z.string(), project: z.any() },
  async ({ id, project }) =>
    text(await api(`/automation/projects/${id}`, { method: "PUT", body: project })),
);

server.tool(
  "delete_automation_project",
  "Delete an automation project. This one is not a trash — it is gone.",
  { id: z.string() },
  async ({ id }) => text(await api(`/automation/projects/${id}`, { method: "DELETE" })),
);

server.tool(
  "duplicate_automation_project",
  "Copy an automation project.",
  { id: z.string() },
  async ({ id }) =>
    text(await api(`/automation/projects/${id}/duplicate`, { method: "POST" })),
);

server.tool(
  "export_automation_project",
  "Export a project as a bundle. Any module its steps call travels inside the bundle. Parameters the project marked secret come out empty, and `needs` says which ones.",
  { id: z.string() },
  async ({ id }) => text(await api(`/automation/projects/${id}/export`)),
);

server.tool(
  "import_automation_project",
  "Import a project bundle; it arrives as a new project with a new id. Modules carried in the bundle are installed first, except where one is already installed under the same id \u2014 that one is kept.",
  { bundle: z.any() },
  async ({ bundle }) =>
    text(await api("/automation/import", { method: "POST", body: bundle })),
);

server.tool(
  "run_automation_project",
  "Start a project. Answers as soon as the run is under way — the browsers it needs come from its own profile blocks. Poll automation_status for progress.",
  { id: z.string() },
  async ({ id }) => text(await api(`/automation/projects/${id}/run`, { method: "POST" })),
);

server.tool(
  "stop_automation_project",
  "Ask a run to stop. Each browser finishes the step it is in and then closes, so the run does not end the instant this answers.",
  { id: z.string() },
  async ({ id }) => text(await api(`/automation/projects/${id}/stop`, { method: "POST" })),
);

server.tool(
  "automation_status",
  "How a run is going: each worker's pass, step and status, plus the tail of the log. Null when the project is not running.",
  { id: z.string() },
  async ({ id }) => text(await api(`/automation/projects/${id}/status`)),
);

server.tool(
  "list_automation_runs",
  "Every automation run going right now.",
  {},
  async () => text(await api("/automation/runs")),
);

server.tool(
  "list_automation_modules",
  "List installed WebAssembly modules. Each contributes blocks under the kind `module:<module id>:<block>`.",
  {},
  async () => text(await api("/automation/modules")),
);

server.tool(
  "install_automation_module",
  "Install a .wasm module from a path on this machine.",
  { path: z.string() },
  async ({ path }) =>
    text(await api("/automation/modules", { method: "POST", body: { path } })),
);

server.tool(
  "remove_automation_module",
  "Remove a module. Projects using its blocks stop working; nothing rewrites them.",
  { id: z.string() },
  async ({ id }) => text(await api(`/automation/modules/${id}`, { method: "DELETE" })),
);

// ================= CDP browser tools (patchright) =================

server.tool(
  "browser_navigate",
  "Open a URL in the profile's browser (starts it with CDP if needed). Set headless to launch without a window.",
  { profile_id: z.string(), url: z.string(), headless: z.boolean().optional() },
  async ({ profile_id, url, headless }) => {
    const page = await pageFor(profile_id, { headless: !!headless });
    await page.goto(url, { waitUntil: "domcontentloaded", timeout: 60000 });
    return text({ url: page.url(), title: await page.title() });
  },
);

server.tool(
  "browser_evaluate",
  "Run a JavaScript expression in the active page and return the result.",
  { profile_id: z.string(), expression: z.string() },
  async ({ profile_id, expression }) => {
    const page = await pageFor(profile_id);
    const result = await page.evaluate(expression);
    return text(result === undefined ? "undefined" : result);
  },
);

server.tool(
  "browser_content",
  "Return the active page's full HTML.",
  { profile_id: z.string() },
  async ({ profile_id }) => text(await (await pageFor(profile_id)).content()),
);

server.tool(
  "browser_screenshot",
  "Screenshot the active page (PNG).",
  { profile_id: z.string(), full_page: z.boolean().optional() },
  async ({ profile_id, full_page }) => {
    const page = await pageFor(profile_id);
    const buf = await page.screenshot({ fullPage: !!full_page });
    return { content: [{ type: "image", data: buf.toString("base64"), mimeType: "image/png" }] };
  },
);

server.tool(
  "browser_click",
  "Click the first element matching a CSS selector.",
  { profile_id: z.string(), selector: z.string() },
  async ({ profile_id, selector }) => {
    const page = await pageFor(profile_id);
    await page.click(selector, { timeout: 15000 });
    return text(`clicked ${selector}`);
  },
);

server.tool(
  "browser_fill",
  "Fill an input/textarea matching a CSS selector with text.",
  { profile_id: z.string(), selector: z.string(), text: z.string() },
  async ({ profile_id, selector, text: value }) => {
    const page = await pageFor(profile_id);
    await page.fill(selector, value, { timeout: 15000 });
    return text(`filled ${selector}`);
  },
);

server.tool(
  "browser_current_url",
  "Return the active page's current URL and title.",
  { profile_id: z.string() },
  async ({ profile_id }) => {
    const page = await pageFor(profile_id);
    return text({ url: page.url(), title: await page.title() });
  },
);

// ---- navigation ----

server.tool("browser_back", "Go back in history.", { profile_id: z.string() }, async ({ profile_id }) => {
  const page = await pageFor(profile_id);
  await page.goBack({ waitUntil: "domcontentloaded" }).catch(() => {});
  return text({ url: page.url() });
});

server.tool("browser_forward", "Go forward in history.", { profile_id: z.string() }, async ({ profile_id }) => {
  const page = await pageFor(profile_id);
  await page.goForward({ waitUntil: "domcontentloaded" }).catch(() => {});
  return text({ url: page.url() });
});

server.tool(
  "browser_reload",
  "Reload the active page.",
  { profile_id: z.string() },
  async ({ profile_id }) => {
    const page = await pageFor(profile_id);
    await page.reload({ waitUntil: "domcontentloaded" });
    return text({ url: page.url() });
  },
);

// ---- waiting ----

server.tool(
  "browser_wait_for_selector",
  "Wait until an element matching the selector reaches a state.",
  {
    profile_id: z.string(),
    selector: z.string(),
    state: z.enum(["attached", "detached", "visible", "hidden"]).optional(),
    timeout_ms: z.number().optional(),
  },
  async ({ profile_id, selector, state, timeout_ms }) => {
    const page = await pageFor(profile_id);
    await page.waitForSelector(selector, { state: state ?? "visible", timeout: timeout_ms ?? 30000 });
    return text(`ready: ${selector}`);
  },
);

server.tool(
  "browser_wait_for_load",
  "Wait for a page load state (load | domcontentloaded | networkidle).",
  { profile_id: z.string(), state: z.enum(["load", "domcontentloaded", "networkidle"]).optional() },
  async ({ profile_id, state }) => {
    const page = await pageFor(profile_id);
    await page.waitForLoadState(state ?? "load");
    return text(`load state: ${state ?? "load"}`);
  },
);

server.tool(
  "browser_wait",
  "Wait a fixed number of milliseconds.",
  { profile_id: z.string(), ms: z.number() },
  async ({ profile_id, ms }) => {
    const page = await pageFor(profile_id);
    await page.waitForTimeout(ms);
    return text(`waited ${ms}ms`);
  },
);

// ---- reading ----

server.tool(
  "browser_get_text",
  "Return innerText of the first element matching the selector.",
  { profile_id: z.string(), selector: z.string() },
  async ({ profile_id, selector }) => {
    const page = await pageFor(profile_id);
    return text(await loc(page, selector).innerText({ timeout: TIMEOUT }));
  },
);

server.tool(
  "browser_get_attribute",
  "Return an attribute of the first element matching the selector.",
  { profile_id: z.string(), selector: z.string(), name: z.string() },
  async ({ profile_id, selector, name }) => {
    const page = await pageFor(profile_id);
    const v = await loc(page, selector).getAttribute(name, { timeout: TIMEOUT });
    return text(v ?? "null");
  },
);

server.tool(
  "browser_get_html",
  "Return outerHTML of a selector (or the whole document when omitted).",
  { profile_id: z.string(), selector: z.string().optional() },
  async ({ profile_id, selector }) => {
    const page = await pageFor(profile_id);
    if (!selector) return text(await page.content());
    return text(await loc(page, selector).evaluate((el) => el.outerHTML));
  },
);

server.tool(
  "browser_exists",
  "Whether at least one element matches the selector.",
  { profile_id: z.string(), selector: z.string() },
  async ({ profile_id, selector }) => {
    const page = await pageFor(profile_id);
    return text({ exists: (await page.locator(selector).count()) > 0 });
  },
);

server.tool(
  "browser_count",
  "Count elements matching the selector.",
  { profile_id: z.string(), selector: z.string() },
  async ({ profile_id, selector }) => {
    const page = await pageFor(profile_id);
    return text({ count: await page.locator(selector).count() });
  },
);

server.tool(
  "browser_links",
  "List anchor links on the page as { text, href }.",
  { profile_id: z.string() },
  async ({ profile_id }) => {
    const page = await pageFor(profile_id);
    return text(
      await page.evaluate(() =>
        Array.from(document.querySelectorAll("a[href]"))
          .map((a) => ({ text: a.innerText.trim().slice(0, 120), href: a.href }))
          .filter((l) => l.href),
      ),
    );
  },
);

// ---- interaction ----

server.tool(
  "browser_type",
  "Type text into an element key-by-key (good for inputs that watch keystrokes).",
  { profile_id: z.string(), selector: z.string(), text: z.string(), delay_ms: z.number().optional() },
  async ({ profile_id, selector, text: value, delay_ms }) => {
    const page = await pageFor(profile_id);
    await loc(page, selector).pressSequentially(value, { delay: delay_ms ?? 20, timeout: TIMEOUT });
    return text(`typed into ${selector}`);
  },
);

server.tool(
  "browser_press",
  "Press a keyboard key on the active page (e.g. Enter, Escape, Control+A, ArrowDown).",
  { profile_id: z.string(), key: z.string() },
  async ({ profile_id, key }) => {
    const page = await pageFor(profile_id);
    await page.keyboard.press(key);
    return text(`pressed ${key}`);
  },
);

server.tool(
  "browser_hover",
  "Hover the first element matching the selector.",
  { profile_id: z.string(), selector: z.string() },
  async ({ profile_id, selector }) => {
    const page = await pageFor(profile_id);
    await loc(page, selector).hover({ timeout: TIMEOUT });
    return text(`hovered ${selector}`);
  },
);

server.tool(
  "browser_select_option",
  "Select an option in a <select> by value (or label).",
  { profile_id: z.string(), selector: z.string(), value: z.string(), by: z.enum(["value", "label"]).optional() },
  async ({ profile_id, selector, value, by }) => {
    const page = await pageFor(profile_id);
    const arg = by === "label" ? { label: value } : { value };
    const picked = await loc(page, selector).selectOption(arg, { timeout: TIMEOUT });
    return text({ selected: picked });
  },
);

server.tool(
  "browser_set_checkbox",
  "Check or uncheck a checkbox/radio.",
  { profile_id: z.string(), selector: z.string(), checked: z.boolean() },
  async ({ profile_id, selector, checked }) => {
    const page = await pageFor(profile_id);
    await loc(page, selector).setChecked(checked, { timeout: TIMEOUT });
    return text(`${checked ? "checked" : "unchecked"} ${selector}`);
  },
);

server.tool(
  "browser_focus",
  "Focus the first element matching the selector.",
  { profile_id: z.string(), selector: z.string() },
  async ({ profile_id, selector }) => {
    const page = await pageFor(profile_id);
    await loc(page, selector).focus({ timeout: TIMEOUT });
    return text(`focused ${selector}`);
  },
);

server.tool(
  "browser_scroll",
  "Scroll: to an element (selector) or by a pixel delta (dy / dx).",
  { profile_id: z.string(), selector: z.string().optional(), dy: z.number().optional(), dx: z.number().optional() },
  async ({ profile_id, selector, dy, dx }) => {
    const page = await pageFor(profile_id);
    if (selector) {
      await loc(page, selector).scrollIntoViewIfNeeded({ timeout: TIMEOUT });
      return text(`scrolled to ${selector}`);
    }
    await page.mouse.wheel(dx ?? 0, dy ?? 600);
    return text(`scrolled by (${dx ?? 0}, ${dy ?? 600})`);
  },
);

server.tool(
  "browser_set_files",
  "Set files on a file <input> (upload).",
  { profile_id: z.string(), selector: z.string(), paths: z.array(z.string()) },
  async ({ profile_id, selector, paths }) => {
    const page = await pageFor(profile_id);
    await loc(page, selector).setInputFiles(paths, { timeout: TIMEOUT });
    return text(`set ${paths.length} file(s) on ${selector}`);
  },
);

server.tool(
  "browser_element_screenshot",
  "Screenshot a single element (PNG).",
  { profile_id: z.string(), selector: z.string() },
  async ({ profile_id, selector }) => {
    const page = await pageFor(profile_id);
    const buf = await loc(page, selector).screenshot({ timeout: TIMEOUT });
    return { content: [{ type: "image", data: buf.toString("base64"), mimeType: "image/png" }] };
  },
);

server.tool(
  "browser_set_viewport",
  "Set the page viewport size.",
  { profile_id: z.string(), width: z.number(), height: z.number() },
  async ({ profile_id, width, height }) => {
    const page = await pageFor(profile_id);
    await page.setViewportSize({ width, height });
    return text({ width, height });
  },
);

server.tool(
  "browser_pdf",
  "Render the active page to PDF (headless Chromium only). Returns base64.",
  { profile_id: z.string() },
  async ({ profile_id }) => {
    const page = await pageFor(profile_id);
    const buf = await page.pdf({ printBackground: true });
    return { content: [{ type: "text", text: buf.toString("base64") }] };
  },
);

server.tool(
  "browser_get_cookies",
  "Return the browser context's cookies (live, from the running browser).",
  { profile_id: z.string() },
  async ({ profile_id }) => text(await (await contextFor(profile_id)).cookies()),
);

// ---- tabs ----

server.tool(
  "browser_list_tabs",
  "List open tabs as { index, url, title, active }.",
  { profile_id: z.string() },
  async ({ profile_id }) => {
    const ctx = await contextFor(profile_id);
    const cur = activePage.get(profile_id);
    const pages = ctx.pages();
    const out = [];
    for (let i = 0; i < pages.length; i++) {
      out.push({ index: i, url: pages[i].url(), title: await pages[i].title().catch(() => ""), active: pages[i] === cur });
    }
    return text(out);
  },
);

server.tool(
  "browser_open_tab",
  "Open a new tab (optionally navigating to a URL) and make it active.",
  { profile_id: z.string(), url: z.string().optional() },
  async ({ profile_id, url }) => {
    const ctx = await contextFor(profile_id);
    const page = await ctx.newPage();
    if (url) await page.goto(url, { waitUntil: "domcontentloaded", timeout: 60000 });
    activePage.set(profile_id, page);
    return text({ url: page.url(), title: await page.title() });
  },
);

server.tool(
  "browser_switch_tab",
  "Make the tab at `index` (from browser_list_tabs) the active one.",
  { profile_id: z.string(), index: z.number() },
  async ({ profile_id, index }) => {
    const ctx = await contextFor(profile_id);
    const page = ctx.pages()[index];
    if (!page) throw new Error(`no tab at index ${index}`);
    await page.bringToFront().catch(() => {});
    activePage.set(profile_id, page);
    return text({ url: page.url(), title: await page.title() });
  },
);

server.tool(
  "browser_close_tab",
  "Close a tab by index (defaults to the active tab).",
  { profile_id: z.string(), index: z.number().optional() },
  async ({ profile_id, index }) => {
    const ctx = await contextFor(profile_id);
    const pages = ctx.pages();
    const page = index === undefined ? activePage.get(profile_id) : pages[index];
    if (!page) throw new Error(`no tab to close`);
    await page.close();
    activePage.delete(profile_id);
    return text(`closed tab`);
  },
);

// ---- more reading ----

server.tool(
  "browser_text",
  "Return the page's visible text (document.body.innerText) — cheap way to read content.",
  { profile_id: z.string() },
  async ({ profile_id }) => {
    const page = await pageFor(profile_id);
    return text(await page.evaluate(() => document.body?.innerText ?? ""));
  },
);

server.tool(
  "browser_element_state",
  "Element state: count, visible, enabled, checked.",
  { profile_id: z.string(), selector: z.string() },
  async ({ profile_id, selector }) => {
    const page = await pageFor(profile_id);
    const l = page.locator(selector).first();
    const count = await page.locator(selector).count();
    if (count === 0) return text({ count: 0, visible: false, enabled: false, checked: false });
    return text({
      count,
      visible: await l.isVisible().catch(() => false),
      enabled: await l.isEnabled().catch(() => false),
      checked: await l.isChecked().catch(() => false),
    });
  },
);

server.tool(
  "browser_bounding_box",
  "Bounding box {x,y,width,height} of an element (or null if not visible).",
  { profile_id: z.string(), selector: z.string() },
  async ({ profile_id, selector }) => {
    const page = await pageFor(profile_id);
    return text(await loc(page, selector).boundingBox());
  },
);

// ---- more waiting ----

server.tool(
  "browser_wait_for_url",
  "Wait until the page URL matches (glob/substring).",
  { profile_id: z.string(), url: z.string(), timeout_ms: z.number().optional() },
  async ({ profile_id, url, timeout_ms }) => {
    const page = await pageFor(profile_id);
    await page.waitForURL(url, { timeout: timeout_ms ?? 30000 });
    return text({ url: page.url() });
  },
);

server.tool(
  "browser_wait_for_function",
  "Wait until a JS expression evaluates truthy in the page.",
  { profile_id: z.string(), expression: z.string(), timeout_ms: z.number().optional() },
  async ({ profile_id, expression, timeout_ms }) => {
    const page = await pageFor(profile_id);
    await page.waitForFunction(expression, undefined, { timeout: timeout_ms ?? 30000 });
    return text("condition met");
  },
);

// ---- more interaction ----

server.tool(
  "browser_double_click",
  "Double-click an element.",
  { profile_id: z.string(), selector: z.string() },
  async ({ profile_id, selector }) => {
    await loc(await pageFor(profile_id), selector).dblclick({ timeout: TIMEOUT });
    return text(`double-clicked ${selector}`);
  },
);

server.tool(
  "browser_right_click",
  "Right-click (context-menu) an element.",
  { profile_id: z.string(), selector: z.string() },
  async ({ profile_id, selector }) => {
    await loc(await pageFor(profile_id), selector).click({ button: "right", timeout: TIMEOUT });
    return text(`right-clicked ${selector}`);
  },
);

server.tool(
  "browser_drag",
  "Drag one element onto another.",
  { profile_id: z.string(), from: z.string(), to: z.string() },
  async ({ profile_id, from, to }) => {
    const page = await pageFor(profile_id);
    await loc(page, from).dragTo(loc(page, to), { timeout: TIMEOUT });
    return text(`dragged ${from} → ${to}`);
  },
);

server.tool(
  "browser_mouse_click",
  "Click at absolute viewport coordinates (for canvas/maps).",
  { profile_id: z.string(), x: z.number(), y: z.number() },
  async ({ profile_id, x, y }) => {
    await (await pageFor(profile_id)).mouse.click(x, y);
    return text(`clicked at (${x}, ${y})`);
  },
);

server.tool(
  "browser_scroll_to_bottom",
  "Scroll to the bottom of the page (triggers lazy/infinite load).",
  { profile_id: z.string() },
  async ({ profile_id }) => {
    const page = await pageFor(profile_id);
    await page.evaluate(() => window.scrollTo(0, document.body.scrollHeight));
    return text("scrolled to bottom");
  },
);

// ---- storage / network ----

server.tool(
  "browser_set_cookies",
  "Add cookies to the browser context (Playwright format: name, value, and domain+path or url).",
  { profile_id: z.string(), cookies: z.array(z.any()) },
  async ({ profile_id, cookies }) => {
    await (await contextFor(profile_id)).addCookies(cookies);
    return text(`added ${cookies.length} cookie(s)`);
  },
);

server.tool(
  "browser_clear_cookies",
  "Clear all cookies in the browser context.",
  { profile_id: z.string() },
  async ({ profile_id }) => {
    await (await contextFor(profile_id)).clearCookies();
    return text("cookies cleared");
  },
);

server.tool(
  "browser_local_storage",
  "Read/write the page's localStorage. action: get | set | remove | clear.",
  {
    profile_id: z.string(),
    action: z.enum(["get", "set", "remove", "clear"]),
    key: z.string().optional(),
    value: z.string().optional(),
  },
  async ({ profile_id, action, key, value }) => {
    const page = await pageFor(profile_id);
    const r = await page.evaluate(
      ({ action, key, value }) => {
        if (action === "get") {
          if (key) return localStorage.getItem(key);
          return Object.fromEntries(Object.keys(localStorage).map((k) => [k, localStorage.getItem(k)]));
        }
        if (action === "set") { localStorage.setItem(key, value ?? ""); return "ok"; }
        if (action === "remove") { localStorage.removeItem(key); return "ok"; }
        localStorage.clear();
        return "ok";
      },
      { action, key, value },
    );
    return text(r);
  },
);

server.tool(
  "browser_set_extra_headers",
  "Set extra HTTP headers sent on every request (e.g. Authorization). Empty object clears.",
  { profile_id: z.string(), headers: z.record(z.string()) },
  async ({ profile_id, headers }) => {
    await (await pageFor(profile_id)).setExtraHTTPHeaders(headers);
    return text({ headers: Object.keys(headers) });
  },
);

const dialogHandlers = new Map(); // profile_id → dialog listener

server.tool(
  "browser_dialog",
  "Auto-handle native dialogs (alert/confirm/prompt). action: accept | dismiss | off.",
  { profile_id: z.string(), action: z.enum(["accept", "dismiss", "off"]), prompt_text: z.string().optional() },
  async ({ profile_id, action, prompt_text }) => {
    const page = await pageFor(profile_id);
    const prev = dialogHandlers.get(profile_id);
    if (prev) { page.off("dialog", prev); dialogHandlers.delete(profile_id); }
    if (action !== "off") {
      const handler = async (d) => {
        try { action === "accept" ? await d.accept(prompt_text) : await d.dismiss(); } catch {}
      };
      page.on("dialog", handler);
      dialogHandlers.set(profile_id, handler);
    }
    return text(`dialog handling: ${action}`);
  },
);

server.tool(
  "browser_block_resources",
  "Abort matching resource types for speed (image, media, font, stylesheet, script, …). Empty list unblocks.",
  { profile_id: z.string(), types: z.array(z.string()) },
  async ({ profile_id, types }) => {
    const page = await pageFor(profile_id);
    await page.unroute("**/*").catch(() => {});
    if (types.length) {
      const blocked = new Set(types);
      await page.route("**/*", (route) =>
        blocked.has(route.request().resourceType()) ? route.abort() : route.continue(),
      );
    }
    return text(`blocking: ${types.join(", ") || "none"}`);
  },
);

// ---- frames ----

server.tool(
  "browser_frames",
  "List the page's frames as { index, name, url }.",
  { profile_id: z.string() },
  async ({ profile_id }) => {
    const page = await pageFor(profile_id);
    return text(page.frames().map((f, i) => ({ index: i, name: f.name(), url: f.url() })));
  },
);

server.tool(
  "browser_frame_evaluate",
  "Evaluate JS inside a frame matched by URL substring or name.",
  { profile_id: z.string(), frame: z.string(), expression: z.string() },
  async ({ profile_id, frame, expression }) => {
    const page = await pageFor(profile_id);
    const fr = page.frames().find((f) => f.url().includes(frame) || f.name() === frame);
    if (!fr) throw new Error(`no frame matching "${frame}"`);
    const r = await fr.evaluate(expression);
    return text(r === undefined ? "undefined" : r);
  },
);

// ---- scraping helpers ----

server.tool(
  "browser_get_texts",
  "innerText of ALL elements matching the selector (scrape lists/tables).",
  { profile_id: z.string(), selector: z.string() },
  async ({ profile_id, selector }) =>
    text(await (await pageFor(profile_id)).locator(selector).allInnerTexts()),
);

server.tool(
  "browser_input_value",
  "Current value of an input / textarea / select.",
  { profile_id: z.string(), selector: z.string() },
  async ({ profile_id, selector }) =>
    text(await loc(await pageFor(profile_id), selector).inputValue({ timeout: TIMEOUT })),
);

server.tool(
  "browser_insert_text",
  "Insert text into the focused element (fast; no per-key events).",
  { profile_id: z.string(), text: z.string() },
  async ({ profile_id, text: value }) => {
    await (await pageFor(profile_id)).keyboard.insertText(value);
    return text("inserted");
  },
);

server.tool(
  "browser_aria_snapshot",
  "Accessibility-tree snapshot of the page (or a selector) — a compact, agent-friendly view of the UI.",
  { profile_id: z.string(), selector: z.string().optional() },
  async ({ profile_id, selector }) => {
    const page = await pageFor(profile_id);
    const target = selector ? page.locator(selector).first() : page.locator("body");
    return text(await target.ariaSnapshot());
  },
);

// ---- network: wait / capture / mock ----

server.tool(
  "browser_wait_for_response",
  "Wait for a response whose URL matches (glob/substring); returns { url, status }.",
  { profile_id: z.string(), url_pattern: z.string(), timeout_ms: z.number().optional() },
  async ({ profile_id, url_pattern, timeout_ms }) => {
    const page = await pageFor(profile_id);
    const resp = await page.waitForResponse(url_pattern, { timeout: timeout_ms ?? 30000 });
    return text({ url: resp.url(), status: resp.status() });
  },
);

const captures = new Map(); // profile_id → { handler, log }

server.tool(
  "browser_capture_start",
  "Start logging finished network requests for the profile.",
  { profile_id: z.string() },
  async ({ profile_id }) => {
    const page = await pageFor(profile_id);
    const prev = captures.get(profile_id);
    if (prev) page.off("requestfinished", prev.handler);
    const log = [];
    const handler = async (req) => {
      try {
        const r = await req.response();
        log.push({ method: req.method(), url: req.url(), status: r ? r.status() : null, type: req.resourceType() });
      } catch {}
    };
    page.on("requestfinished", handler);
    captures.set(profile_id, { handler, log });
    return text("capturing network");
  },
);

server.tool(
  "browser_capture_stop",
  "Stop logging and return the captured requests.",
  { profile_id: z.string() },
  async ({ profile_id }) => {
    const page = await pageFor(profile_id);
    const c = captures.get(profile_id);
    if (!c) return text([]);
    page.off("requestfinished", c.handler);
    captures.delete(profile_id);
    return text(c.log);
  },
);

const mocks = new Map(); // profile_id → Map(pattern → handler)

server.tool(
  "browser_mock",
  "Fulfill requests matching a URL glob with a canned response (status/body/content_type).",
  {
    profile_id: z.string(),
    url_pattern: z.string(),
    status: z.number().optional(),
    body: z.string().optional(),
    content_type: z.string().optional(),
  },
  async ({ profile_id, url_pattern, status, body, content_type }) => {
    const page = await pageFor(profile_id);
    const handler = (route) =>
      route.fulfill({
        status: status ?? 200,
        contentType: content_type ?? "application/json",
        body: body ?? "",
      });
    await page.route(url_pattern, handler);
    let m = mocks.get(profile_id);
    if (!m) { m = new Map(); mocks.set(profile_id, m); }
    m.set(url_pattern, handler);
    return text(`mocking ${url_pattern}`);
  },
);

server.tool(
  "browser_unmock",
  "Remove a mock for a pattern (or all mocks when omitted).",
  { profile_id: z.string(), url_pattern: z.string().optional() },
  async ({ profile_id, url_pattern }) => {
    const page = await pageFor(profile_id);
    const m = mocks.get(profile_id);
    if (url_pattern) {
      await page.unroute(url_pattern).catch(() => {});
      m?.delete(url_pattern);
    } else {
      for (const p of m?.keys() ?? []) await page.unroute(p).catch(() => {});
      mocks.delete(profile_id);
    }
    return text("unmocked");
  },
);

// ---- downloads ----

server.tool(
  "browser_wait_for_download",
  "Wait for a download to start, save it into `dir`, and return the saved path.",
  { profile_id: z.string(), dir: z.string(), timeout_ms: z.number().optional() },
  async ({ profile_id, dir, timeout_ms }) => {
    const page = await pageFor(profile_id);
    const dl = await page.waitForEvent("download", { timeout: timeout_ms ?? 60000 });
    const out = `${dir.replace(/[/\\]+$/, "")}/${dl.suggestedFilename()}`;
    await dl.saveAs(out);
    return text({ path: out, url: dl.url() });
  },
);

server.tool(
  "browser_press_on",
  "Press a key while a specific element is focused (e.g. Enter in a search box).",
  { profile_id: z.string(), selector: z.string(), key: z.string() },
  async ({ profile_id, selector, key }) => {
    await loc(await pageFor(profile_id), selector).press(key, { timeout: TIMEOUT });
    return text(`pressed ${key} on ${selector}`);
  },
);

server.tool(
  "browser_intercept",
  "Modify matching requests in flight: override/add request headers, replace POST data, or abort. Remove with browser_unmock.",
  {
    profile_id: z.string(),
    url_pattern: z.string(),
    headers: z.record(z.string()).optional(),
    post_data: z.string().optional(),
    abort: z.boolean().optional(),
  },
  async ({ profile_id, url_pattern, headers, post_data, abort }) => {
    const page = await pageFor(profile_id);
    const handler = (route) => {
      if (abort) return route.abort();
      const overrides = {};
      if (headers) overrides.headers = { ...route.request().headers(), ...headers };
      if (post_data !== undefined) overrides.postData = post_data;
      return route.continue(overrides);
    };
    await page.route(url_pattern, handler);
    let m = mocks.get(profile_id);
    if (!m) { m = new Map(); mocks.set(profile_id, m); }
    m.set(url_pattern, handler);
    return text(`intercepting ${url_pattern}`);
  },
);

server.tool(
  "browser_set_network_conditions",
  "Emulate network via CDP: offline, latency, and throughput (kbps). Omit/false/0 to reset to unlimited.",
  {
    profile_id: z.string(),
    offline: z.boolean().optional(),
    latency_ms: z.number().optional(),
    download_kbps: z.number().optional(),
    upload_kbps: z.number().optional(),
  },
  async ({ profile_id, offline, latency_ms, download_kbps, upload_kbps }) => {
    const page = await pageFor(profile_id);
    const client = await page.context().newCDPSession(page);
    await client.send("Network.enable");
    await client.send("Network.emulateNetworkConditions", {
      offline: !!offline,
      latency: latency_ms ?? 0,
      downloadThroughput: download_kbps ? Math.round((download_kbps * 1024) / 8) : -1,
      uploadThroughput: upload_kbps ? Math.round((upload_kbps * 1024) / 8) : -1,
    });
    await client.detach().catch(() => {});
    return text({ offline: !!offline, latency_ms: latency_ms ?? 0, download_kbps: download_kbps ?? 0, upload_kbps: upload_kbps ?? 0 });
  },
);

// ---- human input (Motion domain) ----
//
// Prefer over browser_click / browser_type where a site watches how input
// arrives. They cost real time, which is the point.

server.tool(
  "human_move",
  "Move the pointer to an element (or a point) along a human trajectory. Give either selector or x+y.",
  {
    profile_id: z.string(),
    selector: z.string().optional(),
    x: z.number().optional(),
    y: z.number().optional(),
    offset_x: z.number().optional(),
    offset_y: z.number().optional(),
  },
  async ({ profile_id, selector, x, y, offset_x, offset_y }) => {
    const page = await pageFor(profile_id);
    const m = await motionFor(profile_id);
    const t = await pointOf(page, { selector, x, y, offset_x, offset_y });
    const ms = await glide(m, page, t);
    return text({ x: t.x, y: t.y, duration_ms: ms });
  },
);

server.tool(
  "human_click",
  "Move to an element (or a point) and click it the way a person does. Give either selector or x+y.",
  {
    profile_id: z.string(),
    selector: z.string().optional(),
    x: z.number().optional(),
    y: z.number().optional(),
    offset_x: z.number().optional(),
    offset_y: z.number().optional(),
    button: z.enum(["left", "middle", "right"]).optional(),
    click_count: z.number().int().min(1).max(3).optional(),
  },
  async ({ profile_id, selector, x, y, offset_x, offset_y, button, click_count }) => {
    const page = await pageFor(profile_id);
    const m = await motionFor(profile_id);
    const t = await pointOf(page, { selector, x, y, offset_x, offset_y });
    const ms = await glide(m, page, t);
    await m.session.send("Motion.tap", {
      button: button ?? "left",
      clickCount: click_count ?? 1,
    });
    return text({ clicked: selector ?? `${t.x},${t.y}`, duration_ms: ms });
  },
);

server.tool(
  "human_type",
  "Type text into whatever currently has focus, key by key with human timing. Use human_fill to focus a field first.",
  {
    profile_id: z.string(),
    text: z.string(),
    allow_typos: z.boolean().optional(),
  },
  async ({ profile_id, text: value, allow_typos }) => {
    const page = await pageFor(profile_id);
    const m = await motionFor(profile_id);
    await ensurePointer(m, page);
    const r = await m.session.send("Motion.enterText", {
      text: value,
      allowTypos: !!allow_typos,
    });
    return text({ typed: value.length, duration_ms: r?.durationMs ?? 0 });
  },
);

server.tool(
  "human_fill",
  "Click a field and type into it, both humanly. The one to reach for on a form.",
  {
    profile_id: z.string(),
    selector: z.string(),
    text: z.string(),
    // Triple-clicks to select first; without it the text is appended.
    clear: z.boolean().optional(),
    allow_typos: z.boolean().optional(),
  },
  async ({ profile_id, selector, text: value, clear, allow_typos }) => {
    const page = await pageFor(profile_id);
    const m = await motionFor(profile_id);
    const t = await targetOf(page, selector);
    const moved = await glide(m, page, t);
    await m.session.send("Motion.tap", { button: "left", clickCount: clear ? 3 : 1 });
    const r = await m.session.send("Motion.enterText", {
      text: value,
      allowTypos: !!allow_typos,
    });
    return text({
      filled: selector,
      at: `${t.x},${t.y}`,
      move_ms: moved,
      type_ms: r?.durationMs ?? 0,
    });
  },
);

server.tool(
  "human_release_pointer",
  "Drop this profile's pointer. Rarely needed — the next human_* call makes a new one.",
  { profile_id: z.string() },
  async ({ profile_id }) => {
    const m = motion.get(profile_id);
    if (m?.pointer) {
      await m.session.send("Motion.destroyPointer").catch(() => {});
      m.pointer = false;
    }
    return text("pointer released");
  },
);

// ---------- run ----------
//
// Two transports:
//   * stdio (default) — the MCP client spawns this process and talks over
//     stdin/stdout.  Standard, works with any client.
//   * HTTP (when MCP_HTTP_PORT is set) — listens on 127.0.0.1:<port>/mcp so
//     the ShardX app can host it as a managed child and clients connect by
//     URL.  Used by the launcher's "embed MCP" option.

const httpPort = process.env.MCP_HTTP_PORT ? Number(process.env.MCP_HTTP_PORT) : 0;

if (httpPort) {
  const { StreamableHTTPServerTransport } = await import(
    "@modelcontextprotocol/sdk/server/streamableHttp.js"
  );
  const { randomUUID } = await import("node:crypto");
  const http = await import("node:http");

  const transport = new StreamableHTTPServerTransport({
    sessionIdGenerator: () => randomUUID(),
  });
  await server.connect(transport);

  http
    .createServer((req, res) => {
      const chunks = [];
      req.on("data", (c) => chunks.push(c));
      req.on("end", async () => {
        let parsed;
        try {
          const raw = Buffer.concat(chunks).toString("utf8");
          parsed = raw ? JSON.parse(raw) : undefined;
        } catch {
          parsed = undefined;
        }
        try {
          await transport.handleRequest(req, res, parsed);
        } catch (e) {
          if (!res.headersSent) {
            res.writeHead(500, { "Content-Type": "application/json" });
            res.end(JSON.stringify({ error: String(e) }));
          }
        }
      });
    })
    .listen(httpPort, "127.0.0.1", () => {
      console.error(`[shardx-mcp] HTTP transport on http://127.0.0.1:${httpPort}/mcp`);
    });
} else {
  await server.connect(new StdioServerTransport());
}
