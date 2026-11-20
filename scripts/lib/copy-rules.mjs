// The copy rules of audit Phase G: the words, case, spelling, punctuation and
// Chinese and Russian terms the interface uses (overview G1 to G5, writing.md, 6.2
// "Words"). G1b (Pair 67) will run them inside `npm run check:i18n` over both
// locales and the What's new notes, with scripts/copy-rules.allow.json listing
// the entries a later task (or the Automation track) still has to fix; until
// then each copy task runs `copyProblems` once over its own rule. Pure
// functions over plain objects, so scripts/copy-rules.test.mjs tests each rule
// alone.

/** Capitalized words allowed inside a sentence: the three place names the UI
 *  capitalizes (6.2), the What's new dialog, products, platforms and keys. */
export const PROPER = new Set([
  "Trash", "Settings", "Peek", "What’s", "What's",
  "ShardBrowser", "ProxyShard", "ShardX", "Cloudflare", "Shard", "Helper", "Launcher", "GitHub", "Google", "Chrome", "Chromium", "Web", "Store", "Widevine",
  "Node.js", "README", "PowerShell", "Finder", "Explorer", "Microsoft", "Apple", "Intel", "Edge",
  "macOS", "Windows", "Linux", "Android", "iOS",
  "Cmd", "Ctrl", "Shift", "Alt", "Option", "Enter", "Esc", "Tab", "Space", "Backspace", "Delete", "Home", "End",
  // Languages are names in English ("Release notes are in English"), G3.
  "English", "Chinese",
]);

/** The keys of the profile template's own copy (#145): the one place “template”
 *  is the word, for a Profile template, never for a fingerprint (CONTEXT.md). */
const PROFILE_TEMPLATE_KEY = /^(?:templates\.|commands\.saveAsTemplate$|settings\.(?:tabTemplates|templates\w*)$|team\.trashKindTemplate$|sync\.kindFoldersHelp$)/;

/** Words the glossary replaces (CONTEXT.md, "Words in the interface"). `except`
 *  names the keys a word is the glossary's own in. */
export const BANNED = [
  { re: /\bcore\b/i, use: "“browser engine” (never “core”)" },
  { re: /\b(?:fingerprint|library) templates?\b/i, use: "“fingerprint” (never “template”)" },
  { re: /\btemplates?\b/i, use: "“fingerprint” (never “template”); “template” is a profile template’s word, in its own keys", except: PROFILE_TEMPLATE_KEY },
  { re: /\b(?:bind|binds|binding|bound)\b/i, use: "“assign” (UI verbs)" },
  { re: /\bdistribut(?:e|es|ed|ing)\b/i, use: "“assign” (UI verbs)" },
  { re: /\blaunch(?:es|ed|ing)?\b/i, use: "“start” (Start and Stop)" },
  { re: /\b(?:this|your) machine\b|\bhost (?:monitor|machine|computer)\b|\bthe host[’']s\b/i, use: "“this computer”" },
  { re: /\bAutomation API\b/i, use: "“Local API”" },
  { re: /\bNothing matches\b/i, use: "“No results for “{q}””" },
  { re: /\bsynchroni[sz]\w*/i, use: "“in sync”" },
];
/** The banned words that also apply to the What's new notes (ruling 3): a note may name profile templates. */
const NOTE_BANNED = BANNED.slice(0, 2);

const BRITISH = /\b(?:colour\w*|behaviour\w*|favourite\w*|licence|centre\w*|organis\w*|recognis\w*|synchronis\w*|cancelled|cancelling|postcode|untick\w*|tick|ticked|grey)\b/i;

/** The middle dot, banned everywhere (owner rule): a comma, a word or a line
 *  break says it instead. Written as an escape so this file holds none. */
const MIDDOT = /\u00B7/;
const middot = (v) => (MIDDOT.test(v) ? "no middle dot: use a comma, a word or a new line" : null);

/** Keys whose value is a command label: a button, a menu item or a palette
 *  command. A command does not end with "…" (clarity fixes, item 4): the
 *  dialog it opens says what it needs. Placeholders ("Search…"), hints and
 *  progress words ("Saving…") keep theirs. Every key under a prefix counts;
 *  a label elsewhere is named one by one: add the key here when you add the
 *  label (check-i18n fails on a key that is listed but no longer exists). */
export const COMMAND_PREFIXES = ["commands."];
export const COMMAND_KEYS = [
  "actionMenu.readText", "actionMenu.type",
  "dataRootCard.change",
  "deleteAllData.button", "deleteAllData.confirm", "deleteAllData.continue", "deleteAllData.restart",
  "deleteAllData.close", "deleteAllData.showBackup", "deleteAllData.showFolder",
  "extensions.addFromFile", "extensions.addFromLink", "extensions.addUnpacked",
  "fingerprintsMenu.importFile", "fingerprintsMenu.pasteJson",
  "folderCellMenu.newFolder",
  "profileFilterBar.saveView",
  "psBuyCard.open", "psKey.changeItem",
  "resetPending.cancel", "resetPending.finish", "resetPending.setAside", "resetPending.setAsideConfirm",
  "resetPending.showFolder", "resetPending.undo",
  "sidebar.newFolder", "sidebar.viewRename",
  "stepDetails.branchGoto", "stepDetails.branchRetry",
  "trash.deleteForGood",
  "useProfile.directAssign", "useProxy.assignReplacements",
  "viewBar.saveAs", "viewBar.saveCopy",
];
export const isCommandKey = (key) => COMMAND_PREFIXES.some((p) => key.startsWith(p)) || COMMAND_KEYS.includes(key);
const ELLIPSIS_END = /(?:…|\.\.\.)\s*$/;
const commandEllipsis = (v, key) => (isCommandKey(key) && ELLIPSIS_END.test(v) ? "a command label does not end with …: the dialog it opens says what it needs" : null);

const ACRONYM = /^[A-Z0-9]{2,}(?:s|es)?$/;
const INNER_CAPS = /[a-z][A-Z]/;

/** Capitalized words inside a sentence that are not names (sentence case). A
 *  sentence starts at the value's start, after . ! ? : … and after an opening
 *  quote, bracket, slash or a new line. Placeholders count as a word ("{n}
 *  Profiles" is mid-sentence); tags are dropped and code is skipped. */
export function capitalsInside(value) {
  const text = String(value)
    .replace(/<code>[\s\S]*?<\/code>/gi, "0")
    .replace(/\{[^}]*\}/g, "0")
    .replace(/<\/?[a-z][a-z0-9]*>/gi, "");
  const out = [];
  for (const m of text.matchAll(/[A-Za-zÀ-ɏ][A-Za-zÀ-ɏ0-9’'.-]*/g)) {
    const word = m[0].replace(/[.’'-]+$/, "");
    if (!/^[A-Z]/.test(word)) continue;
    const before = text.slice(0, m.index);
    if (/^\s*$/.test(before) || /[.!?:…]\s+$/.test(before) || /[\n“"(‘«/]\s*$/.test(before)) continue;
    if (PROPER.has(word) || ACRONYM.test(word) || INNER_CAPS.test(word) || /\d/.test(word) || /^[A-Z]$/.test(word)) continue;
    out.push(word);
  }
  return out;
}

const varsOf = (s) => [...String(s).matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort().join(",");
const tagsOf = (s) => [...String(s).matchAll(/<([a-z][a-z0-9]*)>/gi)].map((m) => m[1]).sort().join(",");
const banned = (value, list, key = "") => list.filter((b) => b.re.test(value) && !b.except?.test(key)).map((b) => `use ${b.use}`).join("; ") || null;

/** Rules over one English value: `test(value, key)` returns a message or null. */
const EN_RULES = [
  { id: "sentence-case", test: (v) => { const c = capitalsInside(v); return c.length ? `lowercase inside a sentence: ${c.join(", ")}` : null; } },
  { id: "us-spelling", test: (v) => (BRITISH.test(v) ? `US spelling: ${v.match(BRITISH)[0]}` : null) },
  { id: "banned-word", test: (v, key) => banned(v, BANNED, key) },
  { id: "straight-quote", test: (v) => (/"|[A-Za-z]'[A-Za-z]|(?:^|[\s(])'|'(?:[\s).,!?]|$)/.test(v) ? "curly quotes and apostrophes: “ ” ’" : null) },
  { id: "three-dots", test: (v) => (v.includes("...") ? "an ellipsis is one character: …" : null) },
  { id: "command-ellipsis", test: (v, key) => commandEllipsis(v, key) },
  { id: "label-glyph", test: (v) => (/[→←‹›✓✗]/.test(v) ? "no arrows or check marks in copy: say it, or use an icon" : null) },
  { id: "bare-count", test: (v) => (/^(?:Deleted|Copied|Moved|Restored|Assigned|Imported|Exported|Added|Removed|Bound|Tested|Started|Stopped) \{n\}(?: (?:to|from|in|into) (?:the )?[a-z]+)?\.?$/.test(v) ? "name what the number counts: “Deleted {n} proxies”" : null) },
  { id: "plural-hack", test: (v) => (/[A-Za-z]\(s\)/.test(v) ? "one key for one, one for many: never “(s)”" : null) },
  { id: "error-in-title", test: (v) => (/\{(?:err|error|e)\}/.test(v) ? "a raw error inside copy: pass it as toast.fail's detail" : null) },
  { id: "bare-learn-more", test: (v) => (/\bLearn more\b(?! about\b)/.test(v) ? "“Learn more about {topic}”, never a bare “Learn more”" : null) },
  { id: "no-middot", test: (v) => middot(v) },
  { id: "fragment-key", test: (_v, key) => { const last = key.split(".").pop(); return /(?:Part\d*|Word|Suffix)$/.test(last) || /(?:Help|Body|Intro|Outro|Hint)\d$/.test(last) ? "a sentence in pieces: one key per sentence, with t.rich for bold or code" : null; } },
];

/** Rules over one Chinese value: `test(zh, en, key)`. */
const ZH_RULES = [
  { id: "vars-match", test: (z, e) => (varsOf(z) !== varsOf(e) ? `placeholders differ: en {${varsOf(e)}} zh {${varsOf(z)}}` : null) },
  { id: "tags-match", test: (z, e) => (tagsOf(z) !== tagsOf(e) ? `tags differ: en <${tagsOf(e)}> zh <${tagsOf(z)}>` : null) },
  { id: "no-middot", test: (z) => middot(z) },
  { id: "command-ellipsis", test: (z, _e, key) => commandEllipsis(z, key) },
  { id: "straight-quote-zh", test: (z) => (z.includes('"') ? "中文使用弯引号 “ ”" : null) },
  { id: "zh-pin", test: (z, e) => (/\b(?:Pin|Pinned|Unpin)\b/.test(e) && z.includes("固定") ? "Pin 是 置顶" : null) },
  { id: "zh-peek", test: (z, e) => (/\bPeek\b/.test(e) && (z.includes("Peek") || !z.includes("预览")) ? "Peek 是 预览" : null) },
  { id: "zh-order", test: (z, e) => (/\border\s#/i.test(e) && !z.includes("订单") ? "order 是 订单" : null) },
  { id: "zh-country", test: (z) => (/国家(?!\/地区)/.test(z) ? "country 是 国家/地区" : null) },
  { id: "zh-formal-you", test: (z) => (z.includes("您") ? "用 你，不用 您" : null) },
  { id: "zh-profile-file", test: (z, e) => (z.includes("配置文件") && !/\bconfig(?:uration)? files?\b/i.test(e) ? "Profile 是 配置（配置文件 指 config file）" : null) },
  { id: "zh-move-to-folder", test: (z, e) => (/^Move to folder/.test(e) && !z.startsWith("移到文件夹") ? "Move to folder 是 移到文件夹" : null) },
];

/** A Russian count's word forms, `{n|файл|файла|файлов}`: one, few and many,
 *  picked by the number in `n` (src/shared/i18n). */
const pluralsOf = (s) => [...String(s).matchAll(/\{(\w+)\|([^{}]*)\}/g)].map((m) => ({ v: m[1], forms: m[2].split("|") }));
const outsideCode = (s) => String(s).replace(/<code>[\s\S]*?<\/code>/gi, "");
const RU_TY = /(?<!\p{L})(?:ты|тебя|тебе|тобой|твой|твоя|твоё|твое|твои|твоих|твоим|твоими|твоего|твоей|твоему|твою)(?!\p{L})/iu;
/** "Вы" with a capital inside a sentence: the interface writes "вы". */
const RU_VY_CAPITAL = /[^\s.!?…:«(\n]\s+(?:Вы|Вас|Вам|Вами|Ваш\p{L}*)(?!\p{L})/u;

/** Rules over one Russian value: `test(ru, en, key)`. */
const RU_RULES = [
  { id: "vars-match", test: (r, e) => (varsOf(r) !== varsOf(e) ? `placeholders differ: en {${varsOf(e)}} ru {${varsOf(r)}}` : null) },
  { id: "tags-match", test: (r, e) => (tagsOf(r) !== tagsOf(e) ? `tags differ: en <${tagsOf(e)}> ru <${tagsOf(r)}>` : null) },
  {
    id: "ru-plural",
    test: (r, e) => {
      const known = varsOf(e).split(",");
      for (const p of pluralsOf(r)) {
        if (p.forms.length !== 3 || p.forms.some((f) => !f.trim())) return `a count takes three forms: {${p.v}|файл|файла|файлов}`;
        if (!known.includes(p.v)) return `{${p.v}|…} counts a placeholder the English does not have`;
      }
      return null;
    },
  },
  { id: "no-middot", test: (r) => middot(r) },
  { id: "command-ellipsis", test: (r, _e, key) => commandEllipsis(r, key) },
  { id: "three-dots", test: (r) => (r.includes("...") ? "an ellipsis is one character: …" : null) },
  { id: "ru-quotes", test: (r) => (/["“”„]/.test(outsideCode(r)) ? "кавычки-ёлочки: « »" : null) },
  { id: "ru-formal-you", test: (r) => (RU_TY.test(r) ? "обращение на «вы», не на «ты»" : null) },
  { id: "ru-you-lowercase", test: (r) => (RU_VY_CAPITAL.test(outsideCode(r)) ? "«вы» со строчной буквы" : null) },
  { id: "ru-trash", test: (r, e) => (/\bTrash\b/.test(e) && !r.includes("Корзин") ? "Trash — это Корзина, с прописной" : null) },
  { id: "ru-peek", test: (r, e) => (/\bPeek\b/.test(e) && !/быстр\p{L}* просмотр/iu.test(r) ? "Peek — это быстрый просмотр" : null) },
  { id: "ru-pin", test: (r, e) => (/\b(?:[Pp]in(?:s|ned)?|[Uu]npin(?:ned)?)\b/.test(e) && !/закреп|откреп/i.test(r) ? "Pin — это закрепить, Unpin — открепить" : null) },
  { id: "ru-fingerprint", test: (r, e) => (/\bfingerprints?\b/i.test(e) && !/отпечат/i.test(r) ? "fingerprint — это отпечаток" : null) },
  { id: "ru-order", test: (r, e) => (/\border\s#/i.test(e) && !r.includes("заказ") ? "order — это заказ" : null) },
  { id: "ru-residential", test: (r) => (/резидентн/i.test(r) ? "residential — это резидентский (резидентские прокси), не «резидентный»" : null) },
  // «Ядра» are a CPU's cores and stay; the browser engine is never one.
  {
    id: "ru-engine",
    test: (r, e) => (/\bbrowser engine\b/i.test(e) && !/движ/i.test(r)) || (/(?<!\p{L})ядр\p{L}*/iu.test(r) && !/\bcores?\b/i.test(e))
      ? "browser engine — это движок браузера, не «ядро»" : null,
  },
];

/** Rules over one What's new note text: `test(text)`. */
const NOTE_RULES = [
  { id: "us-spelling", test: (v) => (BRITISH.test(v) ? `US spelling: ${v.match(BRITISH)[0]}` : null) },
  { id: "banned-word", test: (v) => banned(v, NOTE_BANNED) },
  { id: "no-middot", test: (v) => middot(v) },
];

/** Every text of every What's new entry, with where it sits. */
export function noteTexts(patchlog) {
  const out = [];
  for (const rel of patchlog?.releases ?? []) {
    for (const entry of rel.entries ?? []) {
      const where = `${rel.version}/${entry.id}`;
      const walk = (v) => {
        if (typeof v === "string") out.push({ where, text: v });
        else if (Array.isArray(v)) v.forEach(walk);
        else if (v && typeof v === "object") {
          for (const [k, x] of Object.entries(v)) if (!["id", "tag", "scope", "type", "href", "url"].includes(k)) walk(x);
        }
      };
      walk(entry);
    }
  }
  return out;
}

export function copyProblems({ en, zh = {}, ru = {}, notes = [] }) {
  const out = [];
  for (const [key, value] of Object.entries(en)) {
    for (const r of EN_RULES) {
      const m = r.test(String(value), key);
      if (m) out.push({ where: `en:${key}`, rule: r.id, message: m });
    }
    for (const [lang, dict, list] of [["zh", zh, ZH_RULES], ["ru", ru, RU_RULES]]) {
      if (typeof dict[key] !== "string") continue;
      for (const r of list) {
        const m = r.test(dict[key], String(value), key);
        if (m) out.push({ where: `${lang}:${key}`, rule: r.id, message: m });
      }
    }
  }
  for (const n of notes) {
    for (const r of NOTE_RULES) {
      const m = r.test(n.text);
      if (m && !out.some((p) => p.where === `notes:${n.where}` && p.rule === r.id)) out.push({ where: `notes:${n.where}`, rule: r.id, message: m });
    }
  }
  return out;
}

export const allowed = (allow, p) => (allow[p.rule] ?? []).includes(p.where);

/** Allowlist lines that no longer break their rule: they must be deleted. */
export function staleAllow(allow, problems) {
  const out = [];
  for (const [rule, list] of Object.entries(allow)) {
    for (const where of list) if (!problems.some((p) => p.rule === rule && p.where === where)) out.push({ rule, where });
  }
  return out;
}
