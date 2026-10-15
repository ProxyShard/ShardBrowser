export const STORAGE_KEY = "shardx.lang";

export type Lang = "en" | "zh" | "ru";

export function parseLang(value: unknown): Lang | null {
  if (value === "en" || value === "zh" || value === "ru") return value;
  if (typeof value === "string") {
    const lower = value.toLowerCase().trim();
    if (lower === "en" || lower === "zh" || lower === "ru") return lower as Lang;
  }
  return null;
}

export function resolveLang(
  storedValue: unknown,
  systemLanguage?: string | null,
): Lang {
  const parsedStored = parseLang(storedValue);
  if (parsedStored) return parsedStored;

  const sys = systemLanguage?.toLowerCase();
  if (sys?.startsWith("zh")) return "zh";
  if (sys?.startsWith("ru")) return "ru";
  return "en";
}

export function loadLang(
  storage: { getItem: (key: string) => string | null },
  systemLanguage?: string | null,
  key: string = STORAGE_KEY,
): Lang {
  try {
    const v = storage.getItem(key);
    return resolveLang(v, systemLanguage);
  } catch {
    return "en";
  }
}

export function safeWriteStorage(
  storage: { setItem: (key: string, value: string) => void },
  lang: Lang,
  key: string = STORAGE_KEY,
): boolean {
  try {
    storage.setItem(key, lang);
    return true;
  } catch {
    /* private mode / error */
    return false;
  }
}

export function translate(
  dicts: Record<Lang, Record<string, string>>,
  lang: Lang,
  key: string,
  vars?: Record<string, string | number>,
): string {
  let out = dicts[lang]?.[key] ?? dicts.en?.[key] ?? key;
  if (vars) {
    for (const [k, v] of Object.entries(vars)) {
      out = out.split(`{${k}}`).join(String(v));
    }
  }
  return out;
}
