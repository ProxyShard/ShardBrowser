import { invoke } from "../../../shared/api/invoke";
import type { Bookmark } from "./types";

export const bookmarkList = () => invoke<Bookmark[]>("bookmark_list");
export const bookmarkSave = (entry: Bookmark) => invoke<Bookmark>("bookmark_save", { entry });
export const bookmarkDelete = (id: string) => invoke("bookmark_delete", { id });
export const bookmarkDeleteMany = (ids: string[]) => invoke("bookmark_delete_many", { ids });
