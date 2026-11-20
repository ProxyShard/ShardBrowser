import { useState } from "react";
import { Checkbox, DialogModal, Input, useLatched } from "@proxyshard/shardx-ui-kit";
import { useT } from "../../../shared/i18n";
import { useDialogLife } from "../../../shared/lib/useDialogLife";
import { useProfile } from "../../../entities/profile";
import { TEMPLATE_NAME_MAX, defaultTemplate, useTemplates } from "../../../entities/template";
import { teamRefusal, useRightsChange } from "../../../entities/space";
import { closeSaveTemplate, submitSaveTemplate, templateNameProblem, useSaveTemplate, type TemplateBase } from "../model/saveTemplate";
import type { SourceKind } from "../model/template";
import { SourceField } from "./SourceField";

/// "Save as template" (#145): a name, where each new profile's fingerprint comes
/// from, and whether "New profile" starts from it. Mounted once in the app
/// shell, over the profile form when it is opened from there.
export function SaveTemplateDialog() {
  const base = useSaveTemplate((s) => s.base);
  const life = useDialogLife(base != null);
  const shown = useLatched(base);
  if (!life.mounted || !shown) return null;
  return <SaveTemplateBody key={life.session} base={shown} open={base != null} />;
}

function SaveTemplateBody({ base, open }: { base: TemplateBase; open: boolean }) {
  const t = useT();
  const saving = useSaveTemplate((s) => s.saving);
  const lib = useProfile((s) => s.fingerprints.find((f) => f.id === base.library) ?? null);
  const [name, setName] = useState(base.name);
  const [kind, setKind] = useState<SourceKind>(lib ? "library" : "os");
  const [isDefault, setDefault] = useState(false);
  const [tried, setTried] = useState(false);
  useTemplates((s) => s.templates);
  // A team's templates are its own: making one needs Create there.
  useRightsChange();
  const refused = teamRefusal("create");
  const problem = templateNameProblem(name);
  // A taken name says so as it is typed; an empty one once Save was pressed.
  const shownProblem = problem && (tried || name.trim()) ? problem : undefined;
  const current = defaultTemplate();
  const submit = async () => {
    setTried(true);
    if (problem || refused) return;
    await submitSaveTemplate({ name, kind, isDefault });
  };
  return (
    <DialogModal
      open={open}
      onClose={closeSaveTemplate}
      dirty={name !== base.name || isDefault}
      title={t("templates.saveTitle")}
      confirmLabel={saving ? t("templates.saving") : t("templates.save")}
      onConfirm={() => void submit()}
      isLoading={saving}
      isDisabled={!!refused || (tried && !!problem)}
      cancelLabel={t("confirm.cancel")}
      onCancel={closeSaveTemplate}
      footerNote={refused ?? undefined}
    >
      <div data-save-template className="flex flex-col gap-4 py-4">
        <p className="m-0 text-secondary text-text-sub-600">{t("templates.saveHelp")}</p>
        <Input
          label={t("templates.name")}
          inputSize="xsmall"
          autoFocus
          required
          value={name}
          maxLength={TEMPLATE_NAME_MAX}
          aria-invalid={!!shownProblem || undefined}
          error={shownProblem}
          onChange={(e) => setName(e.target.value)}
        />
        <SourceField value={kind} onChange={setKind} library={lib} os={base.os} disabled={saving} />
        <div className="flex flex-col gap-1">
          <Checkbox label={t("templates.makeDefault")} checked={isDefault} disabled={saving} onChange={(e) => setDefault(e.target.checked)} />
          {isDefault && current && <p className="m-0 text-help text-text-sub-600">{t("templates.replacesDefault", { name: current.name })}</p>}
        </div>
      </div>
    </DialogModal>
  );
}
