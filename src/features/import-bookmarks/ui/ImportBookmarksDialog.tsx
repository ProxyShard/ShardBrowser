import { useMemo } from "react";
import { DialogModal, ProgressBar, useLatched } from "@proxyshard/shardx-ui-kit";
import { useT } from "../../../shared/i18n";
import { useDialogLife } from "../../../shared/lib/useDialogLife";
import { BookmarkFolderSelect, useBookmarks } from "../../../entities/bookmark";
import { newMarks } from "../lib/chromeBookmarks";
import { useImportBookmarks } from "../model/store";

/// Import from Chrome: what the picked file holds, where it will show, and
/// the import itself, with its progress and any refusal in place.
export function ImportBookmarksDialog({ folders }: { folders: string[] }) {
  const open = useImportBookmarks((s) => s.open);
  // Stays mounted through its exit (FEEL-01).
  const life = useDialogLife(open);
  if (!life.mounted) return null;
  return <ImportDialog key={life.session} open={open} folders={folders} />;
}

function ImportDialog({ open, folders }: { open: boolean; folders: string[] }) {
  const t = useT();
  const fileName = useImportBookmarks((s) => s.fileName);
  const file = useImportBookmarks((s) => s.file);
  const problem = useImportBookmarks((s) => s.problem);
  const folder = useImportBookmarks((s) => s.folder);
  const progress = useImportBookmarks((s) => s.progress);
  const stopping = useImportBookmarks((s) => s.stopping);
  const failure = useImportBookmarks((s) => s.failure);
  const stopped = useImportBookmarks((s) => s.stopped);
  const { pick, setFolder, run, close } = useImportBookmarks.getState();
  const items = useBookmarks((s) => s.items);
  const busy = progress != null;

  const live = useMemo(() => (file ? newMarks(file.marks, items) : null), [file, items]);
  // The counts as they were when the import began, until it ends.
  const plan = useLatched(busy ? null : live) ?? live;
  const fresh = plan?.fresh.length ?? 0;

  const confirmLabel = !file
    ? t("importBookmarks.pickAnother")
    : fresh === 0 ? t("importBookmarks.import")
    : fresh === 1 ? t("importBookmarks.importOne")
    : t("importBookmarks.importMany", { n: fresh });
  const footerNote = failure ? (
    <span role="alert" className="text-error-text">
      {failure.done > 0 ? t("importBookmarks.stoppedAt", { done: failure.done, total: failure.total }) : t("importBookmarks.nothingImported")} {failure.reason}
    </span>
  ) : stopped && !busy ? (
    <span role="status">{t("importBookmarks.stoppedByYou", { done: stopped.done, total: stopped.total })}</span>
  ) : file && !busy && fresh === 0 ? t("importBookmarks.nothingNew") : undefined;

  return (
    <DialogModal
      open={open}
      onClose={close}
      title={t("importBookmarks.title")}
      confirmLabel={confirmLabel}
      onConfirm={() => void (file ? run() : pick())}
      isLoading={busy}
      isDisabled={file != null && fresh === 0}
      cancelLabel={stopping ? t("importBookmarks.stopping") : busy ? t("importBookmarks.stop") : t("bookmarks.cancel")}
      onCancel={close}
      footerNote={footerNote}
    >
      <div className="flex w-full flex-col gap-3 py-4">
        {!file ? (
          <>
            <p role="alert" className="m-0 text-body text-error-text">{problem}</p>
            <p className="m-0 text-secondary text-text-sub-600">
              {t.rich("importBookmarks.whereHint", { code: (s) => <code className="text-mono [overflow-wrap:anywhere]">{s}</code> })}
            </p>
          </>
        ) : (
          <>
            <p className="m-0 text-body text-text-strong-950" data-import-summary>
              {file.found === 0
                ? t("importBookmarks.foundNone", { name: fileName })
                : file.found === 1 ? t("importBookmarks.foundOne", { name: fileName }) : t("importBookmarks.foundMany", { n: file.found, name: fileName })}
              {plan && plan.already > 0 && ` ${plan.already === 1 ? t("importBookmarks.alreadyOne") : t("importBookmarks.alreadyMany", { n: plan.already })}`}
            </p>
            {file.skipped > 0 && (
              <p className="m-0 text-secondary text-text-sub-600" data-import-skipped>
                {file.skipped === 1 ? t("importBookmarks.skippedOne") : t("importBookmarks.skippedMany", { n: file.skipped })}
              </p>
            )}
            {plan && plan.repeats > 0 && (
              <p className="m-0 text-secondary text-text-sub-600" data-import-repeats>
                {plan.repeats === 1 ? t("importBookmarks.repeatsOne") : t("importBookmarks.repeatsMany", { n: plan.repeats })}
              </p>
            )}
            <BookmarkFolderSelect label={t("importBookmarks.showsIn")} value={folder} folders={folders} onChange={setFolder} disabled={busy} />
            <p className="m-0 text-secondary text-text-soft-400" data-import-folders>
              {file.folders === 0 ? t("importBookmarks.foldersNone") : t("importBookmarks.foldersKept")}
            </p>
            {progress && progress.total > 0 && (
              <div className="flex flex-col gap-1.5" data-import-progress>
                <span className="text-secondary text-text-sub-600">{t("importBookmarks.progress", { done: progress.done, total: progress.total })}</span>
                <ProgressBar aria-label={t("importBookmarks.title")} value={progress.done} max={progress.total} />
              </div>
            )}
          </>
        )}
      </div>
    </DialogModal>
  );
}
