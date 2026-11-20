import { useEffect } from "react";
import { Button, IconButton, Switch } from "@proxyshard/shardx-ui-kit";
import { AddIcon, DeleteIcon, EditIcon } from "../../../shared/icons";
import { deferDelete } from "../../../shared/lib/deferDelete";
import { toast } from "../../../shared/model/toast";
import { useT } from "../../../shared/i18n";
import { useProfile } from "../../../entities/profile";
import { templateDelete, templateDeleteKey, useTemplates, type ProfileTemplate } from "../../../entities/template";
import { teamRefusal, useRightsChange } from "../../../entities/space";
import { openTemplateForm } from "../../../features/profile-form";
import { focusAfterRowDelete } from "../rowDelete";
import { SettingsGroup } from "../SettingRow";

/// Profile templates (#145): what "New profile" can start from. Each opens in
/// the profile form to change it; one may be the default; a delete runs at
/// once, behind its Undo. In a team they are the team's.
export function TemplatesSection() {
  const t = useT();
  const templates = useTemplates((s) => s.templates);
  useEffect(() => { void useTemplates.getState().load(); }, []);
  useRightsChange();
  const noCreate = teamRefusal("create");
  return (
    <SettingsGroup list help={templates.length > 0 ? t("settings.templatesHelp") : undefined}>
      {templates.length === 0 ? (
        <p className="m-0 py-6 text-center text-body text-balance text-text-sub-600">{t("settings.templatesEmpty")}</p>
      ) : (
        <ul data-template-list className="m-0 flex list-none flex-col p-0">
          {templates.map((x) => <TemplateRow key={x.id} tpl={x} />)}
        </ul>
      )}
      <div className="flex px-cell py-3">
        <Button
          variant="neutral"
          mode="stroke"
          size="2xsmall"
          leftIcon={<AddIcon className="size-3.5" />}
          disabled={!!noCreate}
          aria-description={noCreate ?? undefined}
          onClick={() => openTemplateForm(null)}
        >
          {t("templates.new")}
        </Button>
      </div>
    </SettingsGroup>
  );
}

function TemplateRow({ tpl }: { tpl: ProfileTemplate }) {
  const t = useT();
  const fp = useProfile((s) => (tpl.source.library ? s.fingerprints.find((f) => f.id === tpl.source.library) : undefined));
  const noEdit = teamRefusal("edit");
  const noDelete = teamRefusal("delete");
  const source = fp ? t("templates.summaryLibrary", { name: fp.label }) : t("templates.summaryOs", { os: tpl.source.os || "?" });

  const setDefault = (on: boolean) => {
    if (noEdit) return;
    useTemplates.getState().save({ ...tpl, is_default: on })
      .catch((e) => toast.fail({ title: t("templates.saveFailed", { name: tpl.name }), detail: e, subject: tpl.name }));
  };

  // Out of the list at once, sent when the Undo goes. The keyboard goes to the next row's Delete.
  const del = (button: HTMLElement) => {
    if (noDelete) return;
    const next = focusAfterRowDelete(button);
    deferDelete({
      key: templateDeleteKey(tpl.id),
      title: t("templates.deleted", { name: tpl.name }),
      failTitle: t("templates.deleteFailed", { name: tpl.name }),
      hide: () => useTemplates.getState().hide(tpl.id),
      restore: () => useTemplates.getState().show(tpl),
      commit: async () => { await templateDelete(tpl.id); },
    });
    next();
  };

  return (
    <li data-template-row={tpl.id} className="row-line relative flex items-center gap-4 px-cell py-3">
      <div className="flex min-w-0 flex-1 flex-col">
        <span className="truncate text-body text-text-strong-950">{tpl.name}</span>
        <span className="truncate text-secondary text-text-sub-600">
          {tpl.folder ? t("templates.summaryInFolder", { source, folder: tpl.folder }) : source}
        </span>
      </div>
      <Switch
        role="switch"
        aria-label={t("templates.defaultFor", { name: tpl.name })}
        label={t("templates.defaultShort")}
        checked={tpl.is_default}
        disabled={!!noEdit}
        onChange={setDefault}
      />
      <IconButton
        size="2xsmall"
        icon={<EditIcon className="size-3.5" />}
        label={t("templates.edit", { name: tpl.name })}
        onClick={() => openTemplateForm(tpl.id)}
      />
      <IconButton
        data-row-delete
        size="2xsmall"
        icon={<DeleteIcon className="size-3.5" />}
        label={noDelete ? t("rights.labelWhy", { label: t("templates.delete", { name: tpl.name }), why: noDelete }) : t("templates.delete", { name: tpl.name })}
        disabled={!!noDelete}
        onClick={(e) => del(e.currentTarget)}
      />
    </li>
  );
}
