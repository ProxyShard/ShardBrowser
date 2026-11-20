import { SelectField } from "../../../shared/ui/SelectField";
import { useT } from "../../../shared/i18n";
import type { FingerprintEntry } from "../../../entities/fingerprint";
import type { SourceKind } from "../model/template";

/// Where each profile made from a template gets its fingerprint (#145): this
/// library fingerprint, drawn anew for each, or any of its OS. Never a copy.
export function SourceField({ value, onChange, library, os, disabled }: {
  value: SourceKind;
  onChange: (kind: SourceKind) => void;
  /** The library fingerprint it can be drawn from; none offers only the OS. */
  library: FingerprintEntry | null;
  os: string;
  disabled?: boolean;
}) {
  const t = useT();
  return (
    <div data-template-source className="flex flex-col gap-1">
      <SelectField<SourceKind>
        label={t("templates.sourceLabel")}
        value={library ? value : "os"}
        disabled={disabled}
        onChange={onChange}
        options={[
          ...(library ? [{ value: "library" as const, label: t("templates.sourceLibrary", { name: library.label }) }] : []),
          { value: "os" as const, label: t("templates.sourceOs", { os }) },
        ]}
      />
      <p className="m-0 text-help text-text-sub-600">{t("templates.sourceHelp")}</p>
    </div>
  );
}
