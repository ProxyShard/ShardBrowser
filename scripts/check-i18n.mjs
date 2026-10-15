// Fails when a string a person reads is not behind a translation key, or when
// a key is called but missing from a locale. Run it with `npm run check:i18n`.
//
// The allow-list at the bottom is for text that is the same in every language:
// product names, protocol names, the endonyms in the language picker.
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

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
  "ShardX", "ShardX Launcher", "ProxyShard", "Shard Helper", "MCP", "GPU", "CPU",
  "URL", "SQL", "UDP", "TCP", "HTTP", "HTTPS", "SOCKS5", "Canvas", "WebGL",
  "WebGPU", "WebRTC", "TLS", "CDP", "API", "JSON", "Cookie", "User-Agent",
  "macOS", "MacOS", "Windows", "Windows 10", "Windows 11", "Linux", "Android",
  "iOS", "IOS", "X", "Y", "ISP", "Do Not Track",
  "Authorization: Bearer …", "Content-Type: application/json",
  "BlockedByClient / AccessDenied / …", "· UDP ✓", "· UDP ✗",
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
    if (statSync(p).isDirectory()) { if (name !== "i18n") walk(p); }
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

  for (const m of src.matchAll(/>([^<>{}]{2,400})</g)) {
    const text = m[1].trim().replace(/\s+/g, " ");
    if (human(text) && !CODEY.test(text) && PROSE.test(text)) problems.push([rel, at(m.index), text]);
  }
  for (const m of src.matchAll(/(?<![A-Za-z0-9_.])t\(\s*"([^"\\]{2,400})"/g)) {
    if (NAMED.test(m[1])) called.add(m[1]);
    else if (human(m[1])) problems.push([rel, at(m.index), `t("${m[1]}") — key it by name`]);
  }
  for (const m of src.matchAll(/\b([a-zA-Z-]+)\s*[=:]\s*\{?"([^"\\]{2,400})"\}?/g)) {
    if (TEXT_PROPS.has(m[1]) && human(m[2])) problems.push([rel, at(m.index), `${m[1]}="${m[2]}"`]);
  }
  for (const m of src.matchAll(/"((?:palette|helperKinds)\.[A-Za-z0-9_.]+)"/g)) called.add(m[1]);
}

const en = JSON.parse(readFileSync(join(LOCALES, "en.json"), "utf8"));
const localeFiles = readdirSync(LOCALES).filter((name) => name.endsWith(".json")).sort();
const locales = new Map(localeFiles.map((name) => [
  name.slice(0, -5), JSON.parse(readFileSync(join(LOCALES, name), "utf8")),
]));
const missingEn = [...called].filter((k) => !(k in en)).sort();
const unused = Object.keys(en).filter((k) => !called.has(k)).sort();
const localeProblems = [];
const placeholders = (value) => [...value.matchAll(/(?<!\{)\{([A-Za-z][A-Za-z0-9_]*)\}(?!\})/g)]
  .map((match) => match[1]).sort();

for (const [locale, dictionary] of locales) {
  const missing = Object.keys(en).filter((key) => !(key in dictionary)).sort();
  const extra = Object.keys(dictionary).filter((key) => !(key in en)).sort();
  if (missing.length) localeProblems.push(`keys in en.json but not in ${locale}.json:\n  ${missing.join("\n  ")}`);
  if (extra.length) localeProblems.push(`keys in ${locale}.json but not in en.json:\n  ${extra.join("\n  ")}`);

  for (const [key, value] of Object.entries(dictionary)) {
    if (typeof value !== "string" || !value.trim()) {
      localeProblems.push(`${locale}.json: ${key} must have a non-empty string value`);
      continue;
    }
    if (key in en && typeof en[key] === "string" &&
        JSON.stringify(placeholders(value)) !== JSON.stringify(placeholders(en[key]))) {
      localeProblems.push(`${locale}.json: ${key} placeholders do not match en.json`);
    }
  }
}

for (const [f, line, what] of problems) console.log(`${f}:${line}  ${what}`);
if (missingEn.length) console.log(`\nkeys called but not in en.json:\n  ${missingEn.join("\n  ")}`);
for (const problem of localeProblems) console.log(`\n${problem}`);
if (unused.length) console.log(`\nkeys in en.json that nothing calls:\n  ${unused.join("\n  ")}`);

const bad = problems.length + missingEn.length + localeProblems.length + unused.length;
console.log(bad ? `\n${bad} problem(s)` : `i18n OK — ${Object.keys(en).length} keys, all called, all translated`);
process.exit(bad ? 1 : 0);
