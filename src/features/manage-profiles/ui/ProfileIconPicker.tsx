import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { Button } from "@proxyshard/shardx-ui-kit";
import { useT } from "../../../shared/i18n";

export function ProfileIconPicker({ name, color, value, onChange, onBusyChange }: {
  name: string;
  color: string;
  value: string;
  onChange: (value: string) => void;
  onBusyChange: (busy: boolean) => void;
}) {
  const t = useT();
  const [preview, setPreview] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [previewError, setPreviewError] = useState("");
  const currentChange = useRef(onChange);
  currentChange.current = onChange;
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  useEffect(() => {
    let active = true;
    const timer = setTimeout(() => {
      invoke<string>("profile_icon_preview", {
        name: name || "untitled", color: color || null, iconPng: value || null,
      }).then((url) => {
        if (active) { setPreview(url); setPreviewError(""); }
      }).catch((e) => {
        if (active) { setPreview(null); setPreviewError(String(e)); }
      });
    }, 150);
    return () => { active = false; clearTimeout(timer); };
  }, [name, color, value]);

  const choose = async () => {
    setBusy(true);
    onBusyChange(true);
    setError("");
    try {
      const path = await open({
        multiple: false, directory: false, title: t("profileIcon.dialogTitle"),
        filters: [{ name: t("profileIcon.imageFiles"), extensions: ["png", "jpg", "jpeg", "webp"] }],
      });
      if (typeof path !== "string" || !mounted.current) return;
      const encoded = await invoke<string>("profile_icon_import", { path });
      if (mounted.current) currentChange.current(encoded);
    } catch (e) {
      if (mounted.current) setError(String(e));
    } finally {
      if (mounted.current) { setBusy(false); onBusyChange(false); }
    }
  };

  return (
    <div className="flex flex-col gap-1.5">
      <span className="text-label-base font-medium text-text-strong-900">{t("profileIcon.label")}</span>
      <div className="flex items-center gap-3">
        <div className="flex h-20 w-20 shrink-0 items-center justify-center" aria-busy={!preview}>
          {preview && <img src={preview} alt={t("profileIcon.previewAlt")} width={80} height={80} />}
        </div>
        <div className="flex min-w-0 flex-col items-start gap-2">
          <div className="flex flex-wrap gap-2">
            <Button variant="neutral" mode="stroke" size="2xsmall" isLoading={busy} onClick={() => void choose()}>
              {value ? t("profileIcon.change") : t("profileIcon.choose")}
            </Button>
            {value && <Button variant="neutral" mode="ghost" size="2xsmall" disabled={busy} onClick={() => {
              setError(""); currentChange.current("");
            }}>{t("profileIcon.reset")}</Button>}
          </div>
          <span className="text-paragraph-xs text-text-soft-400">{t("profileIcon.formats")}</span>
        </div>
      </div>
      <p className="m-0 text-paragraph-xs text-text-soft-400">{t("profileIcon.help")}</p>
      {(error || previewError) && <p role="alert" className="m-0 text-paragraph-xs text-error-base">{error || previewError}</p>}
    </div>
  );
}
