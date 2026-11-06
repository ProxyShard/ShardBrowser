import { useEffect, useRef } from "react";
import { Button, CopyButton, cn } from "@proxyshard/shardx-ui-kit";
import { AlertCircleIcon } from "../../shared/icons";
import { useT } from "../../shared/i18n";
import { clip } from "../../shared/lib/clipboard";
import { fmtBytesProgress } from "../../shared/lib/format";
import { useRunningCount } from "../../entities/profile";
import { HAS_CAPTIONS } from "../WindowControls";
import { joined, reason, retryEngineUpdate, stopAndFinish, useEngineUpdate } from "./engineUpdate";

/// An engine update that stopped with the previous engine kept (#122): one
/// slim tile over the page, as the team's status is, with Try again. Stopped
/// by running profiles, it goes in once the last one closes.
export function EngineUpdateBanner() {
  const t = useT();
  const error = useEngineUpdate((s) => s.error);
  const staged = useEngineUpdate((s) => s.staged);
  const retrying = useEngineUpdate((s) => s.retrying);
  const progress = useEngineUpdate((s) => s.progress);
  const running = useRunningCount();
  const before = useRef(running);
  useEffect(() => {
    const was = before.current;
    before.current = running;
    if (!(was > 0 && running === 0)) return;
    // A browser may still be exiting when its profile shows stopped: a few tries, further apart.
    let gone = false;
    void (async () => {
      for (const ms of [800, 2500, 6000]) {
        await new Promise((r) => setTimeout(r, ms));
        if (gone || useEngineUpdate.getState().error?.stage !== "busy") return;
        await retryEngineUpdate(true);
      }
    })();
    return () => { gone = true; };
  }, [running]);
  if (!error) return null;
  const title = retrying ? t("engineUpdate.updatingTitle") : t("engineUpdate.title");
  // The fact first, then what to do.
  const detail = !retrying
    ? [staged ? t("engineUpdate.staged") : "", reason(t, error), t("engineUpdate.kept")].filter(Boolean).reduce(joined)
    : staged ? t("engineUpdate.finishing")
      : progress?.phase === "download" ? t("engineUpdate.downloaded", { progress: fmtBytesProgress(progress.received, progress.total) })
        : progress ? t("firstRunGate.extracting") : t("firstRunGate.contactingCdn");
  // Try again would meet the same profiles: stopping them is the way on.
  const stopFirst = error.stage === "busy" && running > 0 && !retrying;
  return (
    <div
      data-engine-update={error.stage}
      // On Windows and Linux the caption buttons sit over this row (#142): their room, as PageHeader keeps.
      className={cn("mx-gutter mt-2.5 flex shrink-0 items-center gap-2 rounded-control bg-warning-tint px-3 py-1 text-secondary text-text-strong-950 forced-edge", HAS_CAPTIONS && "me-[150px]")}
    >
      <div className="flex min-w-0 flex-1 items-start gap-2 py-1">
        <AlertCircleIcon aria-hidden="true" className="mt-0.5 size-3.5 shrink-0 text-warning-text" />
        <p role="status" className="m-0 min-w-0 [overflow-wrap:anywhere]">
          <strong className="font-medium">{title}</strong>
          {/[。！？]$/.test(title) ? "" : " "}
          <span className="text-text-sub-600">{detail}</span>
        </p>
      </div>
      {/* The backend's own words, with the paths and numbers, for a report. */}
      {error.message && <CopyButton size="2xsmall" className="shrink-0" label={t("kit.copyDetails")} onCopy={async () => { await clip.write(error.message); }} />}
      {stopFirst ? (
        <Button variant="neutral" mode="stroke" size="2xsmall" className="shrink-0" onClick={() => void stopAndFinish()}>
          {running === 1 ? t("extensions.stopAndUpdateOne", { n: running }) : t("extensions.stopAndUpdateMany", { n: running })}
        </Button>
      ) : (
        <Button variant="neutral" mode="stroke" size="2xsmall" className="shrink-0" isLoading={retrying} onClick={() => void retryEngineUpdate()}>
          {t("firstRunGate.retry")}
        </Button>
      )}
    </div>
  );
}
