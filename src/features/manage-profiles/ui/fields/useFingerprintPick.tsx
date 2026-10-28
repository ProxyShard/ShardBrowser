import { useEffect, useMemo, useState, type ReactNode } from "react";
import { useLatched } from "@proxyshard/shardx-ui-kit";
import { HOST_OS } from "../../../../shared/lib/utils";
import { useDialogLife } from "../../../../shared/lib/useDialogLife";
import { matchesOs, osIdFor } from "../../../../shared/constants";
import { enrichPicksForPreset, type ProfileForm } from "../../../../entities/profile";
import type { FingerprintEntry } from "../../../../entities/fingerprint";
import { useGpuCompat, verdictsLoaded } from "../../../../shared/model/gpuCompat";
import { IncompatibleWarningModal } from "../../../gpu-compat";

export type FingerprintPick = {
  osFilter: string;
  pickOs: (os: string) => void;
  gpusForOs: FingerprintEntry[];
  /** Applies a pick that fits this machine, and asks first otherwise. */
  chooseGpu: (id: string) => void;
  /** The profile's template is no longer in the library. Nothing changes
   *  until the user picks another. */
  missing: boolean;
  /** The "this machine can't back that GPU" dialog, when one is pending. */
  warning: ReactNode;
  /** This machine's GPU check is still running: a pick waits for it (PC-067). */
  checking: boolean;
};

/// The OS and library-fingerprint (GPU) choice shared by the editor's groups:
/// the OS filter, and the pick with its compatibility check. Nothing here
/// changes the form's draft unless the user picks.
export function useFingerprintPick(
  f: ProfileForm,
  /** Applies changes to the draft of profile `id`'s form, if it is still the one open. */
  patch: (id: string, changes: Partial<ProfileForm>) => void,
  fingerprints: FingerprintEntry[],
): FingerprintPick {
  // OS filter init from bound fingerprint's platform; new profile uses host OS.
  const currentFp = fingerprints.find((x) => x.id === f.gpu_preset_id);
  const [osFilter, setOsFilter] = useState<string>(
    currentFp ? osIdFor(currentFp.platform as string) : HOST_OS
  );
  const gpusForOs = useMemo(
    () => fingerprints.filter((fp) => matchesOs(fp.platform, osFilter)),
    [fingerprints, osFilter],
  );
  const missing = fingerprints.length > 0 && !currentFp;

  // The verdict map and the "stop warning me" flag; the load is once per app run.
  const compatById = useGpuCompat((s) => s.byId);
  const loadCompat = useGpuCompat((s) => s.load);
  useEffect(() => { void loadCompat(); }, [loadCompat]);
  // A pick held back until the operator has seen what it costs.
  const [pendingGpu, setPendingGpu] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  // The question stays mounted, on the pick it asked about, through its exit (FEEL-01).
  const warningLife = useDialogLife(pendingGpu != null);
  const pendingGpuShown = useLatched(pendingGpu);

  /// Pick GPU = full fingerprint snap; toStored carries lib.payload at save.
  const setGpu = async (id: string) => {
    const fp = fingerprints.find((x) => x.id === id);
    if (!fp) return;
    const forId = f.id;
    const nav = fp.payload?.navigator ?? {};
    // Ask Rust for the same hw + platform_version triplet save uses.
    let picks: { hardware_concurrency?: number; device_memory?: number; platform_version?: string } = {};
    try {
      picks = await enrichPicksForPreset(id);
    } catch {
      // Fall back to preset's nav defaults if Rust enrich fails.
    }
    patch(forId, {
      gpu_preset_id: id,
      hardware_concurrency: picks.hardware_concurrency ?? nav.hardware_concurrency ?? f.hardware_concurrency,
      device_memory: picks.device_memory ?? nav.device_memory ?? f.device_memory,
      // The picked template's own version, never the form's: switching from a
      // Mac template to a phone one would otherwise carry macOS 15.6.1 onto the
      // handset, which is the pair no real phone reports.
      platform_version: picks.platform_version ?? nav.platform_version ?? "",
      user_agent: nav.user_agent ?? f.user_agent,
    });
    // A pick of another OS (From template) moves the OS control with it.
    setOsFilter(osIdFor(fp.platform));
  };

  // Waits up to a second for this machine's verdicts; the rows say "Checking this
  // computer…" only while they are really on their way (no one-frame flash).
  const checked = async () => {
    const waiting = !useGpuCompat.getState().loaded;
    if (waiting) setChecking(true);
    await verdictsLoaded();
    if (waiting) setChecking(false);
  };

  // Decided on the verdicts when they come within that second; a check that
  // hangs lets the pick through, and the badge says it once the check ends.
  const chooseGpu = (id: string) => {
    void (async () => {
      await checked();
      const { byId, suppressed } = useGpuCompat.getState();
      const verdict = byId[id];
      if (verdict && !verdict.compatible && !suppressed) {
        setPendingGpu(id);
        return;
      }
      await setGpu(id);
    })();
  };

  // Another OS: a fingerprint of it this machine can back, as a new profile gets.
  const pickOs = (os: string) => {
    setOsFilter(os);
    // "This computer's graphics card" is only ever offered while the profile
    // claims this computer's system. A Windows profile reporting a Mac's card
    // is the mismatch the whole option exists to avoid, so moving the OS away
    // takes the option back.
    if (os !== HOST_OS && f.real_gpu) {
      patch(f.id, { real_gpu: false });
    }
    if (currentFp && matchesOs(currentFp.platform, os)) return;
    void (async () => {
      await checked();
      const verdicts = useGpuCompat.getState().byId;
      const sameOs = fingerprints.filter((g) => matchesOs(g.platform, os));
      const fitting = sameOs.filter((g) => verdicts[g.id]?.compatible !== false);
      const pool = fitting.length > 0 ? fitting : sameOs;
      const pick = pool[Math.floor(Math.random() * pool.length)];
      if (pick) await setGpu(pick.id);
    })();
  };

  const warning = warningLife.mounted && pendingGpuShown && compatById[pendingGpuShown] ? (
    <IncompatibleWarningModal
      key={warningLife.session}
      open={pendingGpu != null}
      compat={compatById[pendingGpuShown]}
      onKeep={() => {
        const id = pendingGpuShown;
        setPendingGpu(null);
        void setGpu(id);
      }}
      onCancel={() => setPendingGpu(null)}
    />
  ) : null;

  return { osFilter, pickOs, gpusForOs, chooseGpu, missing, warning, checking };
}
