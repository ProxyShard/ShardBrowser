import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { cn, layoutRect, Menu, pointerToLayout } from "@proxyshard/shardx-ui-kit";
import { toast } from "../../shared/model/toast";
import { confirmModal } from "../../shared/model/confirm";
import { isOpenWithoutAutomation } from "../../shared/api/automationRefusals";
import {
  automationCall,
  automationDetach,
  automationLaunch,
  automationPick,
  automationScreencast,
  type Frame,
  type Picked,
} from "../../entities/automation";
import { confirmDirect, lockedRefusal, processKill, profileList, profileRefusal, proxyMissing, startRefusal, useProfile, useProfileUi, type ProfileMeta } from "../../entities/profile";
import { proxyList } from "../../entities/proxy";
import { describeTarget, liveActions, type MenuAction } from "./liveActions";
import { useT } from "../../shared/i18n";

const MAX_W = 1280;
const MAX_H = 800;

type Props = {
  profileId: string | null;
  /** The profile was started here and its frames come in (`useLiveBrowser`). */
  live: boolean;
  /** Adds a step. `target` is null when nothing resolvable was under the click. */
  onAction?: (action: MenuAction, target: Picked | null) => void;
  /** Records an action the operator just performed on the page. */
  onRecorded?: (kind: string, target: Picked | null, extra?: Record<string, unknown>) => void;
  recording: boolean;
  /** The next click picks an element instead of clicking it (a step's Pick). */
  picking?: boolean;
  /** What the picking click was over; null when nothing could be identified. */
  onPicked?: (target: Picked | null) => void;
};

/** Keys the page should get as a named key rather than as text. Everything
 *  else that produces a character is sent as that character, so the profile's
 *  own keyboard layout decides which physical key it came from. */
const NAMED = new Set([
  "Enter", "Tab", "Escape", "Backspace", "Delete", "Home", "End",
  "PageUp", "PageDown", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight",
  "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
]);

/** Starting and stopping the profile the live view mirrors. Held by the
 *  editor, not the view: hiding the browser column must not forget that the
 *  profile runs. */
export function useLiveBrowser(profileId: string | null) {
  const t = useT();
  const [live, setLive] = useState(false);
  const [busy, setBusy] = useState(false);
  // Another profile chosen: it was not started here.
  useEffect(() => { setLive(false); }, [profileId]);

  const start = useCallback(async () => {
    if (!profileId) return;
    // This Start path doesn't go through useProfile.startStop, so it checks on
    // its own that the profile's proxy still exists (IFACE-01, backend B1): a
    // deleted one would start it on this computer's own IP. The profile and
    // proxies are read fresh, and nothing starts when they can't be read.
    let fresh: ProfileMeta[];
    let proxies: { id: string }[];
    try {
      [fresh, proxies] = await Promise.all([profileList(), proxyList()]);
    } catch (e) {
      toast.fail({ title: t("liveView.openFailed"), detail: e });
      return;
    }
    const p = fresh.find((x) => x.id === profileId);
    if (!p) return;
    // A team's: driving it needs Start and Cookies, and no other device holding it.
    // A protected one is never driven (automation_launch refuses it).
    const refused = startRefusal(p) ?? profileRefusal(p, "cookies") ?? lockedRefusal(p);
    if (refused) { toast.fail({ title: t("liveView.openFailed"), detail: refused }); return; }
    if (proxyMissing(p, proxies)) {
      toast.err(t("useProfile.proxyMissingStart", { name: p.name }), {
        action: { label: t("useProfile.assignProxy"), run: () => useProfileUi.getState().openAssign([p.id]) },
      });
      return;
    }
    // Task 11 fix round 2: this Start path skipped O9a's question too — the
    // same shared confirm every other start path uses, not a third copy.
    const choice = await confirmDirect(useProfile.getState, [p]);
    if (choice === "assign") { useProfileUi.getState().openAssign([p.id]); return; }
    if (choice !== "start") return;
    const open = async (restart: boolean) => {
      await automationLaunch(profileId, restart);
      await automationScreencast(profileId, true, MAX_W, MAX_H);
      setLive(true);
    };
    setBusy(true);
    try {
      try { await open(false); }
      catch (e) {
        // Already open without automation: that window can't get it, so it restarts only if asked.
        if (!isOpenWithoutAutomation(e)) throw e;
        setBusy(false);
        const yes = await confirmModal({
          title: t("liveView.withoutAutomationTitle", { name: p.name }),
          message: t("liveView.withoutAutomationMessage"),
          confirmLabel: t("liveView.restart"),
        });
        if (yes !== true) return;
        setBusy(true);
        await open(true);
      }
    } catch (e) { toast.fail({ title: t("liveView.openFailed"), detail: e }); }
    finally { setBusy(false); }
  }, [profileId, t]);

  const stop = useCallback(async () => {
    if (!profileId) return;
    try { await automationScreencast(profileId, false, MAX_W, MAX_H); } catch { /* already gone */ }
    automationDetach(profileId);
    // Actually close the browser — Stop used to only end the screencast and
    // detach CDP, leaving the window open.
    try { await processKill(profileId); } catch { /* already closed */ }
    setLive(false);
  }, [profileId]);

  return { live, busy, start, stop };
}

export type LiveBrowser = ReturnType<typeof useLiveBrowser>;

/** The profile's own window, mirrored frame by frame. Not an embedded browser:
 *  the page runs in the real engine and only its pixels come here. */
export function LiveView({
  profileId,
  live,
  onAction,
  onRecorded,
  recording,
  picking = false,
  onPicked,
}: Props) {
  const t = useT();
  // The data outlives `menuOpen` so the menu keeps its place while it fades out.
  const [menu, setMenu] = useState<{ at: { x: number; y: number }; target: Picked | null } | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const canvas = useRef<HTMLCanvasElement | null>(null);
  const meta = useRef<Frame | null>(null);
  // The frame's own pixel ratio, so the view can size a box to it and grow the
  // browser to fill the column instead of sitting tiny inside letterbox bars.
  const [dim, setDim] = useState({ w: 16, h: 10 });

  useEffect(() => {
    let stop: (() => void) | null = null;
    let alive = true;
    listen<Frame>("automation:frame", (e) => {
      if (!alive || !profileId || e.payload.profile_id !== profileId) return;
      meta.current = e.payload;
      const el = canvas.current;
      if (!el) return;
      const img = new Image();
      img.onload = () => {
        if (el.width !== img.width || el.height !== img.height) {
          el.width = img.width;
          el.height = img.height;
          setDim({ w: img.width, h: img.height });
        }
        el.getContext("2d")?.drawImage(img, 0, 0);
      };
      img.src = `data:image/jpeg;base64,${e.payload.data}`;
    }).then((un) => { stop = un; });
    return () => { alive = false; stop?.(); };
  }, [profileId]);

  // A navigation the operator caused becomes an "Open URL" step. Only while
  // recording, and only the top frame — the browser tells us which.
  useEffect(() => {
    let stop: (() => void) | null = null;
    let alive = true;
    listen<{ profile_id: string; url: string }>("automation:navigated", (e) => {
      if (!alive || !recording || !profileId) return;
      if (e.payload.profile_id !== profileId) return;
      const url = e.payload.url;
      if (!url || url === "about:blank" || url.startsWith("chrome")) return;
      onRecorded?.("goto", null, { url });
    }).then((un) => { stop = un; });
    return () => { alive = false; stop?.(); };
  }, [profileId, recording, onRecorded]);

  // Canvas pixels -> page CSS pixels. The frame is scaled to fit maxWidth, so
  // the ratio is the frame's own width against the page width it covers.
  // Where on the PAGE a click on the canvas landed.
  //
  // The canvas is drawn with object-contain, so the frame sits centred inside
  // the element with bars on two sides — the element's own rectangle is not
  // where the picture is. Measuring against the rectangle put every click a
  // little (and near the edges, a lot) off target, which is why recorded steps
  // came out as coordinates: the element resolver was asked about the wrong
  // point and found nothing.
  const toPage = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const el = canvas.current;
    const m = meta.current;
    if (!el || !m || !el.width || !el.height) return null;
    // Layout px on both sides (Interface size): the pointer by the page zoom,
    // the rect by its own scale.
    const r = layoutRect(el);
    const at = pointerToLayout({ x: e.clientX, y: e.clientY });
    if (!r.width || !r.height) return null;

    const shown = Math.min(r.width / el.width, r.height / el.height);
    const padX = (r.width - el.width * shown) / 2;
    const padY = (r.height - el.height * shown) / 2;

    const imgX = (at.x - r.left - padX) / shown;
    const imgY = (at.y - r.top - padY) / shown;
    // A click on the bars is not a click on the page.
    if (imgX < 0 || imgY < 0 || imgX > el.width || imgY > el.height) return null;

    // The frame is a scaled shot of the visible page, so one factor per axis.
    const kx = m.width > 0 ? m.width / el.width : 1;
    const ky = m.height > 0 ? m.height / el.height : 1;
    return { x: imgX * kx, y: imgY * ky };
  };

  // Left click drives the page through Motion, so what the site sees is a
  // human pointer rather than a synthetic event.
  const onClick = async (e: React.MouseEvent<HTMLCanvasElement>) => {
    if (!profileId || !live) return;
    const p = toPage(e);
    if (!p) return;
    // Picking a step's target: the click names an element and does nothing
    // on the page.
    if (picking) {
      const target = await automationPick(profileId, p.x, p.y).catch((err) => {
        toast.fail({ title: t("liveView.identifyFailed"), detail: err });
        return null;
      });
      onPicked?.(target);
      return;
    }
    // While recording, resolve the element BEFORE clicking: the click may
    // navigate, and then there is nothing left under the point to identify.
    let target: Picked | null = null;
    if (recording) {
      // Not swallowed: a pick that fails is why a step ends up as coordinates,
      // and silently falling back hid that for far too long.
      target = await automationPick(profileId, p.x, p.y).catch((err) => {
        toast.fail({ title: t("liveView.identifyFailed"), detail: err });
        return null;
      });
    }
    try {
      await automationCall(profileId, "Motion.createPointer", { x: p.x, y: p.y });
      await automationCall(profileId, "Motion.glideTo", { x: p.x, y: p.y });
      await automationCall(profileId, "Motion.tap", {});
      if (recording) onRecorded?.("click", target ?? { selector: "", tag: "", label: "", x: p.x, y: p.y, tried: [] });
    } catch (err) { toast.fail({ title: t("liveView.inputFailed"), detail: err }); }
  };

  // The canvas is focusable, so it receives keys while the operator is looking
  // at it. Each key goes out through Motion, which resolves it against the
  // profile's own layout — a ru-RU profile pressing "a" reports the physical
  // key its keyboard would have used.
  const onKeyDown = async (e: React.KeyboardEvent<HTMLCanvasElement>) => {
    if (!profileId || !live) return;
    if (e.key === "Unidentified") return;
    const mods: string[] = [];
    if (e.shiftKey) mods.push("Shift");
    if (e.ctrlKey) mods.push("Control");
    if (e.altKey) mods.push("Alt");
    if (e.metaKey) mods.push("Meta");

    let key: string | null = null;
    if (NAMED.has(e.key)) key = e.key;
    else if (e.key.length === 1) key = e.key;
    if (!key) return;

    // Shift is already carried by the character itself; naming it too would
    // press Shift twice for a capital letter.
    const send = mods.filter((m) => !(m === "Shift" && key!.length === 1));

    e.preventDefault();
    try {
      await automationCall(profileId, "Motion.pressKey", {
        key,
        ...(send.length ? { modifiers: send } : {}),
      });
      if (recording) {
        onRecorded?.("press", null, {
          key,
          ...(send.length ? { modifiers: send.join(", ") } : {}),
        });
      }
    } catch (err) { toast.fail({ title: t("liveView.inputFailed"), detail: err }); }
  };

  // Right-click belongs to the automator only while recording; otherwise it
  // goes to the page, where it opens the site's own menu.
  // Right-click is the automator's, always. Shift is the way through to the
  // page's own menu — the reverse would mean the operator had to switch a mode
  // on just to add a step, which is the thing they do most.
  const onContext = async (e: React.MouseEvent<HTMLCanvasElement>) => {
    e.preventDefault();
    if (!profileId || !live) return;
    const p = toPage(e);
    if (!p) return;

    if (e.shiftKey) {
      try {
        await automationCall(profileId, "Motion.createPointer", { x: p.x, y: p.y });
        await automationCall(profileId, "Motion.glideTo", { x: p.x, y: p.y });
        await automationCall(profileId, "Motion.tap", { button: "right" });
      } catch (err) { toast.fail({ title: t("liveView.inputFailed"), detail: err }); }
      return;
    }

    const at = { x: e.clientX, y: e.clientY };
    setMenu({ at, target: null });
    setMenuOpen(true);
    const target = await automationPick(profileId, p.x, p.y).catch((err) => {
      toast.fail({ title: t("liveView.identifyFailed"), detail: err });
      return null;
    });
    setMenu((m) => (m ? { ...m, target: target ?? { selector: "", tag: "", label: "", x: p.x, y: p.y, tried: [] } } : m));
  };

  // Nothing to show yet: a centered line says what brings the page here.
  const empty = !profileId ? t("liveView.pickProfile") : !live ? t("liveView.startHint") : null;

  return (
    <div className="relative flex h-full min-h-0 items-center justify-center overflow-hidden rounded-control bg-bg-weak-50 ring-1 ring-inset ring-stroke-soft-200">
      {empty && (
        <p className="m-0 max-w-[32ch] px-6 text-center text-balance text-body text-text-soft-400">{empty}</p>
      )}
      {/* A box the size of the frame's aspect ratio, grown to the largest that
          fits — so the browser fills the column, not a tiny letterboxed strip.
          It stays mounted (hidden) so the last frame survives a tab switch. */}
      <div hidden={!!empty} className="max-h-full max-w-full" style={{ width: "100%", aspectRatio: `${dim.w} / ${dim.h}` }}>
        <canvas
          ref={canvas}
          tabIndex={empty ? -1 : 0}
          // Every key goes to the remote browser: the app's own keys stay out.
          data-own-keys
          // No focus ring: it would frame the remote page the whole time
          // the operator types into it.
          className={cn("h-full w-full object-contain focus-visible:outline-hidden", picking && "cursor-crosshair")}
          onClick={onClick}
          onContextMenu={onContext}
          onKeyDown={onKeyDown}
        />
      </div>

      {menu && (
        <Menu
          open={menuOpen}
          onOpenChange={setMenuOpen}
          anchor={menu.at}
          heading={describeTarget(menu.target, t)}
          items={liveActions(t).map((a) => ({ label: a.label, onSelect: () => onAction?.(a, menu.target) }))}
        />
      )}
    </div>
  );
}
