import { useLayoutEffect, useRef } from "react";
import { useT } from "../i18n";
import { announce } from "../model/announce";

/// Selection mode (owner, 2026-10-01). A list that selects keeps its select
/// column before Name in every state (owner, 2026-10-07, item 9: a box laid
/// over the name hid its start), and at rest a row's box shows on its hover
/// only, the header's Select all on the list's; once a row is selected the
/// list is in selection mode: every box shows, and the bulk bar comes up. The list carries
/// one class for it (`has-selection`, beside `list-select`), and the kit's
/// tokens.css draws the rest from it, never per row.

/** The select column's grid track: the 28 px box column with its 10 px gap,
 *  one shared length for the header and every row (`--list-select`, the
 *  kit's tokens.css). */
export const SELECT_TRACK = "var(--list-select)";

/// What entering and leaving selection mode say, through the app's live
/// region: "3 selected", then "Selection cleared". Leaving, a focused box
/// that hides never keeps the keyboard: its row takes it (Select all keeps
/// it: a focused box stays shown).
export function useSelectionMode(listRef: React.RefObject<HTMLElement | null>, count: number): void {
  const t = useT();
  const open = count > 0;
  const was = useRef(open);
  useLayoutEffect(() => {
    if (was.current === open) return;
    was.current = open;
    announce(open ? t("bulkActionsBar.selectedCount", { n: count }) : t("listSelection.cleared"));
    if (open) return;
    const box = listRef.current;
    const at = document.activeElement;
    if (!box || !(at instanceof HTMLElement) || !box.contains(at) || !at.closest(".list-select-box")) return;
    if (at.closest("[data-list-header]")) return;
    const row = at.closest<HTMLElement>("[role='row'][aria-selected]")
      ?? box.querySelector<HTMLElement>("[role='row'][aria-selected][tabindex='0']");
    row?.focus({ preventScroll: true });
  }, [open, count, t, listRef]);
}
