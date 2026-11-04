// node --test: reading Chrome's bookmarks file (`npm run test:unit`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { newMarks, readChromeBookmarks, urlKey, type ChromeMark, type ChromeRoot } from "./chromeBookmarks.ts";

const url = (name: string, u: string) => ({ type: "url", name, url: u, date_added: "13300000000000000", id: "1" });
const folder = (name: string, children: unknown[]) => ({ type: "folder", name, children });
const file = (roots: Record<string, unknown>) => JSON.stringify({ checksum: "", roots, version: 1 });
const mark = (title: string, u: string, path: string[] = [], root: ChromeRoot = ""): ChromeMark => ({ title, url: u, root, path });
/** A mark as "root:Folder/Sub/Title" ("bar" for the bookmarks bar), to read a whole list at a glance. */
const placed = (m: ChromeMark) => `${m.root || "bar"}:${[...m.path, m.title].join("/")}`;

test("each bookmark keeps its root and its folders inside folders, in Chrome's order", () => {
  const text = file({
    synced: folder("Mobile bookmarks", [url("Phone", "https://m.example/")]),
    bookmark_bar: folder("Bookmarks bar", [
      url("A", "https://a.example/"),
      folder("Work", [url("B", "https://b.example/"), folder("Deep", [url("C", "https://c.example/")]), url("B2", "https://b2.example/")]),
      url("D", "https://d.example/"),
    ]),
    other: folder("Другие закладки", [url(" E ", " https://e.example/ "), folder(" Shop ", [url("F", "https://f.example/")])]),
  });
  const got = readChromeBookmarks(text);
  assert.ok(got);
  // The roots' own names (in Chrome's language) are not folders: the root says where.
  assert.deepEqual(got.marks.map(placed), [
    "bar:A", "bar:Work/B", "bar:Work/Deep/C", "bar:Work/B2", "bar:D",
    "other:E", "other:Shop/F",
    "synced:Phone",
  ]);
  assert.deepEqual(got.marks[5], mark("E", "https://e.example/", [], "other"));
  assert.equal(got.found, 8);
  assert.equal(got.skipped, 0);
  // Work, Work/Deep and Shop.
  assert.equal(got.folders, 3);
});

test("a root a later Chrome adds becomes a folder of its name in Other bookmarks; an unnamed folder adds no level", () => {
  const got = readChromeBookmarks(file({
    bookmark_bar: folder("", [folder("", [url("A", "https://a.example/")])]),
    other: folder("", [url("B", "https://b.example/")]),
    synced: folder("", []),
    tablet: folder("Tablet bookmarks", [url("T", "https://t.example/")]),
    // Not a root Chrome knows, and inherited names are not roots either.
    toString: folder("", [url("X", "https://x.example/")]),
  }));
  assert.ok(got);
  assert.deepEqual(got.marks.map(placed), ["bar:A", "other:B", "other:Tablet bookmarks/T", "other:toString/X"]);
});

test("bookmarklets come in as written; empty addresses, mailto: and data: are counted and skipped", () => {
  const got = readChromeBookmarks(file({
    bookmark_bar: folder("Bookmarks bar", [
      url("Title", "javascript:alert(document.title)"),
      url("Blank", ""),
      folder("Tools", [url("Mail", "mailto:me@example.com"), folder("Deeper", [url("Run", "JavaScript: void(0)"), url("Data", "data:text/html,hi")])]),
      url("Bare", "javascript:"),
      { type: "url", name: "No address" },
      url("Settings", "chrome://settings/"),
    ]),
    other: folder("Other bookmarks", []),
  }));
  assert.ok(got);
  assert.deepEqual(got.marks, [
    mark("Title", "javascript:alert(document.title)"),
    mark("Run", "JavaScript: void(0)", ["Tools", "Deeper"]),
    mark("Settings", "chrome://settings/"),
  ]);
  assert.equal(got.found, 8);
  assert.equal(got.skipped, 5);
  // Tools and Tools/Deeper hold the bookmarklet.
  assert.equal(got.folders, 2);
});

test("a file that is not Chrome's bookmarks file is refused", () => {
  assert.equal(readChromeBookmarks("not json"), null);
  assert.equal(readChromeBookmarks("[]"), null);
  assert.equal(readChromeBookmarks(JSON.stringify({ bookmarks: [{ title: "x", url: "https://x.example" }] })), null);
  assert.equal(readChromeBookmarks(JSON.stringify({ roots: { mine: [] } })), null);
  // Chrome's shape with nothing in it is still Chrome's file.
  assert.deepEqual(readChromeBookmarks(file({ bookmark_bar: folder("Bookmarks bar", []) })), { marks: [], found: 0, skipped: 0, folders: 0 });
});

test("the same address in two places comes in to both; twice in one folder, once", () => {
  const got = readChromeBookmarks(file({
    bookmark_bar: folder("Bookmarks bar", [
      url("GitHub", "https://github.com/"),
      folder("Work", [url("GitHub", "https://github.com"), url("GitHub again", "https://github.com/"), folder("Code", [url("GitHub", "https://github.com/")])]),
      url("Empty", ""),
    ]),
    // A folder named like a bar folder, in another root, is another place.
    other: folder("Other bookmarks", [url("GitHub", "https://github.com/"), folder("Work", [url("GitHub", "https://github.com/")])]),
  }));
  assert.ok(got);
  assert.equal(got.found, 7);
  assert.equal(got.skipped, 1);
  const { fresh, already, repeats } = newMarks(got.marks, []);
  assert.deepEqual(fresh.map(placed), ["bar:GitHub", "bar:Work/GitHub", "bar:Work/Code/GitHub", "other:GitHub", "other:Work/GitHub"]);
  assert.deepEqual({ already, repeats }, { already: 0, repeats: 1 });
  // found = fresh + already + repeats + skipped: every bookmark is accounted for.
  assert.equal(fresh.length + already + repeats + got.skipped, got.found);
});

test("one already here counts only in the same root and folder, so a second import adds nothing", () => {
  const marks = [
    mark("A", "https://a.example/"),
    mark("B", "https://pixelscan.net/", ["Tools"]),
    mark("A again", "https://a.example/"),
    mark("C", "https://c.example/x", ["Tools", "Deep"]),
    mark("B again", "https://pixelscan.net", ["Tools"]),
    mark("A elsewhere", "https://a.example/", [], "other"),
  ];
  // pixelscan on the bar is not the one in Tools; the launcher's own list may have no root or path.
  const here = [{ url: "https://pixelscan.net" }, { url: "https://c.example/x", path: ["Tools", "Deep"] }, { url: "https://other.example", root: "", path: [] }];
  const { fresh, already, repeats } = newMarks(marks, here);
  assert.deepEqual(fresh.map(placed), ["bar:A", "bar:Tools/B", "other:A elsewhere"]);
  assert.deepEqual({ already, repeats }, { already: 1, repeats: 2 });
  // As they were saved (a bar bookmark with no root): every one is here now.
  const saved = fresh.map((m) => ({ url: m.url, path: m.path, ...(m.root ? { root: m.root } : {}) }));
  assert.deepEqual(newMarks(marks, [...here, ...saved]).fresh, []);
});

test("addresses compare as URLs, and an unparsable one as written", () => {
  assert.equal(urlKey("https://Example.com"), "https://example.com/");
  assert.equal(urlKey(" https://a.example/x "), "https://a.example/x");
  assert.notEqual(urlKey("https://a.example/x"), urlKey("https://a.example/x/"));
  assert.equal(urlKey("not a url"), "not a url");
});
