import { test } from "node:test";
import assert from "node:assert/strict";
import { allowed, capitalsInside, copyProblems, isCommandKey, noteTexts, staleAllow } from "./lib/copy-rules.mjs";

const rules = (en, zh = {}, notes = []) => copyProblems({ en, zh, notes }).map((p) => `${p.where} ${p.rule}`);

test("sentence-case: words inside a sentence are lowercase unless they are names", () => {
  assert.deepEqual(capitalsInside("Save as View…"), ["View"]);
  assert.deepEqual(capitalsInside("Buy Proxies"), ["Proxies"]);
  assert.deepEqual(capitalsInside("{n} Profiles in <b>Folder</b>"), ["Profiles", "Folder"]);
  assert.deepEqual(capitalsInside("Move to Trash. Settings and Peek keep it."), []);
  assert.deepEqual(capitalsInside("Copy the API URL from WebGL, macOS or Windows 11 with Cmd+K"), []);
  assert.deepEqual(capitalsInside("Delete “{name}”? It can’t be undone: Delete for good"), []);
  assert.deepEqual(capitalsInside("Start/Stop, A to Z, X and Y"), []);
  assert.deepEqual(capitalsInside("Open What’s new"), []);
  assert.deepEqual(capitalsInside("Sent as <code>Authorization: Bearer …</code> to the Chrome Web Store"), []);
});

test("us-spelling and banned words", () => {
  assert.deepEqual(rules({ "a.b": "Colour" }), ["en:a.b us-spelling"]);
  assert.deepEqual(rules({ "a.b": "Tick only working ones" }), ["en:a.b us-spelling"]);
  assert.deepEqual(rules({ "a.b": "Bind {n}" }), ["en:a.b banned-word"]);
  assert.deepEqual(rules({ "a.b": "Distribute {n} proxies" }), ["en:a.b banned-word"]);
  assert.deepEqual(rules({ "a.b": "on their next launch" }), ["en:a.b banned-word"]);
  assert.deepEqual(rules({ "a.b": "From template" }), ["en:a.b banned-word"]);
  // A profile template's own keys may say it; never of a fingerprint.
  assert.deepEqual(rules({ "templates.save": "Save template", "commands.saveAsTemplate": "Save as template" }), []);
  assert.deepEqual(rules({ "templates.x": "Pick a fingerprint template" }), ["en:templates.x banned-word"]);
  const notes = [{ where: "2.2.0/e1", text: "Profile templates; the library template" }];
  assert.deepEqual(rules({}, {}, notes), ["notes:2.2.0/e1 banned-word"]);
  assert.deepEqual(rules({ "a.b": "the patched core" }), ["en:a.b banned-word"]);
  assert.deepEqual(rules({ "a.b": "This machine can run it." }), ["en:a.b banned-word"]);
  assert.deepEqual(rules({ "a.b": "the automation API stays up" }), ["en:a.b banned-word"]);
  assert.deepEqual(rules({ "a.b": "ShardBrowser Launcher settings, CPU cores" }), []);
});

test("quotes, dots, glyphs, counts, plurals, raw errors and Learn more", () => {
  assert.deepEqual(rules({ "a.b": "Nothing matches \"{q}\"." }), ["en:a.b banned-word", "en:a.b straight-quote"]);
  assert.deepEqual(rules({ "a.b": "It's here" }), ["en:a.b straight-quote"]);
  assert.deepEqual(rules({ "a.b": "It’s “{name}”" }), []);
  assert.deepEqual(rules({ "a.b": "Loading..." }), ["en:a.b three-dots"]);
  assert.deepEqual(rules({ "a.b": "Parse →" }), ["en:a.b label-glyph"]);
  assert.deepEqual(rules({ "a.b": "Deleted {n}" }), ["en:a.b bare-count"]);
  assert.deepEqual(rules({ "a.b": "Copied {n} to clipboard" }), ["en:a.b bare-count"]);
  assert.deepEqual(rules({ "a.b": "Deleted {n} proxies" }), []);
  assert.deepEqual(rules({ "a.b": "{n} profile(s) use it" }), ["en:a.b plural-hack"]);
  assert.deepEqual(rules({ "a.b": "Timeout (s)" }), []);
  assert.deepEqual(rules({ "a.b": "Import failed: {error}" }), ["en:a.b error-in-title"]);
  assert.deepEqual(rules({ "a.b": "Learn more" }), ["en:a.b bare-learn-more"]);
  assert.deepEqual(rules({ "a.b": "Learn more about Shard Helper" }), []);
});

test("no-middot: never the middle dot, in English, Chinese or the notes", () => {
  const dot = "\u00B7";
  assert.deepEqual(rules({ "a.b": `Profiles ${dot} 3` }), ["en:a.b no-middot"]);
  assert.deepEqual(rules({ "a.b": "Profiles, 3" }, { "a.b": `配置 ${dot} 3` }), ["zh:a.b no-middot"]);
  const notes = [{ where: "2.0.3/e1", text: `Faster ${dot} smaller` }];
  assert.deepEqual(rules({}, {}, notes), ["notes:2.0.3/e1 no-middot"]);
  assert.deepEqual(rules({ "a.b": "Cmd+K", "c.d": "⌘K" }, { "a.b": "Cmd+K", "c.d": "⌘K" }), []);
});

test("command-ellipsis: a command label never ends with …; a placeholder or a progress word may", () => {
  assert.deepEqual(rules({ "commands.newFolder": "New folder…" }), ["en:commands.newFolder command-ellipsis"]);
  assert.deepEqual(rules({ "viewBar.saveAs": "Save as view..." }), ["en:viewBar.saveAs three-dots", "en:viewBar.saveAs command-ellipsis"]);
  assert.deepEqual(rules({ "commands.newFolder": "New folder", "viewBar.saveAs": "Save as view" }), []);
  assert.deepEqual(rules({ "commands.newFolder": "New folder" }, { "commands.newFolder": "新建文件夹…" }), ["zh:commands.newFolder command-ellipsis"]);
  // Placeholders and progress words keep theirs; so does a mid-sentence …
  assert.deepEqual(rules({
    "filters.searchValues": "Search…", "status.starting": "Starting…", "peek.notesPlaceholder": "Add notes…",
    "palette.dbOpen.target.hint": "data.db, mysql://…, mongodb://…",
  }, {
    "filters.searchValues": "搜索…", "status.starting": "启动中…", "peek.notesPlaceholder": "添加备注…",
    "palette.dbOpen.target.hint": "data.db, mysql://…, mongodb://…",
  }), []);
  // Any key under commands. is a command; the rest are named one by one.
  assert.equal(isCommandKey("commands.somethingNew"), true);
  assert.equal(isCommandKey("trash.deleteForGood"), true);
  assert.equal(isCommandKey("filters.searchValues"), false);
});

test("fragment-key: one key per sentence", () => {
  const got = rules({ "x.hintPart1": "Hi", "x.gpuWord": "GPU", "x.drivingSuffix": "Hi", "x.apiHelp1": "Hi", "x.hint": "Hi", "x.subtitle": "Hi" })
    .filter((r) => r.endsWith("fragment-key"));
  assert.deepEqual(got, ["en:x.hintPart1 fragment-key", "en:x.gpuWord fragment-key", "en:x.drivingSuffix fragment-key", "en:x.apiHelp1 fragment-key"]);
});

test("zh keeps the English placeholders and tags, and uses curly quotes", () => {
  assert.deepEqual(rules({ "a.b": "Moved {n} to <b>{f}</b>" }, { "a.b": "已将 {n} 个移到 <b>{f}</b>" }), []);
  assert.deepEqual(rules({ "a.b": "Moved {n} to <b>{f}</b>" }, { "a.b": "已移到 {f}" }), ["zh:a.b vars-match", "zh:a.b tags-match"]);
  assert.deepEqual(rules({ "a.b": "Delete “{n}”?" }, { "a.b": "删除\"{n}\"？" }), ["zh:a.b straight-quote-zh"]);
});

test("zh terms follow the glossary", () => {
  const en = {
    "p.pin": "Pin", "p.peek": "Open in Peek", "p.order": "p0f signature, order #{n}", "p.country": "Country",
    "p.you": "Pick another", "p.pf": "This profile’s fingerprint", "p.move": "Move to folder",
  };
  const good = {
    "p.pin": "置顶", "p.peek": "在预览中打开", "p.order": "p0f 签名，订单 #{n}", "p.country": "国家/地区",
    "p.you": "另选一个", "p.pf": "此配置的指纹", "p.move": "移到文件夹",
  };
  const bad = {
    "p.pin": "固定", "p.peek": "在 Peek 中打开", "p.order": "p0f 签名，顺序 #{n}", "p.country": "国家",
    "p.you": "请您另选", "p.pf": "此配置文件的指纹", "p.move": "移动到文件夹",
  };
  assert.deepEqual(rules(en, good), []);
  assert.deepEqual(rules(en, bad), [
    "zh:p.pin zh-pin", "zh:p.peek zh-peek", "zh:p.order zh-order", "zh:p.country zh-country",
    "zh:p.you zh-formal-you", "zh:p.pf zh-profile-file", "zh:p.move zh-move-to-folder",
  ]);
  assert.deepEqual(rules({ "d.r": "The small config files stay here" }, { "d.r": "少量配置文件留在这里" }), []);
});

const ruRules = (en, ru) => copyProblems({ en, ru }).map((p) => `${p.where} ${p.rule}`);

test("ru keeps the English placeholders and tags, and counts in three forms", () => {
  assert.deepEqual(ruRules({ "a.b": "Moved {n} to <b>{f}</b>" }, { "a.b": "{n|Перемещён|Перемещено|Перемещено} {n} в <b>{f}</b>" }), []);
  assert.deepEqual(ruRules({ "a.b": "Moved {n} to <b>{f}</b>" }, { "a.b": "Перемещено в {f}" }), ["ru:a.b vars-match", "ru:a.b tags-match"]);
  assert.deepEqual(ruRules({ "a.b": "{n} profiles" }, { "a.b": "{n} {n|профиль|профиля|профилей}" }), []);
  assert.deepEqual(ruRules({ "a.b": "{n} profiles" }, { "a.b": "{n} {n|профиль|профилей}" }), ["ru:a.b ru-plural"]);
  assert.deepEqual(ruRules({ "a.b": "{n} profiles" }, { "a.b": "{n} {n|профиль||профилей}" }), ["ru:a.b ru-plural"]);
  assert.deepEqual(ruRules({ "a.b": "{n} profiles" }, { "a.b": "{n} {count|профиль|профиля|профилей}" }), ["ru:a.b ru-plural"]);
});

test("ru punctuation: «ёлочки», one-character ellipsis, no middle dot, no … on a command", () => {
  const dot = "·";
  assert.deepEqual(ruRules({ "a.b": "Delete “{name}”?" }, { "a.b": "Удалить «{name}»?" }), []);
  assert.deepEqual(ruRules({ "a.b": "Delete “{name}”?" }, { "a.b": "Удалить \"{name}\"?" }), ["ru:a.b ru-quotes"]);
  assert.deepEqual(ruRules({ "a.b": "Delete “{name}”?" }, { "a.b": "Удалить “{name}”?" }), ["ru:a.b ru-quotes"]);
  assert.deepEqual(ruRules({ "a.b": "Run <code>npm i x</code>" }, { "a.b": "Выполните <code>npm i \"x\"</code>" }), []);
  assert.deepEqual(ruRules({ "a.b": "Saving…" }, { "a.b": "Сохранение..." }), ["ru:a.b three-dots"]);
  assert.deepEqual(ruRules({ "a.b": "Profiles, 3" }, { "a.b": `Профили ${dot} 3` }), ["ru:a.b no-middot"]);
  assert.deepEqual(ruRules({ "commands.newFolder": "New folder" }, { "commands.newFolder": "Новая папка…" }), ["ru:commands.newFolder command-ellipsis"]);
});

test("ru addresses the user as lowercase «вы»", () => {
  assert.deepEqual(ruRules({ "a.b": "Pick another" }, { "a.b": "Выберите другой. Вы сможете вернуться" }), []);
  assert.deepEqual(ruRules({ "a.b": "Pick another" }, { "a.b": "Выбери другой" }), []);
  assert.deepEqual(ruRules({ "a.b": "Pick another" }, { "a.b": "Если хочешь, твой выбор сохранится" }), ["ru:a.b ru-formal-you"]);
  assert.deepEqual(ruRules({ "a.b": "Pick another" }, { "a.b": "Если Вы хотите, выберите другой" }), ["ru:a.b ru-you-lowercase"]);
});

test("ru terms follow the glossary", () => {
  const en = {
    "p.trash": "Move to Trash", "p.peek": "Open in Peek", "p.pin": "Pin", "p.fp": "Regenerate fingerprint",
    "p.order": "p0f signature, order #{n}", "p.engine": "The browser engine isn’t installed",
  };
  const good = {
    "p.trash": "Переместить в Корзину", "p.peek": "Открыть в быстром просмотре", "p.pin": "Закрепить", "p.fp": "Создать новый отпечаток",
    "p.order": "Подпись p0f, заказ №{n}", "p.engine": "Движок браузера не установлен",
  };
  const bad = {
    "p.trash": "Переместить в корзину", "p.peek": "Открыть в Peek", "p.pin": "В избранное", "p.fp": "Создать новый шаблон",
    "p.order": "Подпись p0f, порядок №{n}", "p.engine": "Ядро браузера не установлено",
  };
  assert.deepEqual(ruRules(en, good), []);
  assert.deepEqual(ruRules(en, bad), [
    "ru:p.trash ru-trash", "ru:p.peek ru-peek", "ru:p.pin ru-pin", "ru:p.fp ru-fingerprint", "ru:p.order ru-order", "ru:p.engine ru-engine",
  ]);
  // Residential proxies are «резидентские», in every form.
  assert.deepEqual(ruRules({ "r.a": "Residential proxies" }, { "r.a": "Резидентские прокси" }), []);
  assert.deepEqual(ruRules({ "r.b": "Get a residential proxy" }, { "r.b": "Получить резидентный прокси" }), ["ru:r.b ru-residential"]);
  assert.deepEqual(ruRules({ "r.c": "No residential order" }, { "r.c": "Нет резидентного заказа" }), ["ru:r.c ru-residential"]);
  // A CPU's cores are «ядра»; an engine is never «ядро», even under another word.
  assert.deepEqual(ruRules({ "c.c": "{n} cores" }, { "c.c": "{n} {n|ядро|ядра|ядер}" }), []);
  assert.deepEqual(ruRules({ "c.e": "Updating the browser engine" }, { "c.e": "Обновление браузера" }), ["ru:c.e ru-engine"]);
});

test("What’s new notes: US spelling and the engine’s name", () => {
  const notes = noteTexts({ releases: [{ version: "2.0.3", entries: [
    { id: "e1", title: "Colour fix", tag: "fix", scope: "browser", blocks: [{ type: "p", text: "The patched core agrees now." }] },
  ] }] });
  assert.deepEqual(notes.map((n) => n.where), ["2.0.3/e1", "2.0.3/e1"]);
  assert.deepEqual(rules({}, {}, notes), ["notes:2.0.3/e1 us-spelling", "notes:2.0.3/e1 banned-word"]);
});

test("the allowlist names exact entries and may only shrink", () => {
  const problems = copyProblems({ en: { "a.b": "Colour" }, zh: {} });
  assert.equal(allowed({ "us-spelling": ["en:a.b"] }, problems[0]), true);
  assert.equal(allowed({ "us-spelling": ["en:c.d"] }, problems[0]), false);
  assert.deepEqual(staleAllow({ "us-spelling": ["en:a.b", "en:c.d"] }, problems), [{ rule: "us-spelling", where: "en:c.d" }]);
});
