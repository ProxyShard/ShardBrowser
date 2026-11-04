import { t } from "../../../shared/i18n";
import { toast } from "../../../shared/model/toast";
import { askPassword } from "../../../shared/model/passwordAsk";
import {
  forgetForm, profileChangePassword, profileProtect, profileUnprotect, resolveId, useProfile, type ProfileMeta,
} from "../../../entities/profile";

/// Set, change and remove a profile's own password (#127). Each runs from the
/// password dialog: a wrong or short password, or a refusal, is said there.

/** The profile's backend id; a row whose create failed is gone. */
async function realId(p: ProfileMeta): Promise<string> {
  const id = await resolveId(p.id);
  if (!id) throw t("profileForm.gone");
  return id;
}

/** Seals a profile with a new password. True when it was set. */
export async function setPassword(p: ProfileMeta): Promise<boolean> {
  const done = await askPassword({
    kind: "set",
    title: t("profileLock.setTitle", { name: p.name }),
    message: t("profileLock.setMessage"),
    confirmLabel: t("profileLock.setConfirm"),
    attempt: async ({ newPassword }) => {
      const id = await realId(p);
      useProfile.getState().applyRows([await profileProtect(id, newPassword)]);
      // Peek's copy of the fingerprint goes: a sealed profile shows none.
      forgetForm(id);
    },
  });
  if (done) toast.ok(t("profileLock.setDone", { name: p.name }));
  return !!done;
}

/** Replaces a profile's password. True when it changed. */
export async function changePassword(p: ProfileMeta): Promise<boolean> {
  const done = await askPassword({
    kind: "change",
    title: t("profileLock.changeTitle", { name: p.name }),
    message: t("profileLock.changeMessage"),
    confirmLabel: t("profileLock.changeConfirm"),
    attempt: async ({ password, newPassword }) => { await profileChangePassword(await realId(p), password, newPassword); },
  });
  if (done) toast.ok(t("profileLock.changeDone", { name: p.name }));
  return !!done;
}

/** Takes the password off: the profile is stored open again. True when it went. */
export async function removePassword(p: ProfileMeta): Promise<boolean> {
  const done = await askPassword({
    kind: "remove",
    title: t("profileLock.removeTitle", { name: p.name }),
    message: t("profileLock.removeMessage"),
    confirmLabel: t("profileLock.removeConfirm"),
    attempt: async ({ password }) => { useProfile.getState().applyRows([await profileUnprotect(await realId(p), password)]); },
  });
  if (done) toast.ok(t("profileLock.removeDone", { name: p.name }));
  return !!done;
}
