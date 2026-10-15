import test from "node:test";
import assert from "node:assert";
import {
  parseLang,
  resolveLang,
  loadLang,
  safeWriteStorage,
  translate,
  STORAGE_KEY,
} from "../src/shared/i18n/engine.ts";

const mockDicts = {
  en: {
    "greeting": "Hello {name}",
    "multivar": "{a} and {a} with {b}",
    "enOnly": "English fallback",
  },
  zh: {
    "greeting": "你好 {name}",
  },
  ru: {
    "greeting": "Привет {name}",
  },
};

test("1. valid saved en/zh/ru wins over system locale", () => {
  assert.strictEqual(resolveLang("en", "ru-RU"), "en");
  assert.strictEqual(resolveLang("zh", "en-US"), "zh");
  assert.strictEqual(resolveLang("ru", "en-US"), "ru");
});

test("2. invalid/missing saved value detects ru/zh case-insensitively from system locale", () => {
  assert.strictEqual(resolveLang(null, "RU-ru"), "ru");
  assert.strictEqual(resolveLang(undefined, "zh-CN"), "zh");
  assert.strictEqual(resolveLang("invalid", "RU"), "ru");
  assert.strictEqual(resolveLang("", "ZH-HK"), "zh");
});

test("3. unsupported/undefined system locale falls back en", () => {
  assert.strictEqual(resolveLang(null, "fr-FR"), "en");
  assert.strictEqual(resolveLang(undefined, undefined), "en");
  assert.strictEqual(resolveLang(null, null), "en");
});

test("4. storage read failure falls back en", () => {
  const badStorage = {
    getItem: () => {
      throw new Error("Storage disabled");
    },
  };
  const resolved = loadLang(badStorage, "ru-RU", STORAGE_KEY);
  assert.strictEqual(resolved, "en");
});

test("4b. successful null read detects system locale (ru)", () => {
  const nullStorage = {
    getItem: () => null,
  };
  const resolved = loadLang(nullStorage, "ru-RU", STORAGE_KEY);
  assert.strictEqual(resolved, "ru");
});

test("5. save writes key/value and tolerates write failure", () => {
  const store = new Map<string, string>();
  const goodStorage = {
    setItem: (k: string, v: string) => store.set(k, v),
  };
  const success = safeWriteStorage(goodStorage, "zh", STORAGE_KEY);
  assert.strictEqual(success, true);
  assert.strictEqual(store.get(STORAGE_KEY), "zh");

  const badStorage = {
    setItem: () => {
      throw new Error("Quota exceeded / private mode");
    },
  };
  const handled = safeWriteStorage(badStorage, "ru", STORAGE_KEY);
  assert.strictEqual(handled, false);
});

test("6. selected-locale missing key falls back English", () => {
  const res = translate(mockDicts, "ru", "enOnly");
  assert.strictEqual(res, "English fallback");
});

test("7. missing everywhere returns key", () => {
  const res = translate(mockDicts, "ru", "nonexistent.key");
  assert.strictEqual(res, "nonexistent.key");
});

test("8. placeholders replace repeated occurrences and leave unspecified placeholders intact", () => {
  const res = translate(mockDicts, "en", "multivar", { a: "Alpha", b: "Beta" });
  assert.strictEqual(res, "Alpha and Alpha with Beta");

  const partialRes = translate(mockDicts, "en", "greeting", { unrelated: "test" });
  assert.strictEqual(partialRes, "Hello {name}");
});
