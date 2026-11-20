import { Fragment, useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { invoke } from "@tauri-apps/api/core";
import { Badge, Button, Card, Modal, cn, prefersReducedMotion, springTiming, trackMotion } from "@proxyshard/shardx-ui-kit";
import { ChevronRightIcon, LockedIcon, StarOutlineIcon } from "../../shared/icons";
import { SelectField } from "../../shared/ui/SelectField";
import { useWhatsNew } from "../../shared/model/whatsNew";
import { takeTheTour } from "../../features/tour";
import { askLauncherUpdate } from "../../features/launcher-update";
import { useOnboarding } from "../../features/onboarding";
import type { RtUpdate } from "../../shared/types";
import { withUtm } from "../../shared/lib/utils";
import { fmtCount } from "../../shared/lib/format";
import { useLang, useT } from "../../shared/i18n";
import data from "./patchlog.json";

/** The log is data, in patchlog.json; this module only draws it. */

type Block =
  | { type: "p" | "note"; text: string }
  | { type: "code"; text: string }
  | { type: "list"; items: string[] };

type Entry = {
  id: string;
  title: string;
  /** One line under the title: what it is, before any detail. */
  lead: string;
  tag?: string;
  /** Which half of the product changed: the engine, or the launcher. */
  scope?: "browser" | "launcher";
  locked?: boolean;
  blocks: Block[];
};

type Release = { version: string; date: string; entries: Entry[] };

const RELEASES = data.releases as Release[];
/** Newest release, and the one the picker starts on. */
const LATEST = RELEASES[0];

/** Inline markup, deliberately tiny: `code`, **strong**, *emphasis*. */
function Rich({ text }: { text: string }) {
  const parts = text.split(/(`[^`]+`|\*\*[^*]+\*\*|\*[^*]+\*)/g);
  return (
    <>
      {parts.map((p, i) => {
        if (p.startsWith("`") && p.endsWith("`") && p.length > 2) {
          return (
            <code
              key={i}
              className="rounded-chip-sm bg-bg-weak-50 px-1 py-0.5 text-mono text-text-strong-950"
            >
              {p.slice(1, -1)}
            </code>
          );
        }
        if (p.startsWith("**") && p.endsWith("**") && p.length > 4) {
          return (
            <strong key={i} className="text-text-strong-950">
              {p.slice(2, -2)}
            </strong>
          );
        }
        // The names of UI elements, in the element's own words: a step of
        // weight, never a synthesized oblique (TYPE-11).
        if (p.startsWith("*") && p.endsWith("*") && p.length > 2) {
          return (
            <span key={i} className="font-medium text-text-strong-950">
              {p.slice(1, -1)}
            </span>
          );
        }
        return <Fragment key={i}>{p}</Fragment>;
      })}
    </>
  );
}

function Blocks({ blocks }: { blocks: Block[] }) {
  return (
    <>
      {blocks.map((b, i) => {
        const spaced = i === 0 ? "" : "mt-2";
        if (b.type === "code") {
          return (
            <pre
              key={i}
              className={cn(
                "m-0 overflow-x-auto rounded-control bg-bg-weak-50 p-3 text-mono leading-[18px] text-text-sub-600 ring-1 ring-inset ring-stroke-soft-200",
                i === 0 ? "" : "mt-2.5",
              )}
            >
              {b.text}
            </pre>
          );
        }
        if (b.type === "list") {
          return (
            <ul key={i} className={cn("m-0 flex list-none flex-col gap-1.5 p-0", spaced)}>
              {b.items.map((it, j) => (
                <li key={j} className="flex gap-2 text-body text-text-sub-600">
                  <span className="mt-[8px] size-[3px] shrink-0 rounded-full bg-text-soft-400" />
                  <span>
                    <Rich text={it} />
                  </span>
                </li>
              ))}
            </ul>
          );
        }
        return (
          <p
            key={i}
            className={cn(
              "m-0 text-body",
              b.type === "note" ? "text-text-soft-400" : "text-text-sub-600",
              spaced,
            )}
          >
            <Rich text={b.text} />
          </p>
        );
      })}
    </>
  );
}

function LockNotice() {
  const t = useT();
  return (
    <div className="mt-2.5 flex flex-col gap-2 rounded-control bg-warning-tint p-3 ring-1 ring-inset ring-warning-base/30">
      <div className="flex items-center gap-2 text-secondary font-medium text-text-strong-950">
        <span className="text-warning-text">
          <LockedIcon className="size-3.5" />
        </span>
        <span>
          {t("patchlog.shipsAtStars", {
            n: fmtCount(data.starsRequired),
          })}
        </span>
      </div>
      <p className="m-0 text-secondary text-text-sub-600">
        {t("patchlog.lockExplainer")}
      </p>
      <div>
        <Button
          size="2xsmall"
          variant="neutral"
          mode="stroke"
          onClick={() => openUrl(withUtm(data.repoUrl)).catch(() => {})}
        >
          <span className="mr-1.5 inline-grid place-items-center align-middle">
            <StarOutlineIcon className="size-3.5" />
          </span>
          {t("patchlog.starRepo")}
        </Button>
      </div>
    </div>
  );
}

/** A standalone label starts with a capital (owner): the log's tags are "fix", "new". */
const capital = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

/// Mounts a body while it is open or closing, and moves its height with the
/// spring from what shows now (Live layer 6.3): a second press mid-way turns
/// it around without a jump. Under Reduce motion it opens and closes at once.
function useReveal(open: boolean) {
  const ref = useRef<HTMLDivElement>(null);
  const [shown, setShown] = useState(open);
  if (open && !shown) setShown(true);
  const moved = useRef(false);
  useLayoutEffect(() => {
    if (!moved.current) { moved.current = true; return; }
    const el = ref.current;
    if (!el) return;
    const running = el.getAnimations().length > 0;
    // Mid-way it is where the last move left it; a fresh open starts at 0.
    const from = running || !open ? el.offsetHeight : 0;
    for (const a of el.getAnimations()) a.cancel();
    const to = open ? el.offsetHeight : 0;
    if (prefersReducedMotion() || from === to) {
      if (!open) setShown(false);
      return;
    }
    const { easing, duration } = springTiming(getComputedStyle(el));
    const move = trackMotion(el.animate([{ height: `${from}px` }, { height: `${to}px` }], { duration, easing, fill: "forwards" }));
    // Open, it lets go of the height, so it follows its text; closed, it leaves.
    move.onfinish = () => (open ? move.cancel() : setShown(false));
  }, [open]);
  return { ref, shown };
}

/// One entry as a row (CRAFT-28): its title, badges and one-line lead; the
/// details open on Enter, Space or a click (the APG disclosure: a heading
/// that holds the button, the body a region named by it).
function EntryRow({ entry }: { entry: Entry }): ReactNode {
  const t = useT();
  const [open, setOpen] = useState(false);
  const reveal = useReveal(open);
  const titleId = useId();
  const bodyId = useId();
  return (
    <li data-entry={entry.id} className="flex flex-col">
      <h4 className="m-0">
        {/* eslint-disable-next-line no-restricted-syntax -- a disclosure row (title, badges and lead on two lines); the kit Button is one line at a fixed height, as the Sidebar's group toggle */}
        <button
          type="button"
          aria-expanded={open}
          aria-controls={bodyId}
          aria-labelledby={titleId}
          onClick={() => setOpen((v) => !v)}
          className="focus-ring-inset flex w-full items-start gap-2 px-4 py-3 text-start outline-hidden transition-hover hover:bg-bg-hover"
        >
          <ChevronRightIcon
            aria-hidden
            className={cn(
              "mt-1 size-3.5 shrink-0 text-icon-soft-400 transition-transform duration-(--motion-base) ease-out reduced:transition-none",
              open && "rotate-90",
            )}
          />
          <span className="flex min-w-0 flex-1 flex-col gap-0.5">
            <span className="flex flex-wrap items-center gap-2">
              <span id={titleId} className="text-ui text-text-strong-950">{entry.title}</span>
              {/* A changelog category is not a state: gray, like "Latest". */}
              {entry.tag && (
                <span data-badge data-color="gray" className="inline-flex">
                  <Badge color="gray" size="small">{capital(entry.tag)}</Badge>
                </span>
              )}
              {entry.locked && (
                <span data-badge data-color="warning" className="inline-flex">
                  <Badge color="warning" size="small">{t("patchlog.lockedBadge")}</Badge>
                </span>
              )}
            </span>
            <span className="truncate text-secondary text-text-sub-600">{entry.lead}</span>
          </span>
        </button>
      </h4>
      {reveal.shown && (
        <div ref={reveal.ref} id={bodyId} role="region" aria-labelledby={titleId} data-entry-body className="overflow-hidden">
          <div className="measure flex flex-col pb-4 pe-4 ps-10 text-body text-text-sub-600">
            <Blocks blocks={entry.blocks} />
            {entry.locked && <LockNotice />}
          </div>
        </div>
      )}
    </li>
  );
}

/// The release notes (patchlog.json) in a dialog. Opens by itself after an
/// update (WhatsNewGate), or from the sidebar's version item.
export function WhatsNewDialog() {
  const t = useT();
  const lang = useLang((s) => s.lang);
  const panelRef = useRef<HTMLDivElement>(null);
  const open = useWhatsNew((s) => s.open);
  const setOpen = useWhatsNew((s) => s.setOpen);
  const installKind = useWhatsNew((s) => s.installKind);
  const tourNever = useOnboarding((s) => s.tour === "never");
  const [release, setRelease] = useState<Release>(LATEST);
  const [update, setUpdate] = useState<RtUpdate | null>(null);

  useEffect(() => {
    if (!open) return;
    invoke<RtUpdate>("launcher_update_check").then(setUpdate).catch(() => {});
  }, [open]);

  return (
    <Modal
      open={open}
      onClose={() => setOpen(false)}
      title={t("whatsNew.title")}
      closeLabel={t("common.close")}
      size="lg"
      // Plan ruling 23: first focus is the notes, which then scroll with the
      // arrow keys, not the release picker with its ring.
      initialFocusRef={panelRef}
      // After an update only (the UI changed a lot): the full tour, with no welcome screens,
      // and only until the tour was taken or put away (a replay resets it to never).
      // Opened from the sidebar or Settings with no update, it has no footer.
      footer={installKind === "update" && tourNever ? (
        <Button variant="primary" size="xsmall" onClick={() => { setOpen(false); void takeTheTour(); }}>
          {t("commands.takeTour")}
        </Button>
      ) : undefined}
    >
      <div
        ref={panelRef}
        tabIndex={-1}
        role="region"
        data-whats-new-panel
        aria-label={t("whatsNew.title")}
        // Room on every side, given back by the margins, so no ring inside
        // (the release picker's) is cut; the panel itself draws none: it is
        // reached only by the open, never by Tab (MP4 review, I1).
        className="-mx-2 -my-1.5 flex max-h-[calc(var(--viewport-h)*0.7)] flex-col gap-3 overflow-y-auto px-2 py-1.5 outline-hidden"
      >
        {update?.update_available && update.release_url && (
          <Button
            mode="link"
            size="2xsmall"
            className="self-start"
            onClick={() => { setOpen(false); void askLauncherUpdate(); }}
          >
            {t("sidebar.updateStatus", { v: update.latest ?? "" })}
          </Button>
        )}

        <div className="flex items-center gap-2.5">
          <div className="w-[196px]">
            <SelectField
              aria-label={t("whatsNew.release")}
              value={release.version}
              onChange={(v) => setRelease(RELEASES.find((r) => r.version === v) ?? LATEST)}
              options={RELEASES.map((r) => ({ value: r.version, label: `v${r.version} (${r.date})` }))}
            />
          </div>
          {release === LATEST && (
            <Badge color="gray" size="small">
              {t("patchlog.latestBadge")}
            </Badge>
          )}
          <span className="ml-auto text-secondary text-text-soft-400">
            {release.entries.length === 1
              ? t("patchlog.changeCountOne", { n: release.entries.length })
              : t("patchlog.changeCountMany", { n: release.entries.length })}
          </span>
        </div>

        {lang !== "en" && <p className="m-0 text-help text-text-sub-600">{t("whatsNew.englishOnly")}</p>}

        {/* Grouped by what changed; each entry a row whose details open on demand. */}
        {(["launcher", "browser"] as const).map((scope) => {
          const list = release.entries.filter((e) => (e.scope ?? "browser") === scope);
          if (list.length === 0) return null;
          return (
            <section key={scope} data-scope-group={scope} className="flex flex-col gap-2">
              <h3 className="m-0 flex items-baseline gap-2 text-section-title text-text-strong-950">
                {scope === "launcher" ? t("patchlog.scopeLauncher") : t("patchlog.scopeBrowser")}
                <span className="text-secondary tabular-nums text-text-soft-400">{list.length}</span>
              </h3>
              <Card padding="none" className="overflow-hidden">
                <ul className="m-0 flex list-none flex-col divide-y divide-stroke-soft-200 p-0">
                  {list.map((e) => (
                    <EntryRow key={e.id} entry={e} />
                  ))}
                </ul>
              </Card>
            </section>
          );
        })}
      </div>
    </Modal>
  );
}
