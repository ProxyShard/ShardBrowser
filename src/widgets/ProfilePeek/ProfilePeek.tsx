import { useEffect, useRef } from "react";
import { Button, IconSwap, Tooltip, usePresence, useLatched } from "@proxyshard/shardx-ui-kit";
import { EditIcon, PeekIcon, PlayIcon, SpinnerIcon, StopIcon } from "../../shared/icons";
import { focusProfileRow } from "../../shared/lib/focusRow";
import { useNow } from "../../shared/lib/useNow";
import { LoadError, LoadingRows } from "../../shared/ui/ListStates";
import { StatusPill } from "../../shared/ui/StatusPill";
import { PeekBody, PeekHeader, PeekPanel, PeekSection, PropertySheet } from "../../shared/ui/peek";
import { useLang, useT } from "../../shared/i18n";
import { isProtected, may, profileStatus, proxyMissing, settingsRefusal, startRefusal, useProfile, useProfileUi, useSaving, type ProfileMeta } from "../../entities/profile";
import { usePeekForm } from "./usePeekForm";
import { FingerprintDetails, ProfileDetails } from "./rows/DetailRows";
import { ActivityLine } from "./rows/ActivityLine";
import { capOf, command, labelOf, menuItemsFor } from "../../features/commands";
import { openProfileForm, type ProfileFormTab } from "../../features/profile-form";
import { openHereInstead } from "../../features/profile-sync";

/// The side panel that shows one profile while the list stays in view
/// (CONTEXT: Peek). It only shows: the header's Edit, and each section's,
/// open the profile form, where a change is saved by hand. The shell (frame,
/// header, sections, the way in and out) is every Peek's (F1).
export function ProfilePeek() {
  const t = useT();
  const peekId = useProfileUi((s) => s.peekId);
  const closePeek = useProfileUi((s) => s.closePeek);
  const profile = useProfile((s) => (peekId ? s.profiles.find((p) => p.id === peekId) : undefined));
  const { form, loadError, retry } = usePeekForm(peekId);

  // The profile left the list (moved to the Trash): Peek goes with it.
  const shownId = useRef<string | null>(null);
  useEffect(() => {
    if (profile) shownId.current = profile.id;
    else if (peekId && shownId.current === peekId) closePeek();
  }, [peekId, profile, closePeek]);

  // Kept through the exit: the panel leaves showing what it showed (FEEL-02).
  const open = peekId && profile ? profile : null;
  const presence = usePresence(open?.id ?? null, { maxExitMs: 250 });
  const shown = useLatched(open);
  const shownForm = useLatched(form && open && form.id === open.id ? form : null);

  if (!presence.mounted || !shown) return null;
  const close = () => {
    // From inside Peek (its Close), the keyboard goes back to the row: the
    // panel is inert while it leaves.
    const inside = document.activeElement?.closest("aside[data-peek]") != null;
    closePeek();
    focusProfileRow(shown.id, { ifLost: !inside });
  };
  // A section's "Edit" opens the profile form on its tab; the section's title describes it.
  // Without Edit on a team's profile the form opens to read, and the button says View.
  const canEdit = may(shown, "edit");
  // A team row's settings the server sends only with a right that uses it (cache.rs).
  const hidden = settingsRefusal(shown);
  const edit = (tab: ProfileFormTab) => hidden && tab !== "basics" ? undefined : (titleId: string) => (
    <Button variant="neutral" mode="ghost" size="3xsmall" className="-my-1 -mr-1.5" data-section-edit={tab} aria-describedby={titleId} onClick={() => openProfileForm({ mode: "edit", id: shown.id, tab })}>
      {canEdit ? t("peek.edit") : t("peek.view")}
    </Button>
  );
  // The form of the profile shown, not of the one Peek left a moment ago.
  const fingerprint = shownForm && shownForm.id === shown.id ? shownForm : null;

  return (
    <PeekPanel kind="profile" label={t("peek.label", { name: shown.name })} presence={presence} tour="details-panel">
      <ProfilePeekHeader profile={shown} onClose={close} />
      <PeekBody>
        <PropertySheet>
          <PeekSection id="profile" title={t("peek.profile")} action={edit("basics")}>
            <ProfileDetails profile={shown} />
          </PeekSection>
          <PeekSection id="fingerprint" title={t("peek.fingerprint")} action={edit("fingerprint")}>
            {hidden ? (
              <p data-settings-hidden className="m-0 px-1 text-help text-text-sub-600">{hidden}</p>
            ) : isProtected(shown) ? (
              // Sealed by its password: its editor shows it, once the password is given.
              <p data-fingerprint-locked className="m-0 px-1 text-help text-text-sub-600">{t("peek.fingerprintLocked")}</p>
            ) : fingerprint ? (
              <FingerprintDetails form={fingerprint} />
            ) : loadError ? (
              <LoadError headingLevel={3} what={t("otherStates.profile")} error={loadError} onRetry={retry} />
            ) : (
              <LoadingRows rows={4} rowHeight={32} />
            )}
          </PeekSection>
          <PeekSection id="activity" title={t("peek.activity")}>
            <ActivityLine profile={shown} />
          </PeekSection>
        </PropertySheet>
      </PeekBody>
    </PeekPanel>
  );
}

function ProfilePeekHeader({ profile, onClose }: { profile: ProfileMeta; onClose: () => void }) {
  const t = useT();
  const lang = useLang((s) => s.lang);
  const now = useNow();
  const runningSince = useProfile((s) => s.running[profile.id]);
  const isRunning = runningSince !== undefined;
  const isStarting = useProfile((s) => s.startBusy.has(profile.id));
  const isCancelling = useProfile((s) => s.cancelling.has(profile.id));
  const cancelled = useProfile((s) => s.cancelled.has(profile.id));
  const stopping = useProfile((s) => s.pending[profile.id]?.kind === "stop");
  const failedAt = useProfile((s) => s.startFailed[profile.id]);
  const proxies = useProfile((s) => s.proxies);
  const startStop = useProfile((s) => s.startStop);
  const cancelStart = useProfile((s) => s.cancelStart);
  // Task 5d's one function, reused here (Task 5f): the same word, dot and
  // pill the Profiles list's Status column shows (Live layer, spec 4.2).
  // Its session still being sent: Saving…, and Start says why it waits (startRefusal reads it).
  const saving = useSaving((s) => s.ids.has(profile.id));
  const runStatus = profileStatus(profile, { starting: isStarting, cancelled, stopping, runningSince, failedAt, saving, now, lang, proxyMissing: proxyMissing(profile, proxies) });
  // A running profile's note is its status's tooltip, not a line (CRAFT-01);
  // any other state's tooltip is its exact detail, as in the list. A screen
  // reader reads it with the word, as the list's Status cell does.
  const note = isRunning ? t("peek.runningNote") : runStatus.detail;
  // The registry's Peek menu (E3, R9): the row menu's organize and copy
  // items for this profile, with their icons and keys. Start and Stop are the
  // header's own button. Built each render, so Import cookies… reads Running.
  const menuItems = menuItemsFor("peekMenu", "profiles", { ids: [profile.id] });
  // Open on another computer (personal sync): Start would be refused, so the button takes it here instead.
  const held = !isRunning && !isStarting && runStatus.tone === "elsewhere" && !!profile.sync?.open_on;
  // A team's: no Start, or another device has it open. The button says why and does nothing.
  const blocked = !isRunning && !isStarting ? startRefusal(profile) : null;
  const canEdit = may(profile, "edit");
  const pill = (
    // Open elsewhere: the device name is cut before the profile's; the Sync row has it whole.
    <span data-peek-run-status className={runStatus.tone === "elsewhere" ? "flex min-w-[4.5rem] shrink-[100]" : "flex min-w-0 shrink-0"}>
      <StatusPill status={runStatus} />
      {note && <span className="sr-only">{note}</span>}
    </span>
  );
  return (
    <PeekHeader
      subject={profile.id}
      title={<h2 className="m-0 min-w-0 truncate text-panel-title text-text-strong-950">{profile.name}</h2>}
      status={note ? <Tooltip content={note}>{pill}</Tooltip> : pill}
      extra={
        // Its word goes first when the header is tight (PeekHeader): the tooltip says it then.
        <Tooltip content={canEdit ? t("peek.edit") : t("peek.view")} whenTruncated>
          <Button
            variant="neutral" mode="stroke" size="3xsmall" data-peek-edit
            leftIcon={canEdit ? <EditIcon className="size-3.5" /> : <PeekIcon className="size-3.5" />}
            onClick={() => openProfileForm({ mode: "edit", id: profile.id })}
          >
            {canEdit ? t("peek.edit") : t("peek.view")}
          </Button>
        </Tooltip>
      }
      primary={
        // Starting says Cancel and spins in place, keeping focus; pressed, it
        // says Cancelling and waits (busy). Stop comes once the launch returns.
        <Tooltip content={blocked ?? undefined}>
        <Button
          variant={isStarting ? "neutral" : isRunning ? "error" : "primary"}
          mode="lighter"
          size="3xsmall"
          isLoading={isCancelling}
          leftIcon={isStarting
            ? <SpinnerIcon className="size-3.5 animate-spin" />
            : <IconSwap on={isRunning} onIcon={<StopIcon className="size-3.5" />} offIcon={<PlayIcon className="size-3.5" />} />}
          data-blocked={blocked ? "" : undefined}
          disabled={!!blocked}
          aria-description={blocked ?? undefined}
          onClick={() => { if (!blocked) void (isStarting ? cancelStart(profile.id) : held ? openHereInstead(profile) : startStop(profile)); }}
        >
          {isCancelling ? t("profileRowActions.cancelling") : isStarting ? labelOf("profile.cancelStart") : held ? t("profileSync.takeConfirm") : labelOf(isRunning ? "profile.stop" : "profile.start")}
        </Button>
        </Tooltip>
      }
      menuItems={menuItems}
      menuLabel={t("pageHeader.more")}
      closeLabel={t("peek.close")}
      closeShortcut={capOf(command("profile.closePeek"))}
      onClose={onClose}
    />
  );
}
