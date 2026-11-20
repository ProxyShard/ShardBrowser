// Fails when a string a person reads is not behind a translation key, or when
// a key is called but missing from a locale. Run it with `npm run check:i18n`.
//
// The allow-list at the bottom is for text that is the same in every language:
// product names, protocol names, the endonyms in the language picker.
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { COMMAND_KEYS, allowed, copyProblems, noteTexts, staleAllow } from "./lib/copy-rules.mjs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

// fileURLToPath, not .pathname: a URL keeps "%20" where the path has a
// space and keeps a leading slash before a drive letter, so a checkout
// under "My Project" or on Windows looked for files that are not there.
const ROOT = fileURLToPath(new URL("..", import.meta.url));
const SRC = join(ROOT, "src");
const LOCALES = join(SRC, "shared/i18n/locales");

const TEXT_PROPS = new Set([
  "label", "placeholder", "title", "description", "hint", "subtitle", "heading",
  "confirmLabel", "cancelLabel", "searchPlaceholder", "emptyText", "tooltip",
  "alt", "aria-label", "confirmText", "buttonLabel", "caption", "helperText",
  "empty", "summary", "message",
]);

// Same in every language: product and protocol names, OS names, the language
// picker's own entries, and samples of code or headers.
const KEEP = new Set([
  "ShardBrowser", "ShardBrowser Launcher", "ProxyShard", "ShardX", "Shard Helper", "GitHub", "MCP", "GPU", "CPU",
  "URL", "SQL", "UDP", "TCP", "HTTP", "HTTPS", "SOCKS5", "Canvas", "WebGL",
  "WebGPU", "WebRTC", "TLS", "CDP", "API", "JSON", "Cookie", "User-Agent",
  "macOS", "MacOS", "Windows", "Windows 10", "Windows 11", "Linux", "Android",
  "iOS", "IOS", "X", "Y", "ISP", "Do Not Track", "Esc", "Enter", "Ctrl", "Space", "Home", "End", "Delete", "Shift",
  "Authorization: Bearer …", "Content-Type: application/json",
  "BlockedByClient / AccessDenied / …", ", UDP ✓", ", UDP ✗",
]);
const KEEP_PREFIX = ["English (", "Deutsch", "Español", "Français", "Italiano",
  "Nederlands", "Polski", "Português", "Română", "Русский", "Українська",
  "Türkçe", "Ελληνικά", "Čeština", "Svenska", "Suomi", "Norsk", "Dansk",
  "Magyar", "中文", "日本語", "한국어", "العربية", "עברית", "Bahasa", "Tiếng",
  "ไทย", "हिन्दी", "Auto (from proxy geo)"];

const NAMED = /^[a-z][A-Za-z0-9]*\.[A-Za-z0-9_.]+$/;
const CODEY = /[(){};=]|=>|\bconst\b|\bawait\b|\breturn\b|\buseState\b|\bPromise\b|\bRecord\b|\bSet\b/;
const PROSE = /^[A-Za-z\u00C0-\u024F0-9 ,.'"\u2018\u2019\u201c\u201d\u2014\u2013:!?%\u00b7\u2192\u2190\u2713\u2717&/+#\u2026-]+$/;

const files = [];
(function walk(dir) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    // src/dev is the in-browser mock backend: its strings are seed data, not UI.
    if (statSync(p).isDirectory()) { if (name !== "i18n" && p !== join(SRC, "dev")) walk(p); }
    else if (name.endsWith(".tsx") || name.endsWith(".ts")) files.push(p);
  }
})(SRC);

const keep = (s) => KEEP.has(s) || KEEP_PREFIX.some((p) => s.startsWith(p));
const human = (s) => {
  const t = s.trim().replace(/\s+/g, " ");
  if (t.length < 2 || !/[A-Za-z]{2}/.test(t)) return false;
  if (NAMED.test(t) || keep(t)) return false;
  if (/^(https?:|\/\/|#|data:|ws:|\/|-|\*)/.test(t)) return false;
  if (/^[\w.\-/:#@[\]]+$/.test(t) && !t.includes(" ") && t === t.toLowerCase()) return false;
  // A fragment of a ternary or a lone identifier my bracket match tripped over.
  if (/^[:?]/.test(t) || /^[A-Za-z_$][\w$]*$/.test(t) && !/[A-Z]/.test(t[0])) return false;
  return true;
};

const problems = [];
const called = new Set();
for (const file of files) {
  const raw = readFileSync(file, "utf8");
  // Block comments only where one opens a line: a URL glob like
  // "*://*.example.com/*" contains /* and would otherwise swallow the code
  // after it, hiding real strings from this check.
  const src = raw
    .replace(/^[ \t]*\/\*[\s\S]*?\*\//gm, (m) => "\n".repeat((m.match(/\n/g) || []).length))
    .split("\n").map((l) => (l.trim().startsWith("//") ? "" : l)).join("\n");
  const at = (i) => src.slice(0, i).split("\n").length;
  const rel = relative(ROOT, file);

  // JSX text only exists between tags in a .tsx file: a plain .ts file's
  // ">...<" is always code (generics, comparisons), and reads as prose often
  // enough in a generic-heavy module to be a false positive on its own.
  if (file.endsWith(".tsx")) {
    for (const m of src.matchAll(/>([^<>{}]{2,400})</g)) {
      const text = m[1].trim().replace(/\s+/g, " ");
      if (human(text) && !CODEY.test(text) && PROSE.test(text)) problems.push([rel, at(m.index), text]);
    }
  }
  for (const m of src.matchAll(/(?<![A-Za-z0-9_.])t(?:\.rich)?\(\s*"([^"\\]{2,400})"/g)) {
    if (NAMED.test(m[1])) called.add(m[1]);
    else if (human(m[1])) problems.push([rel, at(m.index), `t("${m[1]}") — key it by name`]);
  }
  for (const m of src.matchAll(/\b([a-zA-Z-]+)\s*[=:]\s*\{?"([^"\\]{2,400})"\}?/g)) {
    if (TEXT_PROPS.has(m[1]) && human(m[2])) problems.push([rel, at(m.index), `${m[1]}="${m[2]}"`]);
  }
  // A command's id ("palette.run", E2's registry) names a command, not a key.
  // The tour's steps (src/features/tour/steps) hold their keys as plain strings.
  for (const m of src.matchAll(/(?<!\bid:\s*)"((?:palette|blockPicker|helperKinds|labelColors|folderIcons|views|shortcuts|errors|launcherWarning|tour)\.[A-Za-z0-9_.]+)"/g)) called.add(m[1]);
}

const en = JSON.parse(readFileSync(join(LOCALES, "en.json"), "utf8"));
const zh = JSON.parse(readFileSync(join(LOCALES, "zh.json"), "utf8"));
const ru = JSON.parse(readFileSync(join(LOCALES, "ru.json"), "utf8"));

// The owner never wants the middle dot (·) in the UI: not in copy, separators
// or sample data. Checked in the source (new UI and kit) and in every locale.
const MIDDOT = "\u00B7";
const kitFiles = [];
(function walkKit(dir) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walkKit(p);
    else if (/\.(tsx?|css)$/.test(name)) kitFiles.push(p);
  }
})(join(ROOT, "ui-kit/src"));
for (const file of [...files, ...kitFiles]) {
  readFileSync(file, "utf8").split("\n").forEach((line, i) => {
    if (line.includes(MIDDOT)) problems.push([relative(ROOT, file), i + 1, "middle dot (·): not used in the UI"]);
  });
}
for (const [name, dict] of [["en", en], ["zh", zh], ["ru", ru]]) {
  for (const [key, value] of Object.entries(dict)) {
    if (String(value).includes(MIDDOT)) problems.push([`locales/${name}.json`, key, "middle dot (·): not used in the UI"]);
  }
}
// The copy rules (audit Phase G): words, case, spelling, punctuation and the
// Chinese and Russian terms. scripts/copy-rules.allow.json lists what is still to fix and
// may only shrink: a line that no longer breaks its rule must be deleted.
const ALLOW = join(ROOT, "scripts/copy-rules.allow.json");
const allow = existsSync(ALLOW) ? JSON.parse(readFileSync(ALLOW, "utf8")) : {};
const PATCHLOG = join(SRC, "widgets/WhatsNewDialog/patchlog.json");
const notes = existsSync(PATCHLOG) ? noteTexts(JSON.parse(readFileSync(PATCHLOG, "utf8"))) : [];
const copy = copyProblems({ en, zh, ru, notes });
for (const p of copy) if (!allowed(allow, p)) problems.push(["copy", p.where, `${p.rule}: ${p.message}`]);
for (const s of staleAllow(allow, copy)) problems.push(["scripts/copy-rules.allow.json", s.rule, `${s.where} no longer breaks this rule: delete the line`]);
// A command-label key the list still names must exist: delete it when its label goes.
for (const k of COMMAND_KEYS) if (!(k in en)) problems.push(["scripts/lib/copy-rules.mjs", k, "COMMAND_KEYS names a key that is not in en.json: delete it"]);
const missingEn = [...called].filter((k) => !(k in en)).sort();
const missingZh = Object.keys(en).filter((k) => !(k in zh)).sort();
const missingRu = Object.keys(en).filter((k) => !(k in ru)).sort();
// A key only a translation has is one the English lost: delete it there too.
const extra = [...Object.keys(zh).filter((k) => !(k in en)).map((k) => `zh.json: ${k}`), ...Object.keys(ru).filter((k) => !(k in en)).map((k) => `ru.json: ${k}`)].sort();
const unused = Object.keys(en).filter((k) => !called.has(k)).sort();

for (const [f, line, what] of problems) console.log(`${f}:${line}  ${what}`);
if (missingEn.length) console.log(`\nkeys called but not in en.json:\n  ${missingEn.join("\n  ")}`);
if (missingZh.length) console.log(`\nkeys in en.json but not in zh.json:\n  ${missingZh.join("\n  ")}`);
if (missingRu.length) console.log(`\nkeys in en.json but not in ru.json:\n  ${missingRu.join("\n  ")}`);
if (extra.length) console.log(`\nkeys in a translation but not in en.json:\n  ${extra.join("\n  ")}`);
if (unused.length) console.log(`\nkeys in en.json that nothing calls:\n  ${unused.join("\n  ")}`);

const bad = problems.length + missingEn.length + missingZh.length + missingRu.length + extra.length + unused.length;
console.log(bad ? `\n${bad} problem(s)` : `i18n OK — ${Object.keys(en).length} keys, all called, all translated`);
process.exit(bad ? 1 : 0);
