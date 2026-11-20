import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Button } from "@proxyshard/shardx-ui-kit";
import { GH_REPO_URL, SITE_HOST, siteUrl } from "../../../shared/lib/utils";
import type { RtUpdate } from "../../../shared/types";
import { useWhatsNew } from "../../../shared/model/whatsNew";
import { toast } from "../../../shared/model/toast";
import { useOnboarding } from "../../../features/onboarding";
import { takeTheTour } from "../../../features/tour";
import { useT } from "../../../shared/i18n";
import { DetailRow, DetailsCard } from "../../../shared/ui/DetailsCard";
import { askLauncherUpdate } from "../../../features/launcher-update";

const ABOUT_SITE = siteUrl("settings_about");

export function AboutSection() {
  const t = useT();
  const openWhatsNew = useWhatsNew((s) => s.setOpen);
  const tipsOff = useOnboarding((s) => s.tipsOff);
  const replayRef = useRef<HTMLButtonElement>(null);
  const [info, setInfo] = useState<RtUpdate | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    invoke<RtUpdate>("launcher_update_check").then(setInfo).catch(() => setFailed(true));
  }, []);

  const releaseUrl = info?.update_available ? info.release_url : null;
  const status = failed
    ? t("settings.aboutOffline")
    : info === null
      ? t("settings.aboutChecking")
      : info.update_available
        ? t("settings.aboutUpdate", { v: info.latest ?? "" })
        : info.latest
          ? t("settings.aboutUpToDate")
          : t("settings.aboutOffline");

  // Page tips back on. This page counts as seen, so the press does not start
  // a tip over you; the toast says what happened, and the keyboard moves to
  // the button beside, as this one leaves.
  const tipsOn = () => {
    const onboarding = useOnboarding.getState();
    onboarding.markPageSeen("settings");
    onboarding.setTipsOff(false);
    toast.ok(t("settings.aboutTipsOn"));
    replayRef.current?.focus();
  };

  // The details card (owner, "more air"): the version, which a person may
  // copy for a bug report, and the update status; the actions under it.
  return (
    <div className="flex flex-col gap-[10px]">
      <DetailsCard id="about" title="ShardBrowser Launcher">
        <DetailRow id="version" label={t("settings.aboutVersionLabel")} copy={info ? { value: info.current } : undefined}>
          {info?.current ?? "…"}
        </DetailRow>
        <DetailRow id="updates" label={t("settings.aboutUpdatesLabel")}>
          <span className={info?.update_available ? "text-warning-text" : undefined}>{status}</span>
        </DetailRow>
        <DetailRow id="website" label={t("settings.aboutWebsiteLabel")}>
          {/* A real href for the link's role; the click opens the system browser. */}
          <a
            href={ABOUT_SITE}
            className="text-primary-text underline underline-offset-2"
            onClick={(e) => { e.preventDefault(); openUrl(ABOUT_SITE).catch(() => {}); }}
          >
            {SITE_HOST}
          </a>
        </DetailRow>
      </DetailsCard>
      <div className="flex flex-wrap items-center gap-2">
        {releaseUrl && (
          <Button variant="primary" mode="filled" size="xsmall" onClick={() => void askLauncherUpdate()}>
            {t("settings.aboutGetUpdate")}
          </Button>
        )}
        <Button variant="neutral" mode="stroke" size="xsmall" onClick={() => openWhatsNew(true)}>
          {t("whatsNew.title")}
        </Button>
        {/* Replay goes to Profiles first, through the unsaved-changes guard (Settings has its own Save bar). */}
        <Button ref={replayRef} variant="neutral" mode="stroke" size="xsmall" onClick={() => void takeTheTour()}>
          {t("settings.aboutReplayTour")}
        </Button>
        {tipsOff && (
          <Button variant="neutral" mode="ghost" size="xsmall" onClick={tipsOn}>
            {t("settings.aboutTipsAgain")}
          </Button>
        )}
        <Button variant="neutral" mode="ghost" size="xsmall" onClick={() => void openUrl(GH_REPO_URL).catch(() => {})}>
          GitHub
        </Button>
      </div>
    </div>
  );
}
