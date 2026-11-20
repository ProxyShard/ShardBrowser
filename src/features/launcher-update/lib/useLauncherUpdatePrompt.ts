import { useEffect } from "react";
import { useTour } from "../../tour";
import { useWhatsNew } from "../../../shared/model/whatsNew";
import { fetchLauncherOffer, offerLauncherUpdate } from "../model/launcherUpdate";

/** The first check, a little after the start: the page and its rows come first. */
const FIRST_CHECK_MS = 8_000;
/** Then again while the launcher stays open, as a release may come out meanwhile. */
const RECHECK_MS = 6 * 60 * 60 * 1000;
/** How often a found update looks for a free moment to ask. */
const WAIT_MS = 3_000;

/** Nothing else is asking: no dialog (What's new, the welcome, a question) and no tour. */
function free(): boolean {
  const whatsNew = useWhatsNew.getState();
  if (!whatsNew.decided || whatsNew.open) return false;
  if (useTour.getState().run) return false;
  return !document.querySelector('[role="dialog"], [role="alertdialog"]');
}

/** Mount once, in the shell: asks "Update?" when a newer launcher is out. */
export function useLauncherUpdatePrompt() {
  useEffect(() => {
    let gone = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const later = (ms: number, run: () => void) => { timer = setTimeout(run, ms); };
    const check = async () => {
      const offer = await fetchLauncherOffer();
      if (gone) return;
      if (!offer) { later(RECHECK_MS, () => void check()); return; }
      const ask = () => {
        if (gone) return;
        if (!free()) { later(WAIT_MS, ask); return; }
        void offerLauncherUpdate(offer).finally(() => { if (!gone) later(RECHECK_MS, () => void check()); });
      };
      ask();
    };
    later(FIRST_CHECK_MS, () => void check());
    return () => { gone = true; clearTimeout(timer); };
  }, []);
}
