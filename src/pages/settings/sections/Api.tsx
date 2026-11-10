import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Button, Switch } from "@proxyshard/shardx-ui-kit";
import { DownloadIcon } from "../../../shared/icons";
import { DetailRow, DetailsCard } from "../../../shared/ui/DetailsCard";
import { NumField } from "../../../shared/ui/NumField";
import { toast } from "../../../shared/model/toast";
import { confirmModal } from "../../../shared/lib/confirm";
import { withUtm } from "../../../shared/lib/utils";
import { apiRegenerateToken, apiServerAtStart, mcpDownload, type ApiInfo, type Settings } from "../../../entities/settings";
import { LoadError } from "../../../shared/ui/ListStates";
import { useT } from "../../../shared/i18n";
import { SettingRow, SettingsGroup, type SettingsProps } from "../SettingRow";
import { holdSave } from "../useSettingsDraft";

// The one docs page there is (plan ruling 8). A real href gives the link its
// role and a copyable target; the click opens the system browser.
const API_DOCS = "https://docs.shardx.com/eng/shardx-launcher-api/binding-and-lifecycle?fallback=true";
const LOOPBACK = "127.0.0.1";

export function ApiSection({ s, saved, setS, api, setApi, apiError, retryApi }: SettingsProps & {
  /** What is on disk: the restart note speaks of saved settings, not of the draft. */
  saved: Settings;
  api: ApiInfo | null;
  setApi: (api: ApiInfo) => void;
  apiError: string | null;
  retryApi: () => void;
}) {
  const t = useT();
  // The running server is the one this window first read: saving does not
  // restart it, so a switch or port that differs takes effect after a
  // restart (O6; no Restart button without a relaunch command, ruling 7).
  const running = apiServerAtStart();
  const restartNeeded = !!running && ((saved.api_enabled ?? true) !== running.enabled || (saved.api_port ?? 40325) !== running.port);
  // Every script and MCP client on the old token stops working: ask first (WRITE-23).
  const regenToken = async () => {
    const yes = await confirmModal({
      title: t("settings.regenerateTitle"),
      message: t("settings.regenerateMessage"),
      buttons: [
        { label: t("confirm.cancel"), value: false },
        { label: t("settings.apiRegenerateBtn"), value: true, danger: true },
      ],
    });
    if (yes !== true) return;
    try { setApi(await apiRegenerateToken()); toast.ok(t("settings.tokenRegenerated")); }
    catch (e) { toast.fail({ title: t("settings.tokenRegenerateFailed"), detail: e }); }
  };

  const [mcpBusy, setMcpBusy] = useState(false);
  // The MCP server's source goes to a folder; the user installs it and sets up the client.
  const downloadMcp = async () => {
    const dir = await open({ directory: true, title: t("settings.mcpDownloadDialogTitle") });
    if (typeof dir !== "string") return;
    setMcpBusy(true);
    try { toast.ok(t("settings.mcpDownloaded", { path: await mcpDownload(dir) })); }
    catch (e) { toast.fail({ title: t("settings.mcpDownloadFailed"), detail: e }); }
    finally { setMcpBusy(false); }
  };

  // The address is code (TYPE-08), inside the one line of help.
  const apiHelp = t("settings.apiHelp");
  const at = apiHelp.indexOf(LOOPBACK);
  const helpText = at < 0 ? apiHelp : <>{apiHelp.slice(0, at)}<code>{LOOPBACK}</code>{apiHelp.slice(at + LOOPBACK.length)}</>;
  const restart = restartNeeded && <p data-restart-note className="m-0 text-help text-warning-text">{t("settings.restartNote")}</p>;

  return (
    <div className="flex flex-col gap-[32px]">
      <SettingsGroup title={t("settings.apiTitle")}>
        <SettingRow
          id="api"
          title={t("settings.apiEnableLabel")}
          help={
            <>
              {helpText}{" "}
              <a
                href={API_DOCS}
                className="text-primary-text underline underline-offset-2"
                onClick={(e) => { e.preventDefault(); openUrl(withUtm(API_DOCS)).catch(() => {}); }}
              >
                {t("settings.apiLearnMore")}
              </a>
            </>
          }
          control={({ labelId, helpId }) => (
            <Switch aria-labelledby={labelId} aria-describedby={helpId} checked={s.api_enabled ?? true} onChange={(on) => setS({ ...s, api_enabled: on })} />
          )}
        />
        <SettingRow
          id="port"
          title={t("settings.apiPortLabel")}
          control={({ labelId, helpId }) => (
            <div className="w-[120px]">
              <NumField
                aria-labelledby={labelId}
                aria-describedby={helpId}
                value={s.api_port ?? 40325}
                min={1024}
                max={65535}
                onChange={(v) => setS({ ...s, api_port: v })}
                // Text that is not a port holds Save, with the reason in the bar.
                onInvalid={(bad) => holdSave("api_port", bad ? t("numField.invalidRange", { min: 1024, max: 65535 }) : null)}
              />
            </div>
          )}
          below={restart || undefined}
        />
        {/* A refresh that failed says so, even while older details are shown (PC-129). */}
        {apiError && (
          <div className="py-3">
            <LoadError what={t("otherStates.apiDetails")} error={apiError} onRetry={retryApi} headingLevel={3} />
          </div>
        )}
      </SettingsGroup>
      {/* The server's facts as a details card (owner, "more air"): the
          address and the token a person copies, and whether it runs. */}
      {api && (
        <div className="flex flex-col gap-[10px]">
          <DetailsCard id="api" title={t("settings.apiDetailsTitle")}>
            <DetailRow id="base-url" label={t("settings.apiBaseUrlLabel")} copy={{ value: api.base_url }}>
              <span className="mono">{api.base_url}</span>
            </DetailRow>
            <DetailRow id="token" label={t("settings.apiTokenLabel")} copy={{ value: api.token, secret: true }}>
              <span className="mono">{masked(api.token)}</span>
            </DetailRow>
            {running && (
              <DetailRow id="status" label={t("settings.apiStatusLabel")}>
                <span className={running.enabled ? "text-success-text" : "text-text-soft-400"}>
                  {running.enabled ? t("settings.apiStatusRunning") : t("settings.apiStatusOff")}
                </span>
              </DetailRow>
            )}
          </DetailsCard>
          <div className="flex items-center justify-between gap-4">
            <p className="m-0 min-w-0 text-help text-text-sub-600">{t("settings.apiAuthHeaderHint")}<code>Authorization: Bearer &lt;token&gt;</code></p>
            <Button className="shrink-0" variant="neutral" mode="stroke" size="xsmall" onClick={() => void regenToken()}>
              {t("settings.apiRegenerateBtn")}
            </Button>
          </div>
        </div>
      )}
      <SettingsGroup title={t("settings.mcpTitle")}>
        <SettingRow
          id="mcp"
          title={t("settings.mcpDownloadBtn")}
          help={t("settings.mcpHelp")}
          control={() => (
            <Button
              variant="neutral"
              mode="stroke"
              size="xsmall"
              // "Download" beside its row's name; the name says what (label in name).
              aria-label={t("settings.mcpDownloadBtn")}
              leftIcon={<DownloadIcon className="size-3.5" />}
              isLoading={mcpBusy}
              disabled={mcpBusy}
              onClick={() => void downloadMcp()}
            >
              {t("settings.mcpDownloadShort")}
            </Button>
          )}
        />
      </SettingsGroup>
    </div>
  );
}

/** The token as the card shows it: its start and end, enough to tell two
 *  apart; Copy takes the whole of it. */
function masked(token: string): string {
  return token.length > 16 ? `${token.slice(0, 8)}…${token.slice(-4)}` : "••••••••";
}
