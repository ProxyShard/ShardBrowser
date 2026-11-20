import { useId, type ReactNode } from "react";
import { useT } from "../../../shared/i18n";
import { useProfile, type ProfileForm } from "../../../entities/profile";
import {
  useFingerprintPick, OsField, TemplateField,
  HardwareFields, ScreenField, WindowFields, LocaleFields, NoiseFields, PrivacyFields, MediaFields,
} from "../../manage-profiles/ui/fields";
import { patchForm, setFieldInvalid, setSourceKind, useProfileForm, type ProfileFormDraft } from "../model/useProfileForm";
import { SourceField } from "./SourceField";

/** A titled group of fields: a heading over them, the fields in one column. */
function Group({ title, children }: { title: string; children: ReactNode }) {
  const id = useId();
  return (
    <section aria-labelledby={id} className="flex flex-col gap-3">
      <h3 id={id} className="m-0 text-section-label text-text-soft-400">{title}</h3>
      <div className="flex flex-col gap-4">{children}</div>
    </section>
  );
}

/// Fingerprint: the OS and the library fingerprint, then the full editor in
/// three groups, with a line under the fields that need one. A fingerprint
/// this computer cannot back asks first, as everywhere. A template's form
/// (#145) says where each profile's fingerprint comes from instead of its
/// hardware and screen: those are drawn for each profile.
export function FingerprintTab({ draft }: { draft: ProfileFormDraft }) {
  const t = useT();
  const fingerprints = useProfile((s) => s.fingerprints);
  const f = draft.form;
  const u = <K extends keyof ProfileForm>(k: K, v: ProfileForm[K]) => patchForm({ [k]: v } as Partial<ProfileForm>);
  // A pick that lands after the form moved on (closed, or opened on another) changes nothing.
  const patchFor = (id: string, changes: Partial<ProfileForm>) => {
    if (useProfileForm.getState().draft?.form.id === id) patchForm(changes);
  };
  const pick = useFingerprintPick(f, patchFor, fingerprints);
  const template = useProfileForm((s) => s.mode === "template");
  const lib = fingerprints.find((x) => x.id === f.gpu_preset_id) ?? null;
  return (
    <div className="flex flex-col gap-6">
      <Group title={t("profileForm.groupHardware")}>
        {pick.missing && <p data-template-missing className="m-0 text-secondary text-text-sub-600">{t("peek.templateMissing")}</p>}
        {/* What the walk through the form points at: the computer this profile looks like. */}
        <div data-tour="form-fingerprint" className="flex flex-col gap-4">
          <OsField pick={pick} />
          {/* A template keeps no graphics-card choice: each profile's fingerprint is drawn whole. */}
          <TemplateField f={f} u={template ? undefined : u} pick={pick} label={t("peek.template")} help={t("profileForm.gpuHelp")} />
        </div>
        {template ? (
          <>
            <SourceField value={draft.sourceKind ?? "library"} onChange={setSourceKind} library={lib} os={pick.osFilter} />
            <p data-template-drawn className="m-0 text-secondary text-text-sub-600">{t("templates.drawnEach")}</p>
          </>
        ) : (
          <>
            <HardwareFields f={f} u={u} os={pick.osFilter} />
            <ScreenField f={f} u={u} os={pick.osFilter} />
          </>
        )}
        <WindowFields f={f} u={u} />
      </Group>
      <Group title={t("profileForm.groupLocale")}>
        <div data-tour="form-locale"><LocaleFields f={f} u={u} /></div>
        <NoiseFields f={f} u={u} help={t("profileForm.noiseHelp")} />
      </Group>
      <Group title={t("profileForm.groupPrivacy")}>
        <PrivacyFields f={f} u={u} webrtcHelp={t("profileForm.webrtcHelp")} onInvalid={setFieldInvalid} />
        <MediaFields f={f} u={u} />
      </Group>
      {pick.warning}
    </div>
  );
}
