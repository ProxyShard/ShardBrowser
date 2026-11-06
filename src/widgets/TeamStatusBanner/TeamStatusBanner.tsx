import { cn } from "@proxyshard/shardx-ui-kit";
import { activeTeam, useSpace } from "../../entities/space";
import { teamError } from "../../entities/team";
import { AlertCircleIcon, SyncOffIcon } from "../../shared/icons";
import { intlLocale, useLang, useT } from "../../shared/i18n";
import { HAS_CAPTIONS } from "../WindowControls";

/// A team's lists while its server is not answering, or refused: one slim
/// tile over the page, its bold title the word, gone once a read gets through.
export function TeamStatusBanner() {
  const t = useT();
  const lang = useLang((s) => s.lang);
  const st = useSpace((s) => s.state);
  const status = activeTeam(st) ? st?.team_status : undefined;
  if (status?.state !== "offline" && status?.state !== "refused") return null;
  const offline = status.state === "offline";
  const at = status.synced_at
    ? new Intl.DateTimeFormat(intlLocale(lang), { dateStyle: "medium", timeStyle: "short" }).format(status.synced_at * 1000)
    : null;
  const Icon = offline ? SyncOffIcon : AlertCircleIcon;
  const title = offline ? t("teamStatus.offlineTitle") : t("teamStatus.refusedTitle");
  return (
    <div
      role="status"
      data-team-status={status.state}
      className={cn(
        // forced-edge: forced colors drop the tint, an edge keeps the tile.
        "mx-gutter mt-2.5 flex shrink-0 items-start gap-2 rounded-control px-3 py-2 text-secondary text-text-strong-950 forced-edge",
        offline ? "bg-warning-tint" : "bg-error-tint",
        // The caption buttons' room on Windows and Linux (#142), as PageHeader keeps.
        HAS_CAPTIONS && "me-[150px]",
      )}
    >
      <Icon aria-hidden="true" className={cn("mt-0.5 size-3.5 shrink-0", offline ? "text-warning-text" : "text-error-text")} />
      <p className="m-0 min-w-0 [overflow-wrap:anywhere]">
        <strong className="font-medium">{title}</strong>
        {/* No space after a full-width stop (Chinese "。"). */}
        {/[。！？]$/.test(title) ? "" : " "}
        <span className="text-text-sub-600">
          {offline
            ? at ? t("teamStatus.offlineSince", { time: at }) : t("teamStatus.offlineNever")
            : status.error ? teamError(status.error) : t("teamStatus.refusedNoReason")}
        </span>
      </p>
    </div>
  );
}
