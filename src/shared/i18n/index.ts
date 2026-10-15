import { create } from "zustand";
import en from "./locales/en.json";
import zh from "./locales/zh.json";
import ru from "./locales/ru.json";
import {
  STORAGE_KEY,
  Lang,
  loadLang,
  safeWriteStorage,
  translate as translateEngine,
} from "./engine";

export type { Lang };
export { STORAGE_KEY };

const DICTS: Record<Lang, Record<string, string>> = {
  en: en as Record<string, string>,
  zh: zh as Record<string, string>,
  ru: ru as Record<string, string>,
};

export const LANG_OPTIONS: { value: Lang; label: string }[] = [
  { value: "en", label: "English" },
  { value: "zh", label: "中文（简体）" },
  { value: "ru", label: "Русский" },
];

function load(): Lang {
  try {
    const systemLanguage = typeof navigator !== "undefined" ? navigator.language : null;
    return loadLang(localStorage, systemLanguage, STORAGE_KEY);
  } catch {
    return "en";
  }
}

type LangState = { lang: Lang; setLang: (v: Lang) => void };

export const useLang = create<LangState>((set) => ({
  lang: load(),
  setLang: (v) => {
    try {
      safeWriteStorage(localStorage, v, STORAGE_KEY);
    } catch {
      /* private mode */
    }
    set({ lang: v });
  },
}));

/** Look one key up. `{name}` placeholders are filled from `vars`. */
export function translate(
  lang: Lang,
  key: string,
  vars?: Record<string, string | number>,
): string {
  return translateEngine(DICTS, lang, key, vars);
}

/** Inside a component. Re-renders when the language changes. */
export function useT() {
  const lang = useLang((s) => s.lang);
  return (key: string, vars?: Record<string, string | number>) =>
    translate(lang, key, vars);
}

/** Outside React — stores, helpers, toasts. */
export const t = (key: string, vars?: Record<string, string | number>) =>
  translate(useLang.getState().lang, key, vars);
