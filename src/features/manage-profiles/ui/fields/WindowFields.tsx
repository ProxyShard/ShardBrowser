import { Switch } from "@proxyshard/shardx-ui-kit";
import { claimsMobile } from "../../../../entities/profile";
import { useT } from "../../../../shared/i18n";
import { SectionHeading } from "./SectionHeading";
import type { FieldProps } from "./types";

/// How the profile's window behaves, next to the screen it claims: focus, and
/// on a desktop profile whether it looks maximized. Shown for every OS — unlike
/// the resolution, nothing here depends on the host.
export function WindowFields({ f, u }: FieldProps) {
  const t = useT();
  return (
    <>
      <div className="mt-2.5">
        <SectionHeading>{t("inlineEditor.windowHeading")}</SectionHeading>
      </div>
      <Switch
        label={t("inlineEditor.keepFocusLabel")}
        checked={f.keep_focus}
        onChange={(checked) => u("keep_focus", checked)}
      />
      <p className="m-0 -mt-1 text-secondary text-text-soft-400">
        {t("inlineEditor.keepFocusHelp")}
      </p>
      {/* A phone has no window to maximize. */}
      {!claimsMobile(f) && (
        <>
          <Switch
            label={t("inlineEditor.maximizedLabel")}
            checked={f.maximized}
            onChange={(checked) => u("maximized", checked)}
          />
          <p className="m-0 -mt-1 text-secondary text-text-soft-400">
            {t("inlineEditor.maximizedHelp")}
          </p>
        </>
      )}
    </>
  );
}
