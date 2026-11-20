import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useProfile } from "../../../entities/profile";
import { t } from "../../../shared/i18n";
import { commitFocusedField } from "../../../shared/lib/commitField";
import { fmtBytesProgress } from "../../../shared/lib/format";
import { GH_REPO_URL } from "../../../shared/lib/utils";
import { confirmModal } from "../../../shared/model/confirm";
import { toast } from "../../../shared/model/toast";
import { confirmLeave } from "../../../shared/model/unsaved";
import { confirmOpenSessions } from "../../team-sessions";

/** What `launcher_update_offer` answers (launcher_update.rs): a newer version,
 *  and whether this copy installs it itself (the portable .exe does not). */
export type LauncherOffer = {
  current: string;
  version: string;
  installable: boolean;
  release_url: string | null;
};

/** The version "Not now" was pressed for: not asked again until the next start. */
let notNow: string | null = null;
/** A question or an update under way: a second one waits its turn. */
let busy = false;

export async function fetchLauncherOffer(): Promise<LauncherOffer | null> {
  try {
    return await invoke<LauncherOffer | null>("launcher_update_offer");
  } catch {
    return null;
  }
}

/** Asks "Update?" for `offer`, then updates on Update. `asked`: opened by the
 *  person (Settings, What's new), so an earlier Not now does not hold it back. */
export async function offerLauncherUpdate(offer: LauncherOffer, asked = false): Promise<void> {
  if (busy || (!asked && notNow === offer.version)) return;
  busy = true;
  try {
    const s = useProfile.getState();
    const running = s.profiles.filter((p) => s.running[p.id]);
    const message = !offer.installable
      ? t("launcherUpdate.helpDownload")
      : running.length > 0 ? t("launcherUpdate.helpRunning") : t("launcherUpdate.help");
    const yes = await confirmModal({
      title: t("launcherUpdate.title", { v: offer.version }),
      message,
      items: offer.installable ? running.map((p) => ({ label: p.name })) : undefined,
      buttons: [
        { label: t("launcherUpdate.notNow"), value: false },
        { label: offer.installable ? t("launcherUpdate.update") : t("launcherUpdate.download"), value: true, primary: true },
      ],
    });
    if (!yes) {
      notNow = offer.version;
      return;
    }
    if (!offer.installable) {
      await openUrl(offer.release_url ?? `${GH_REPO_URL}/releases/latest`).catch(() => {});
      return;
    }
    await install(offer.version);
  } finally {
    busy = false;
  }
}

/** Downloads, asks about unsaved changes and open team profiles as Quit does,
 *  stops the running profiles, then installs; the launcher restarts on the
 *  new version (on Windows the installer restarts it). */
async function install(v: string): Promise<void> {
  const p = toast.progress({ title: t("launcherUpdate.downloading", { v }), total: 100 });
  const unlisten = await listen<{ received: number; total: number | null }>("launcher-update://progress", (e) => {
    const { received, total } = e.payload;
    if (total) p.update(Math.min(99, Math.floor((received * 100) / total)), t("launcherUpdate.downloadingSize", { v, progress: fmtBytesProgress(received, total) }));
  }).catch(() => () => {});
  try {
    await invoke("launcher_update_download");
  } catch (e) {
    p.done({ title: t("launcherUpdate.failed"), detail: String(e), failed: 1 });
    return;
  } finally {
    unlisten();
  }
  p.done({ title: t("launcherUpdate.downloaded", { v }) });

  commitFocusedField();
  if (!(await confirmLeave()) || !(await confirmOpenSessions())) {
    toast.info(t("launcherUpdate.later"));
    return;
  }
  const s = useProfile.getState();
  const ids = Object.keys(s.running).filter((id) => s.running[id]);
  if (ids.length > 0 && (await s.stopMany(ids)).length > 0) {
    toast.fail({ title: t("launcherUpdate.failed"), detail: t("launcherUpdate.stillRunning") });
    return;
  }
  toast.info(t("launcherUpdate.installing", { v }));
  try {
    await invoke("launcher_update_apply");
  } catch (e) {
    toast.fail({ title: t("launcherUpdate.failed"), detail: e });
  }
}

/** Settings and What's new: checks again and asks at once. */
export async function askLauncherUpdate(): Promise<void> {
  const offer = await fetchLauncherOffer();
  if (offer) await offerLauncherUpdate(offer, true);
  else toast.ok(t("settings.aboutUpToDate"));
}
