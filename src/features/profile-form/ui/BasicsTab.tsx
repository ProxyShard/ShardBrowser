import { useId, useState, type RefObject } from "react";
import { Input } from "@proxyshard/shardx-ui-kit";
import { PropertySheet } from "../../../shared/ui/peek";
import { useT } from "../../../shared/i18n";
import { addTagName, patchDraft, patchForm, problemsOf, setFormPassword, useProfileForm, type ProfileFormDraft } from "../model/useProfileForm";
import { KeyIcon } from "../../../shared/icons";
import { PASSWORD_MIN, passwordProblem } from "../../../entities/profile";
import { useProfilePasswords } from "../../../entities/settings";
import { ColorRow, CookiesRow, NotesRow } from "./fields";
import { FolderField } from "./FolderField";
import { teamRefusal, useInTeam, useRightsChange } from "../../../entities/space";
import { TagsField } from "./TagsField";

/// Basics: the name (required), its Folder, tags, color and notes, and a new
/// profile's password while Settings allows one. The parts the walk through
/// this form points at carry `data-tour`.
export function BasicsTab({ draft, nameRef }: { draft: ProfileFormDraft; nameRef: RefObject<HTMLInputElement | null> }) {
  const t = useT();
  const tried = useProfileForm((s) => s.tried);
  // Said under the field once it was left, or Save was pressed: never while typing the first letter.
  const [blurred, setBlurred] = useState(false);
  const problem = problemsOf(draft).name;
  const f = draft.form;
  // A team's: its profiles take no cookies file yet, and a new tag needs Create in the team.
  const team = useInTeam();
  useRightsChange();
  const keep = useProfileForm((s) => (s.mode === "edit" ? s.saved?.folder : undefined));
  // A protected profile takes no cookies file: its cookies are sealed with it.
  const locked = useProfileForm((s) => s.unlocked !== null);
  const creating = useProfileForm((s) => s.mode === "create");
  const passwords = useProfilePasswords((s) => s.on);
  return (
    <div className="flex flex-col gap-4">
      <div data-tour="form-name">
        <Input
          ref={nameRef}
          label={t("profileForm.name")}
          inputSize="xsmall"
          wrapperClassName="gap-[6px]"
          required
          aria-invalid={!!problem || undefined}
          value={f.name}
          error={problem && (blurred || tried) ? problem : undefined}
          onChange={(e) => patchForm({ name: e.target.value })}
          onBlur={() => setBlurred(true)}
        />
      </div>
      <PropertySheet>
        {/* 2 px between 32 px rows, as Peek's sheet. */}
        <div className="flex flex-col gap-[2px]">
          <div data-tour="form-organize" className="flex flex-col gap-[2px]">
            <FolderField value={draft.folder} keep={keep} onChange={(folder) => patchDraft({ folder })} />
            <TagsField
              value={draft.tagIds}
              newNames={draft.newTags}
              onChange={(tagIds) => patchDraft({ tagIds })}
              onNewNamesChange={(newTags) => patchDraft({ newTags })}
              onCreate={teamRefusal("create") ? undefined : addTagName}
            />
          </div>
          <ColorRow value={f.color} onChange={(color) => patchForm({ color })} />
          <NotesRow value={f.notes} onChange={(notes) => patchForm({ notes })} />
          {!team && !locked && <CookiesRow value={draft.cookies} onChange={(cookies) => patchDraft({ cookies })} />}
        </div>
      </PropertySheet>
      {creating && passwords && <PasswordFields />}
    </div>
  );
}

/// A new profile's password (#127), optional: set once the profile is made.
/// Each field says what is wrong with it once it was left, or Save was pressed.
function PasswordFields() {
  const t = useT();
  const password = useProfileForm((s) => s.password);
  const repeat = useProfileForm((s) => s.passwordRepeat);
  const tried = useProfileForm((s) => s.tried);
  const id = useId();
  const [left, setLeft] = useState({ password: false, repeat: false });
  const problem = passwordProblem(password, repeat);
  const short = problem === "short" && (tried || left.password) ? t("errors.passwordTooShort") : undefined;
  const mismatch = problem === "mismatch" && (tried || left.repeat) ? t("profileLock.mismatch") : undefined;
  const field = { inputSize: "xsmall", wrapperClassName: "gap-[6px]", secret: true, type: "password", leftIcon: <KeyIcon className="size-3.5" /> } as const;
  return (
    <section data-form-password aria-labelledby={id} aria-describedby={`${id}-help`} className="flex flex-col gap-3">
      <div className="flex flex-col gap-0.5">
        <h3 id={id} className="m-0 text-section-label text-text-soft-400">{t("profileForm.passwordTitle")}</h3>
        <p id={`${id}-help`} className="m-0 text-help text-text-sub-600">{t("profileForm.passwordHelp", { n: PASSWORD_MIN })}</p>
      </div>
      <div className="grid grid-cols-2 gap-3">
        <Input
          {...field}
          label={t("profileLock.passwordLabel")}
          value={password}
          error={short}
          aria-invalid={!!short || undefined}
          onChange={(e) => setFormPassword({ password: e.target.value })}
          onBlur={() => setLeft((l) => ({ ...l, password: true }))}
        />
        <Input
          {...field}
          label={t("profileLock.repeatLabel")}
          value={repeat}
          error={mismatch}
          aria-invalid={!!mismatch || undefined}
          onChange={(e) => setFormPassword({ passwordRepeat: e.target.value })}
          onBlur={() => setLeft((l) => ({ ...l, repeat: true }))}
        />
      </div>
      <p data-no-recovery className="m-0 text-help text-warning-text">{t("profileLock.noRecovery")}</p>
    </section>
  );
}
