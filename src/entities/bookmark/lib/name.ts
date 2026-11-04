import { readableUrl } from "../../../shared/lib/utils";
import type { Bookmark } from "../model/types";

/** A bookmark's name: its title, else its address in a form to read. The
 *  backend gives an untitled bookmark its own address as its title, so a
 *  title that is just the address is read the same way (no scheme, no
 *  percent-encoding). A title a person typed stays as typed. */
export function bookmarkName(b: Pick<Bookmark, "title" | "url">): string {
  const title = b.title.trim();
  return title && title !== b.url.trim() ? title : readableUrl(b.url);
}

/** The bookmark folders it sits in, outermost first, as one line; "" when none. */
export const bookmarkPath = (b: Pick<Bookmark, "path">): string => (b.path ?? []).join(" / ");
