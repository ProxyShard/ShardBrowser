// Chrome's own bookmarks file (`Bookmarks` in a profile folder, no
// extension), read with its roots and folders. No imports: node --test runs it.

/** Chrome's root a bookmark sits in: "" is the bookmarks bar. */
export type ChromeRoot = "" | "other" | "synced";

/** One bookmark from the file: its name, its address and its place, as Chrome wrote them. */
export type ChromeMark = {
  title: string;
  url: string;
  root: ChromeRoot;
  /** The folders inside that root, outermost first. */
  path: string[];
};

/** What the file holds: the bookmarks to bring in, in Chrome's order, and how many it had. */
export type ChromeFile = {
  /** Every bookmark a profile can keep, repeats included. */
  marks: ChromeMark[];
  /** Every bookmark in the file. */
  found: number;
  /** Empty addresses and links that are neither a page nor a bookmarklet (mailto:, data:). */
  skipped: number;
  /** Folders holding at least one of `marks`, at any depth. */
  folders: number;
};

/** Chrome's roots in its bookmark manager's order, each with the root it goes to here. */
const ROOTS = new Map<string, ChromeRoot>([["bookmark_bar", ""], ["other", "other"], ["synced", "synced"]]);

type Node = { type?: unknown; name?: unknown; url?: unknown; children?: unknown };
type Placed = { node: unknown; root: ChromeRoot; path: string[] };
const isNode = (v: unknown): v is Node => typeof v === "object" && v !== null && !Array.isArray(v);
const isFolder = (v: unknown): v is Node & { children: unknown[] } => isNode(v) && Array.isArray(v.children);
const nameOf = (n: Node) => (typeof n.name === "string" ? n.name.trim() : "");

/** An address a profile keeps as a bookmark: a page (a scheme with "//": https,
 *  chrome, file) or a bookmarklet with its script. Not mailto: or data:. */
const keepable = (url: string) => /^[a-z][a-z0-9+.-]*:\/\//i.test(url) || /^javascript:\s*\S/i.test(url);

/** The file's bookmarks with their places, or null when the text is not
 *  Chrome's bookmarks file ({"roots": {"bookmark_bar": …}, "version": 1}). */
export function readChromeBookmarks(text: string): ChromeFile | null {
  let doc: unknown;
  try { doc = JSON.parse(text); } catch { return null; }
  if (!isNode(doc) || !isNode((doc as { roots?: unknown }).roots)) return null;
  const roots = (doc as { roots: Record<string, unknown> }).roots;
  const known = [...ROOTS.keys()];
  if (!known.some((k) => isFolder(roots[k]))) return null;
  // The known roots first, then any other a later Chrome adds: a folder of its name in Other bookmarks.
  const keys = [...known.filter((k) => k in roots), ...Object.keys(roots).filter((k) => !known.includes(k))];

  // Depth first, each folder's children in their order: Chrome's own order.
  const stack: Placed[] = [];
  const pushChildren = (folder: { children: unknown[] }, root: ChromeRoot, path: string[]) => {
    for (let i = folder.children.length - 1; i >= 0; i--) stack.push({ node: folder.children[i], root, path });
  };
  for (const k of [...keys].reverse()) {
    const node = roots[k];
    if (!isFolder(node)) continue;
    const root = ROOTS.get(k);
    if (root !== undefined) pushChildren(node, root, []);
    else pushChildren(node, "other", [nameOf(node) || k]);
  }

  const marks: ChromeMark[] = [];
  const folders = new Set<string>();
  let found = 0;
  let skipped = 0;
  while (stack.length > 0) {
    const { node, root, path } = stack.pop()!;
    if (isFolder(node) && node.type !== "url") {
      // An unnamed folder adds no level: its bookmarks join the folder around it.
      pushChildren(node, root, nameOf(node) ? [...path, nameOf(node)] : path);
      continue;
    }
    if (!isNode(node) || node.type !== "url") continue;
    found++;
    const url = typeof node.url === "string" ? node.url.trim() : "";
    if (!url || !keepable(url)) { skipped++; continue; }
    marks.push({ title: nameOf(node), url, root, path });
    for (let i = 1; i <= path.length; i++) folders.add(JSON.stringify([root, ...path.slice(0, i)]));
  }
  return { marks, found, skipped, folders: folders.size };
}

/** An address as it compares: the parsed URL ("https://a.com" is "https://a.com/"), else as written. */
export function urlKey(url: string): string {
  const u = url.trim();
  try { return new URL(u).href; } catch { return u; }
}

/** A bookmark's place: its root, its folders and its address. */
const placeKey = (root: string | undefined, path: readonly string[], url: string) =>
  JSON.stringify([root ?? "", ...path.map((s) => s.trim()).filter(Boolean), urlKey(url)]);

/** The marks not here yet, each place once. `already`: here before the
 *  import, in the same folder; `repeats`: later copies in the same folder of
 *  the file. One Chrome has in two folders comes in to both. */
export function newMarks(
  marks: readonly ChromeMark[],
  existing: Iterable<{ url: string; root?: string; path?: readonly string[] }>,
): { fresh: ChromeMark[]; already: number; repeats: number } {
  const here = new Set<string>();
  for (const b of existing) here.add(placeKey(b.root, b.path ?? [], b.url));
  const seen = new Set<string>();
  const fresh: ChromeMark[] = [];
  let already = 0;
  let repeats = 0;
  for (const m of marks) {
    const key = placeKey(m.root, m.path, m.url);
    if (here.has(key)) { already++; continue; }
    if (seen.has(key)) { repeats++; continue; }
    seen.add(key);
    fresh.push(m);
  }
  return { fresh, already, repeats };
}
