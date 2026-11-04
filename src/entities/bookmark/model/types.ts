export type Bookmark = {
  id: string;
  title: string;
  url: string;
  /** Launcher folder this belongs to; empty means every profile. */
  folder: string;
  /** Chrome's root it goes in: "other", "synced", or absent for the bookmarks bar. */
  root?: string;
  /** Bookmark folders it sits in inside that root, outermost first. */
  path?: string[];
};
