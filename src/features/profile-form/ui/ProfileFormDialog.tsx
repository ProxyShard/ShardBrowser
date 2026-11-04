import { useEffect, useRef } from "react";
import { Button, DialogModal, TabPanel, Tabs } from "@proxyshard/shardx-ui-kit";
import { InfoIcon } from "../../../shared/icons";
import { isMod } from "../../../shared/lib/keyboard";
import { commitFocusedField } from "../../../shared/lib/commitField";
import { useUnsaved } from "../../../shared/model/unsaved";
import { LoadError, LoadingRows } from "../../../shared/ui/ListStates";
import { useT } from "../../../shared/i18n";
import { settingsRefusal, useProfile } from "../../../entities/profile";
import { useRightsChange } from "../../../entities/space";
import { walkTheForm } from "../../tour/lib/profileTour";
import {
  PROFILE_FORM_TABS, canSave, changedTabs, closeProfileForm, isDirty, openProfileForm, passwordProblemOf, problemsOf, readOnlyWhy, reloadProfileForm,
  rightsProblem, saveProfileForm, setProfileFormTab, useProfileForm, type ProfileFormTab,
} from "../model/useProfileForm";
import { BasicsTab } from "./BasicsTab";
import { ProxyTab } from "./ProxyTab";
import { FingerprintTab } from "./FingerprintTab";
import { ExtensionsTab } from "./ExtensionsTab";

/** A tab's state in words, after its label (owner rule: never a dot alone). */
function TabState({ text, error }: { text: string; error?: boolean }) {
  return (
    <span data-tab-state={error ? "error" : "changed"} className={error ? "inline-flex items-center gap-1 text-help text-error-text" : "inline-flex items-center gap-1 text-help text-text-soft-400"}>
      <span aria-hidden="true" className={error ? "size-[5px] rounded-full bg-error-base" : "size-[5px] rounded-full bg-primary-base"} />
      {text}
    </span>
  );
}

/// The profile form, mounted once (App): "New profile" and "Edit Profile 7"
/// in one large dialog, four tabs, Cancel and Save (or Create profile). A new
/// profile's form has a quiet "Show me around" beside the tabs: the walk
/// through the form (the tour), on the form as it is.
/// Save is pressable only when something changed and every field is valid,
/// and the reason it is not shows as words beside it. While it saves the
/// fields are read-only. Cmd/Ctrl+S saves: an app command cannot run while a
/// dialog has the keyboard, so the form listens for it itself.
export function ProfileFormDialog() {
  const t = useT();
  const s = useProfileForm();
  const { open, mode, tab, draft, status, error, session } = s;
  const nameRef = useRef<HTMLInputElement>(null);
  const dirty = isDirty(s);
  const changed = changedTabs(s);
  const problems = problemsOf(draft);
  // A new profile's password, while one is typed: too short, or the repeat differs.
  const passwordProblem = passwordProblemOf(s);
  const saving = status === "saving";
  // The list's name until the form has loaded, so the title never reads "Edit ".
  const listed = useProfile((st) => st.profiles.find((p) => p.id === s.id));
  const name = s.saved?.form.name ?? listed?.name ?? "";
  // A team's profile without Edit opens to read: every field shows, nothing changes.
  useRightsChange();
  const readOnly = listed ? readOnlyWhy(s) : null;
  const refused = readOnly ? null : rightsProblem(s);
  const hidden = mode === "edit" && listed ? settingsRefusal(listed) : null;

  // Every exit the dialog cannot see (a page change, quitting) asks first.
  useUnsaved(open && dirty ? {
    id: "profile-form",
    label: mode === "create" ? t("profileForm.newTitle") : name,
    save: saveProfileForm,
    discard: closeProfileForm,
  } : null);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (!isMod(e) || e.altKey || e.shiftKey || e.key.toLowerCase() !== "s") return;
      // Never the browser's own Save page; with nothing to save, nothing happens.
      e.preventDefault();
      // The field that has the keyboard commits first: the save reads what the screen shows.
      commitFocusedField();
      if (!e.repeat && canSave()) void saveProfileForm();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open]);

  // A new profile's name is selected as it shows, so typing replaces it.
  const ready = !!draft;
  useEffect(() => {
    if (!open || !ready || mode !== "create") return;
    const id = requestAnimationFrame(() => { nameRef.current?.focus(); nameRef.current?.select(); });
    return () => cancelAnimationFrame(id);
  }, [open, ready, mode, session]);

  const labels: Record<ProfileFormTab, string> = {
    basics: t("profileForm.tabBasics"),
    proxy: t("profileForm.tabProxy"),
    fingerprint: t("profileForm.tabFingerprint"),
    extensions: t("profileForm.tabExtensions"),
  };
  const items = PROFILE_FORM_TABS.map((value) => ({
    value,
    label: labels[value],
    badge: value === "basics" && (problems.name || (passwordProblem && s.tried)) ? <TabState error text={t("profileForm.tabError")} />
      : changed.has(value) ? <TabState text={t("profileForm.tabChanged")} />
      : undefined,
  }));

  const note = status === "conflict" ? (
    <span role="alert">
      {t("profileForm.conflict")}{" "}
      {s.theirs && <Button mode="link" size="3xsmall" onClick={reloadProfileForm}>{t("profileForm.reload")}</Button>}
    </span>
  ) : status === "error" && draft ? (
    <span role="alert" className="flex flex-col">
      <span className="text-error-text">{error}</span>
      {s.errorDetail && <span data-error-detail>{s.errorDetail}</span>}
    </span>
  ) : readOnly && draft ? <span data-read-only>{t("profileForm.readOnly", { why: readOnly })}</span>
    : saving || !draft ? null
    : problems.name ? t("profileForm.nameRequired")
    : passwordProblem ? <span data-password-problem>{passwordProblem}</span>
    : refused ? <span data-rights-problem>{refused}</span>
    : s.badFields.length > 0 ? t("profileForm.numberInvalid")
    : mode === "edit" && !dirty ? t("profileForm.noChanges")
    : null;

  return (
    <DialogModal
      open={open}
      onOpenChange={(o) => { if (!o) closeProfileForm(); }}
      title={mode === "create" ? t("profileForm.newTitle") : readOnly ? t("profileForm.viewTitle", { name }) : t("profileForm.editTitle", { name })}
      size="lg"
      // At most 85% of the window; the body scrolls, the footer stays.
      className="max-h-[calc(var(--viewport-h)*0.85)]!"
      dirty={dirty}
      initialFocusRef={nameRef}
      {...(readOnly ? { cancelLabel: t("profileForm.close") } : {
        onConfirm: () => { commitFocusedField(); void saveProfileForm(); },
        confirmLabel: saving ? t("profileForm.saving") : mode === "create" ? t("profileForm.create") : t("profileForm.save"),
      })}
      isLoading={saving}
      isDisabled={!canSave(s)}
      footerNote={note}
    >
      {draft ? (
        <div data-profile-form className="flex flex-col">
          {/* The tabs stay in view while a long tab scrolls under them. */}
          <div className="sticky top-0 z-[1] flex items-center gap-3 bg-bg-raised">
            <div className="min-w-0 flex-1">
              <Tabs
                aria-label={t("profileForm.tabsLabel")}
                idBase="profile-form"
                variant="line"
                items={items}
                value={tab}
                onChange={(v) => setProfileFormTab(v as ProfileFormTab)}
              />
            </div>
            {mode === "create" && (
              // The walk is about making a profile, so only its form has it. A
              // quiet link, as the (i) on a page: the form's own buttons stay Cancel and Create.
              <Button
                variant="neutral"
                mode="link"
                size="3xsmall"
                className="shrink-0"
                disabled={saving}
                leftIcon={<InfoIcon className="size-3.5" />}
                onClick={walkTheForm}
              >
                {t("profileForm.showMe")}
              </Button>
            )}
          </div>
          <fieldset disabled={saving || !!readOnly} data-read-only={readOnly ? "" : undefined} className="m-0 min-w-0 border-0 p-0">
            <TabPanel tabsId="profile-form" value={tab} className="py-4">
              {/* A team row's settings the server did not send: only its row's own fields are known. */}
              {tab !== "basics" && hidden ? <p data-settings-hidden className="m-0 text-secondary text-text-sub-600">{hidden}</p>
                : tab === "basics" ? <BasicsTab draft={draft} nameRef={nameRef} />
                : tab === "proxy" ? <ProxyTab draft={draft} />
                : tab === "fingerprint" ? <FingerprintTab key={session} draft={draft} />
                : <ExtensionsTab draft={draft} />}
            </TabPanel>
          </fieldset>
        </div>
      ) : status === "error" ? (
        <LoadError
          headingLevel={3}
          className="py-4"
          what={t("otherStates.profile")}
          error={error}
          onRetry={() => openProfileForm(s.mode === "edit" && s.id ? { mode: "edit", id: s.id, tab } : { mode: "create", tab })}
        />
      ) : (
        <div className="py-4"><LoadingRows rows={5} rowHeight={44} /></div>
      )}
    </DialogModal>
  );
}
