import { Pair } from "../../../../shared/ui/Pair";
import { PortList } from "../../../../shared/ui/PortList";
import { useT } from "../../../../shared/i18n";
import { SectionHeading } from "./SectionHeading";
import type { FieldProps } from "./types";

/** `help`: a line under the heading that says what noise does (the profile form). */
export function NoiseFields({ f, u, help }: FieldProps & { help?: string }) {
  const t = useT();
  return (
    <>
      <div className="mt-1.5">
        <SectionHeading>{t("inlineEditor.noiseHeading")}</SectionHeading>
        {help && <p className="m-0 text-secondary text-text-soft-400">{help}</p>}
      </div>
      <div className="grid grid-cols-2 gap-y-[16px] gap-x-[12px]">
        <Pair label={t("inlineEditor.noiseCanvas")}        value={f.noise_canvas}        on={(v) => u("noise_canvas", v)} />
        <Pair label={t("inlineEditor.noiseWebgl")}         value={f.noise_webgl}         on={(v) => u("noise_webgl", v)} />
        <Pair label={t("inlineEditor.noiseAudio")}         value={f.noise_audio}         on={(v) => u("noise_audio", v)} />
        {/* Noised DOM sizes are ones no real Chrome reports, so sites can tell. */}
        <Pair label={t("inlineEditor.noiseClientRects")}   value={f.noise_client_rects}  on={(v) => u("noise_client_rects", v)}
              note={t("inlineEditor.noiseClientRectsNote")} warn={f.noise_client_rects === "auto"} />
        <Pair label={t("inlineEditor.noiseSensors")}       value={f.noise_sensors}       on={(v) => u("noise_sensors", v)} />
        <Pair label={t("inlineEditor.noiseFonts")}         value={f.noise_fonts}         on={(v) => u("noise_fonts", v)} />
      </div>

      <PortList
        label={t("inlineEditor.blockedPortsLabel")}
        value={f.blocked_ports}
        onChange={(v) => u("blocked_ports", v)}
      />
    </>
  );
}
