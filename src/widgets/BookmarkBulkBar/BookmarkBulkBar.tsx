import { Button, IconButton, Tooltip, useLatched } from "@proxyshard/shardx-ui-kit";
import { CloseIcon, DeleteIcon } from "../../shared/icons";
import { BulkBarFrame } from "../../shared/ui/BulkBarFrame";
import { useBookmarks, type Bookmark } from "../../entities/bookmark";
import { teamRefusal, useRightsChange } from "../../entities/space";
import { useT } from "../../shared/i18n";

/// The Bookmarks selection bar, as the Trash's: the count, Delete (it asks
/// first, naming how many) and the X. It acts on the picks the list shows.
export function BookmarkBulkBar({ selected, onClear, onClosedWithFocus }: {
  selected: Bookmark[];
  onClear: () => void;
  /** Focus was in the bar as it closed: where it goes back to. */
  onClosedWithFocus?: () => void;
}) {
  const t = useT();
  const n = selected.length;
  // Through its exit it keeps the count and the bookmarks it showed.
  const label = useLatched(n > 0 ? t("bulkActionsBar.selectedCount", { n }) : null) ?? "";
  const shown = useLatched(n > 0 ? selected : null) ?? [];
  useRightsChange();
  const noDelete = teamRefusal("delete");
  return (
    <BulkBarFrame open={n > 0} label={label} onClosedWithFocus={onClosedWithFocus}>
      {() => (
        <>
          <span className="mr-1.5 text-secondary font-medium text-text-strong-950 tabular-nums">{label}</span>
          <Tooltip content={noDelete ?? undefined}>
            <Button
              variant="error"
              mode="ghost"
              size="3xsmall"
              leftIcon={<DeleteIcon className="size-3.5" />}
              disabled={!!noDelete}
              aria-description={noDelete ?? undefined}
              onClick={() => void useBookmarks.getState().removeMany(shown)}
            >
              {t("bookmarks.delete")}
            </Button>
          </Tooltip>
          <IconButton label={t("commands.clearSelection")} icon={<CloseIcon className="size-3.5" />} size="3xsmall" onClick={onClear} />
        </>
      )}
    </BulkBarFrame>
  );
}
