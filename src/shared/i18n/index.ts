import { create } from "zustand";
import { useMemo } from "react";
import type { ReactNode } from "react";
import en from "./locales/en.json";
import { renderRich, type RichTags } from "./rich";

// Every piece of text a person reads lives in locales/, under a NAME rather
// than under the English text itself: `t("profileTable.name")`, not
// `t("Name")`. Rewording the English then touches one line in en.json instead
// of every call site, and the same key carries every language.
//
// A key with no entry in the chosen language falls back to English, and a key
// in no file at all renders as the key — loud on screen, which is what you
// want while a screen is being built.
export type Lang = "en" | "zh" | "ru" | "vi";

// English is always in: every language falls back to it, and the palette
// finds another language's commands by their English names. The others load
// when they are wanted (`loadLang`), each as a chunk of its own: Chinese's
// 105 KB were the largest part of the startup script, and an English UI never
// reads them.
const EN = en as Record<string, string>;
const DICTS: Partial<Record<Lang, Record<string, string>>> = { en: EN };
const LOADERS: Record<Exclude<Lang, "en">, () => Promise<{ default: unknown }>> = {
  zh: () => import("./locales/zh.json"),
  ru: () => import("./locales/ru.json"),
  vi: () => import("./locales/vi.json"),
};
const loading = new Map<Lang, Promise<void>>();

/** Resolves once `lang`'s words are in (at once when they already are). */
export function loadLang(lang: Lang): Promise<void> {
  if (DICTS[lang]) return Promise.resolve();
  let p = loading.get(lang);
  if (!p) {
    p = LOADERS[lang as Exclude<Lang, "en">]().then((m) => { DICTS[lang] = m.default as Record<string, string>; });
    // A failed load (a stale chunk after an update) may be tried again.
    p.catch(() => loading.delete(lang));
    loading.set(lang, p);
  }
  return p;
}

export const LANG_OPTIONS: { value: Lang; label: string }[] = [
  { value: "en", label: "English" },
  { value: "zh", label: "中文（简体）" },
  { value: "ru", label: "Русский" },
  { value: "vi", label: "Tiếng Việt" },
];

/** Every language's words, for a check that spans them all (a View's name). */
export const loadAllLangs = (): Promise<void> => Promise.all(LANG_OPTIONS.map((o) => loadLang(o.value))).then(() => {});

const isLang = (v: unknown): v is Lang => LANG_OPTIONS.some((o) => o.value === v);

const STORAGE_KEY = "shardx.lang";

function load(): Lang {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (isLang(v)) return v;
    // A Chinese or Russian system gets its language the first time, without
    // being asked.
    const sys = navigator.language?.toLowerCase() ?? "";
    if (sys.startsWith("zh")) return "zh";
    if (sys.startsWith("ru")) return "ru";
    if (sys.startsWith("vi")) return "vi";
    return "en";
  } catch {
    return "en";
  }
}

/** The page's language, so screen readers, fonts and quotes follow it (WCAG 3.1.1). */
const HTML_LANG: Record<Lang, string> = { en: "en", zh: "zh-CN", ru: "ru", vi: "vi-VN" };
/** Dates and numbers follow the UI language, not the computer's (WRITE-11). */
const INTL_LOCALE: Record<Lang, string> = { en: "en-US", zh: "zh-CN", ru: "ru-RU", vi: "vi-VN" };
/** The locale Intl formatters use for `lang`. */
export const intlLocale = (lang: Lang): string => INTL_LOCALE[lang] ?? INTL_LOCALE.en;

function applyLang(v: Lang) {
  if (typeof document !== "undefined") document.documentElement.lang = HTML_LANG[v];
}

type LangState = {
  lang: Lang;
  /** Shows `v` and keeps it: the choice is stored. */
  setLang: (v: Lang) => void;
  /** Shows `v` without storing it (Settings, until Save): `savedLang()` stays. */
  previewLang: (v: Lang) => void;
};

/** The language last asked for: a slower load never overrides a later choice. */
let wanted: Lang | null = null;
/** The language that is stored (or the first run's), whatever is shown. */
let saved: Lang = "en";
export const savedLang = (): Lang => saved;

export const useLang = create<LangState>((set) => {
  const show = (v: Lang) => {
    wanted = v;
    // The words first: the tree is then rebuilt in the new language at once.
    void loadLang(v).then(() => {
      if (wanted !== v) return;
      applyLang(v);
      // The tree is keyed on this in app/index.tsx, so a component holding a
      // translated string in local state is rebuilt with the rest.
      set({ lang: v });
    });
  };
  return {
    // A Chinese UI's words start loading with the bundle; app/index.tsx draws
    // once they are in.
    lang: (() => { const v = load(); saved = v; applyLang(v); void loadLang(v); return v; })(),
    setLang: (v) => {
      try { localStorage.setItem(STORAGE_KEY, v); } catch { /* private mode */ }
      saved = v;
      show(v);
    },
    previewLang: show,
  };
});

/** The locale Intl formatters use: the UI language's. */
export const appLocale = () => intlLocale(useLang.getState().lang);

/// Placeholders that count things: grouped in the UI language ("1,283
/// profiles") when given as numbers. Others (a port, an order id, a screen
/// size) stay as they are.
const COUNT_VARS = new Set(["n", "total", "count", "done", "failed", "ok"]);
const countFormats = new Map<Lang, Intl.NumberFormat>();
export function formatCount(lang: Lang, n: number): string {
  let f = countFormats.get(lang);
  if (!f) {
    f = new Intl.NumberFormat(intlLocale(lang));
    countFormats.set(lang, f);
  }
  return f.format(n);
}

/// A count's word in a language with more than two forms. English picks
/// between two keys ("…One" for 1, the plain key for the rest), which
/// Russian cannot: 21 takes the form of 1, 3 another, 5 a third. So a
/// Russian value names the forms where the word goes, `{n|файл|файла|
/// файлов}`, and the number in `n` picks one: "21 файл", "3 файла",
/// "5 файлов". The forms are in CLDR order (one, few, many); a fraction
/// ("other") takes the few form, as Russian says "2,5 файла".
const PLURAL = /\{(\w+)\|([^{}]*)\}/g;
const PLURAL_INDEX: Partial<Record<Lang, Partial<Record<Intl.LDMLPluralRule, number>>>> = {
  ru: { one: 0, few: 1, many: 2, other: 1 },
};
const pluralRules = new Map<Lang, Intl.PluralRules>();

/** A count as a number, also when it came in already formatted ("1,283",
 *  or "1 283" with a no-break space, which `\s` covers). */
function countOf(v: string | number | undefined): number {
  if (typeof v === "number") return v;
  const s = String(v ?? "").replace(/\s/g, "");
  return Number(/^-?\d{1,3}(?:,\d{3})+$/.test(s) ? s.replace(/,/g, "") : s.replace(",", "."));
}

export function resolvePlurals(lang: Lang, template: string, vars?: Record<string, string | number>): string {
  if (!template.includes("|")) return template;
  return template.replace(PLURAL, (_all, name: string, list: string) => {
    const forms = list.split("|");
    const n = countOf(vars?.[name]);
    // No number to go by: the many form, the one a list of things reads in.
    if (!Number.isFinite(n)) return forms[forms.length - 1];
    let rules = pluralRules.get(lang);
    if (!rules) {
      rules = new Intl.PluralRules(intlLocale(lang));
      pluralRules.set(lang, rules);
    }
    const i = PLURAL_INDEX[lang]?.[rules.select(Math.abs(n))] ?? forms.length - 1;
    return forms[Math.min(i, forms.length - 1)];
  });
}

/** A key's template with its count words picked, placeholders still open.
 *  The forms follow the language the words came from: English when the key
 *  falls back to it, `lang` when the key is its own template. */
function template(lang: Lang, key: string, vars?: Record<string, string | number>): string {
  const from = DICTS[lang]?.[key] === undefined && EN[key] !== undefined ? "en" : lang;
  return resolvePlurals(from, DICTS[from]?.[key] ?? key, vars);
}

/** Look one key up. `{name}` placeholders are filled from `vars`. */
export function translate(
  lang: Lang,
  key: string,
  vars?: Record<string, string | number>,
): string {
  let out = template(lang, key, vars);
  if (vars) {
    for (const [k, v] of Object.entries(vars)) out = out.split(`{${k}}`).join(typeof v === "number" && COUNT_VARS.has(k) ? formatCount(lang, v) : String(v));
  }
  return out;
}

type Vars = Record<string, string | number>;

/** A translator: `t("area.key", vars)`, and `t.rich(key, tags, vars)` for one
 *  sentence with inline tags (`src/shared/i18n/rich.ts`). */
export type T = ((key: string, vars?: Vars) => string) & {
  rich: (key: string, tags: RichTags, vars?: Vars) => ReactNode;
};

const translator = (lang: () => Lang): T =>
  Object.assign((key: string, vars?: Vars) => translate(lang(), key, vars), {
    // Only the count words are picked here: the template is parsed first,
    // then its placeholders filled (rich.ts).
    rich: (key: string, tags: RichTags, vars?: Vars) => renderRich(template(lang(), key, vars), tags, vars),
  });

/** Inside a component. Re-renders when the language changes; the same
 *  function for as long as the language stays. */
export function useT(): T {
  const lang = useLang((s) => s.lang);
  return useMemo(() => translator(() => lang), [lang]);
}

// `inLanguage` sets this for one synchronous call.
let forced: Lang | null = null;

/// Runs `fn` with `t` reading another language: the palette finds a Chinese
/// UI's commands by their English names too, from the same label functions.
export function inLanguage<T>(lang: Lang, fn: () => T): T {
  const was = forced;
  forced = lang;
  try { return fn(); } finally { forced = was; }
}

/** Outside React: stores, helpers, toasts, command labels. */
export const t: T = translator(() => forced ?? useLang.getState().lang);
