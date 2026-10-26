import { useEffect, useMemo, useRef, useState } from "react";
import { Badge, Button, DataList, EmptyState } from "@proxyshard/shardx-ui-kit";
import { AddIcon, DownloadIcon, AppleOsIcon, WindowsOsIcon, LinuxOsIcon, AndroidOsIcon, ComputerIcon, NavFingerprintsIcon } from "../../shared/icons";
import { fingerprintOs, useFingerprint, useVisibleFingerprintGroups, FingerprintCard, type FingerprintEntry } from "../../entities/fingerprint";
import { useT } from "../../shared/i18n";
import { useArrivedIds } from "../../shared/hooks/useArrivedIds";
import { LoadError, LoadingCards } from "../../shared/ui/ListStates";
import { rowActionIcons } from "../../shared/ui/rowActionIcons";
import { useNewProfileFrom } from "../FingerprintPeek";
import { useScrollMemory } from "../../shared/model/scrollMemory";
import { toast } from "../../shared/model/toast";

// Structural icons are neutral: an orange Android glyph read as a warning next
// to the orange "GPU mismatch" badge (UI-38). An unknown platform is a
// generic computer, not an Apple.
function PlatformIcon({ platform }: { platform: string }) {
  const p = platform.toLowerCase();
  const cls = "size-3.5 text-icon-sub-600";
  // Before the switch, and on a prefix: navigator.platform on a handset is
  // "Linux armv81", which an equality switch would miss.
  if (p.startsWith("android") || p.startsWith("linux arm")) return <AndroidOsIcon data-platform-icon className={cls} />;
  switch (p) {
    case "macos":
      return <AppleOsIcon data-platform-icon className={cls} />;
    case "windows":
      return <WindowsOsIcon data-platform-icon className={cls} />;
    case "linux":
      return <LinuxOsIcon data-platform-icon className={cls} />;
    default:
      return <ComputerIcon data-platform-icon className={cls} />;
  }
}

/// "New profile from this" from a card's menu: the Peek header's own steps
/// (it waits for this machine's GPU check, asks first when the machine can't
/// back the fingerprint, creates once), started once, when it mounts.
function NewProfileFrom({ entry }: { entry: FingerprintEntry }) {
  const { start, warning } = useNewProfileFrom(entry);
  const started = useRef(false);
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    void start();
  }, [start]);
  return warning;
}

/// `loading` is the page's held first load (`useHeldLoading`).
///
/// The list recipe's card grid (F5, IFACE-11), one group per OS: one Tab
/// stop, arrows move by what is beside, above and below (across the
/// groups), Enter or a click opens Peek, Delete or Backspace removes an
/// imported one, and each card's menu holds New profile from this and Remove. The states center in the same tile a
/// table sits on.
export function FingerprintLibrary({ loading }: { loading: boolean }) {
  const t = useT();
  // The library scrolls itself; it keeps its place across page switches (WIG-06).
  const scrollRef = useRef<HTMLDivElement>(null);
  useScrollMemory(scrollRef, "fingerprints");
  const items = useFingerprint((s) => s.items);
  const status = useFingerprint((s) => s.status);
  const loadError = useFingerprint((s) => s.error);
  const clearFilters = useFingerprint((s) => s.clearFilters);
  const peekId = useFingerprint((s) => s.peekId);
  const openPeek = useFingerprint((s) => s.openPeek);
  const remove = useFingerprint((s) => s.remove);
  const groups = useVisibleFingerprintGroups();
  // The groups' order, flattened: the grid draws them group by group.
  const shown = useMemo(() => groups.flatMap(([, list]) => list), [groups]);
  // An imported fingerprint slides in once.
  const arrived = useArrivedIds(items.map((e) => e.id), status === "ready");
  // Each pick of the menu's New profile from this starts its own run.
  const [newFrom, setNewFrom] = useState<{ entry: FingerprintEntry; n: number } | null>(null);

  const emptyState = items.length === 0 ? (
    <EmptyState
      icon={<NavFingerprintsIcon />}
      title={t("fingerprintLibrary.emptyTitle")}
      hint={t("fingerprintLibrary.emptyHint")}
      action={{ label: t("fingerprintLibrary.emptyAction"), icon: <DownloadIcon />, onClick: () => void useFingerprint.getState().importJsonFile() }}
    />
  ) : (
    <EmptyState
      icon={<NavFingerprintsIcon />}
      title={t("fingerprintLibrary.noMatch")}
      hint={t("fingerprintLibrary.noMatchHint")}
      actions={<Button variant="neutral" mode="stroke" size="xsmall" onClick={clearFilters}>{t("fingerprintFilters.clear")}</Button>}
    />
  );

  return (
    <>
      <DataList<FingerprintEntry>
        variant="grid"
        scrollRef={scrollRef}
        aria-label={t("fingerprints.title")}
        items={shown}
        getId={(e) => e.id}
        getName={(e) => e.label}
        labels={{ selectAll: t("dataList.selectAll"), select: (name) => t("dataList.select", { name }), more: t("pageHeader.more"), actions: t("profileTable.colActions") }}
        status={loading ? "loading" : status === "error" ? "error" : "ready"}
        loading={<LoadingCards />}
        error={<LoadError what={t("listStates.fingerprints")} error={loadError} onRetry={() => void useFingerprint.getState().init()} />}
        empty={emptyState}
        arrived={arrived}
        groupOf={(e) => fingerprintOs(e) || "Other"}
        groupHeader={(g, n) => (
          // 12 px to its cards (the grid's 10 and 2), under the 20 above it
          // from the group before: a title sits closer to what it names
          // (owner, 2026-10-02).
          <div className="flex items-center gap-2.5 pb-[2px]">
            <PlatformIcon platform={g} />
            <h2 className="m-0 text-section-title text-text-strong-950">{g}</h2>
            <Badge color="gray" size="small">{n}</Badge>
          </div>
        )}
        onOpen={(e) => openPeek(e.id)}
        // The one keyboard contract every list has (F28): Delete or Backspace
        // removes an imported fingerprint behind Undo, as its card menu does;
        // a built-in one stays, and a polite status line says why.
        onDelete={(ids) => {
          const gone = items.filter((e) => ids.includes(e.id));
          for (const e of gone) if (!e.builtin) void remove(e.id);
          if (gone.some((e) => e.builtin)) toast.info(t("fingerprintLibrary.builtinNoDelete"));
        }}
        currentId={peekId}
        renderCard={(e) => <FingerprintCard entry={e} inPeek={e.id === peekId} />}
        menuItems={(e) => [
          { label: t("fingerprintPeek.newProfile"), icon: <AddIcon className="size-3.5" />, onSelect: () => setNewFrom((p) => ({ entry: e, n: (p?.n ?? 0) + 1 })) },
          { type: "separator" },
          // A built-in one says why it can't go (as Peek's menu does).
          e.builtin
            ? { label: t("fingerprintPeek.removeItem"), icon: rowActionIcons.trash({ className: "size-3.5" }), disabled: true, hint: t("fingerprintPeek.builtin"), onSelect: () => {} }
            : { label: t("fingerprintPeek.removeItem"), icon: rowActionIcons.trash({ className: "size-3.5" }), danger: true, onSelect: () => void remove(e.id) },
        ]}
      />
      {newFrom && <NewProfileFrom key={newFrom.n} entry={newFrom.entry} />}
    </>
  );
}
