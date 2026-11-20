import { useEffect, useRef, useState, type ReactNode } from "react";
import { Badge, Button, Label, Tooltip, usePresence, useLatched } from "@proxyshard/shardx-ui-kit";
import { BuildingIcon, ClockIcon, EditIcon, GlobeIcon, RefreshIcon } from "../../shared/icons";
import { CountryFlag } from "../../shared/ui/CountryFlag";
import { ProxyTestPill } from "../../shared/ui/StatusPill";
import { PeekBody, PeekHeader, PeekPanel, PeekSection } from "../../shared/ui/peek";
import { explainError } from "../../shared/lib/errors";
import { fmtTs } from "../../shared/lib/utils";
import { focusProxyRow } from "../../shared/lib/focusRow";
import { useT } from "../../shared/i18n";
import { proxyChangeIp, proxyHistory, proxyLastTest, proxyStatus, useProxy, useRotation, type ProxyEntry, type ProxyTestSnapshot } from "../../entities/proxy";
import { toast } from "../../shared/model/toast";
import { useProfileUi } from "../../entities/profile";
import { onProxy, teamRefusal, useRightsChange } from "../../entities/space";
import { capOf, command, menuItemsFor } from "../../features/commands";
import { EditProxyDialog } from "../../features/manage-proxies";
import { Details } from "./Details";

/// The side panel that shows and tests one proxy while the list stays in
/// view, as Profile Peek does for a profile. It only shows: Edit in the
/// header opens the dialog, where a change is saved by hand. The shell
/// (frame, header, sections, the way in and out) is every Peek's (F1).
export function ProxyPeek() {
  const t = useT();
  const peekId = useProxy((s) => s.peekId);
  const closePeek = useProxy((s) => s.closePeek);
  const proxy = useProxy((s) => (peekId ? s.proxies.find((p) => p.id === peekId) : undefined));
  /** The proxy the Edit dialog is open on. */
  const [editing, setEditing] = useState<string | null>(null);

  // The proxy left the list (deleted): Peek goes with it.
  const shownId = useRef<string | null>(null);
  useEffect(() => {
    if (proxy) shownId.current = proxy.id;
    else if (peekId && shownId.current === peekId) closePeek();
  }, [peekId, proxy, closePeek]);

  // Kept through the exit: the panel leaves showing what it showed (FEEL-02).
  const open = peekId && proxy ? proxy : null;
  const presence = usePresence(open?.id ?? null, { maxExitMs: 250 });
  const p = useLatched(open);
  if (!presence.mounted || !p) return null;
  const name = p.name || `${p.host}:${p.port}`;
  const close = () => {
    // From inside Peek (its Close), the keyboard goes back to the row: the
    // panel is inert while it leaves.
    const inside = document.activeElement?.closest("aside[data-peek]") != null;
    closePeek();
    focusProxyRow(p.id, { ifLost: !inside });
  };

  return (
    <>
      <PeekPanel kind="proxy" label={t("proxyPeek.label", { name })} presence={presence}>
        <ProxyPeekHeader name={name} proxy={p} onEdit={() => setEditing(p.id)} onClose={close} />
        <PeekBody>
          <PeekSection id="details" title={t("proxyPeek.details")}>
            {/* Keyed by the proxy: Peek never unmounts while the arrows move it,
                and a password shown for one never shows for the next. */}
            <Details key={p.id} proxy={p} />
          </PeekSection>
          <PeekSection id="test" title={t("proxyPeek.test")}>
            <TestSection proxy={p} />
          </PeekSection>
          <PeekSection id="profiles" title={t("proxyPeek.profiles")}>
            <ProfilesUsing proxyId={p.id} />
          </PeekSection>
        </PeekBody>
      </PeekPanel>
      <EditProxyDialog id={editing} onClose={() => setEditing(null)} />
    </>
  );
}

function ProxyPeekHeader({ name, proxy, onEdit, onClose }: {
  name: string;
  proxy: ProxyEntry;
  onEdit: () => void;
  onClose: () => void;
}) {
  const t = useT();
  const snap = useProxy((s) => s.snapshots[proxy.id]);
  const busy = useProxy((s) => !!s.proxyTesting[proxy.id]);
  const testProxy = useProxy((s) => s.testProxy);
  const testStatus = proxyStatus(snap);
  // The registry's Peek menu (E3, R9): Copy address and Delete…, as the
  // row menu has them. Test is the header's own Test now.
  const menuItems = menuItemsFor("peekMenu", "proxies", { ids: [proxy.id] });
  // A team's proxy without Edit for this person: Edit says why and does nothing.
  useRightsChange();
  const noEdit = teamRefusal("edit", onProxy(proxy.id));
  return (
    <PeekHeader
      subject={proxy.id}
      title={<h2 className="m-0 min-w-0 truncate text-panel-title text-text-strong-950">{name}</h2>}
      // The Live layer's test pill (spec 4.2). The Proxies row still draws a
      // dot and a word until the pair that rebuilds that list adopts it.
      status={
        testStatus === "ok"
          ? <ProxyTestPill data-peek-test-status state="ok" ms={snap!.tcp_ms!} busy={busy} />
          : <ProxyTestPill data-peek-test-status state={testStatus} busy={busy} />
      }
      extra={
        <Tooltip content={noEdit ?? undefined}>
          <Button
            variant="neutral" mode="stroke" size="3xsmall" data-peek-edit leftIcon={<EditIcon className="size-3.5" />}
            disabled={!!noEdit} aria-description={noEdit ?? undefined}
            onClick={onEdit}
          >
            {t("peek.edit")}
          </Button>
        </Tooltip>
      }
      primary={
        // The test runs on the proxy as it is saved.
        <Button
          variant="primary" mode="lighter" size="3xsmall" leftIcon={<RefreshIcon className="size-3.5" />}
          isLoading={busy}
          onClick={() => void testProxy(proxy)}
        >
          {t("proxyPeek.testNow")}
        </Button>
      }
      menuItems={menuItems}
      menuLabel={t("pageHeader.more")}
      closeLabel={t("peek.close")}
      closeShortcut={capOf(command("proxy.closePeek"))}
      onClose={onClose}
    />
  );
}

/// The last test's result and the IP history; "Test now" is the header's
/// main action (LAYOUT-12).
function TestSection({ proxy }: { proxy: ProxyEntry }) {
  const t = useT();
  const snap = useProxy((s) => s.snapshots[proxy.id]);
  const busy = useProxy((s) => !!s.proxyTesting[proxy.id]);
  const [history, setHistory] = useState<ProxyTestSnapshot[]>([]);
  // Newest first; read again after each test of this proxy.
  useEffect(() => {
    let alive = true;
    proxyHistory(proxy.id).then((h) => { if (alive) setHistory([...h].reverse()); }).catch(() => {});
    return () => { alive = false; };
  }, [proxy.id, snap, busy]);

  return (
    <>
      {snap ? <Result proxy={proxy} snap={snap} /> : <p className="m-0 text-secondary text-text-soft-400">{t("proxyPeek.notTested")}</p>}
      <ChangeIp proxy={proxy} />
      <Label as="div" className="mt-1">{t("proxyPeek.history")}</Label>
      {history.length === 0 ? (
        <p className="m-0 text-secondary text-text-soft-400">{t("proxyInfoPopover.historyEmpty")}</p>
      ) : (
        <ul className="m-0 flex list-none flex-col p-0">
          {history.map((s, i) => (
            <li key={`${s.ip}-${s.first_seen}-${i}`} data-history-item data-selectable className="py-2">
              <div className="flex items-center gap-2 text-body text-text-strong-950">
                <span className="mono">{s.ip || "—"}</span>
                {s.country_code && <CountryFlag cc={s.country_code} />}
                {s.city && <span className="text-secondary text-text-soft-400">{s.city}</span>}
              </div>
              <div className="mt-0.5 text-secondary text-text-soft-400">
                {s.first_seen === s.last_seen
                  ? fmtTs(s.first_seen)
                  : t("proxyPeek.historySpan", { from: fmtTs(s.first_seen), to: fmtTs(s.last_seen) })}
              </div>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

/// #162: the provider's change-IP link, for a proxy that has one; then the new exit.
function ChangeIp({ proxy }: { proxy: ProxyEntry }) {
  const t = useT();
  const has = useRotation((s) => s.ids.has(proxy.id));
  const [busy, setBusy] = useState(false);
  useEffect(() => { void useRotation.getState().load(); }, [proxy.id]);
  if (!has) return null;
  const run = async () => {
    setBusy(true);
    try {
      const r = await proxyChangeIp(proxy.id);
      if (r.changed && r.after) toast.ok(t("proxyPeek.ipChanged", { ip: r.after.ip }));
      else toast.info(t("proxyPeek.ipNotChanged"));
      const snap = await proxyLastTest(proxy.id);
      if (snap) useProxy.setState((st) => ({ snapshots: { ...st.snapshots, [proxy.id]: snap } }));
    } catch (e) {
      toast.err(t("proxyPeek.changeIpFailed"), { detail: explainError(e) ?? String(e) });
    } finally {
      setBusy(false);
    }
  };
  return (
    <div>
      <Button variant="neutral" mode="stroke" size="3xsmall" data-change-ip leftIcon={<RefreshIcon className="size-3.5" />} isLoading={busy} onClick={() => void run()}>
        {t("proxyPeek.changeIp")}
      </Button>
    </div>
  );
}

function Result({ proxy, snap }: { proxy: ProxyEntry; snap: ProxyTestSnapshot }) {
  const t = useT();
  const status = proxyStatus(snap);
  const place = [snap.region, snap.city].filter(Boolean).join(", ") || snap.country;
  return (
    // The last test's facts, strong 13 px, 14 px apart (owner, "more air").
    <div data-test-result className="flex flex-col gap-[14px] text-ui text-text-strong-950">
      {snap.ip && <Line icon={<GlobeIcon className="size-3.5" />}><span className="mono">{snap.ip}</span></Line>}
      {place && (
        <Line icon={snap.country_code ? <CountryFlag cc={snap.country_code} height={14} /> : <GlobeIcon className="size-3.5" />}>{place}</Line>
      )}
      {snap.timezone && <Line icon={<ClockIcon className="size-3.5" />}>{snap.timezone}</Line>}
      {snap.isp && <Line icon={<BuildingIcon className="size-3.5" />}>{snap.isp}</Line>}
      <div className="flex flex-wrap items-center gap-2">
        <Badge color={status === "ok" ? "success" : "error"} size="small">
          {snap.tcp_ms != null ? t("proxyPeek.tcpOk", { ms: snap.tcp_ms }) : t("proxyPeek.tcpFailed")}
        </Badge>
        {proxy.kind === "socks5" && (
          <Tooltip content={snap.udp_error ?? ""}>
            <Badge color={snap.udp_ms != null ? "success" : "error"} size="small">
              {snap.udp_ms != null ? t("proxyPeek.udpOk", { ms: snap.udp_ms }) : t("proxyPeek.udpFailed")}
            </Badge>
          </Tooltip>
        )}
        <span className="text-secondary text-text-soft-400">{fmtTs(snap.last_seen)}</span>
      </div>
      {snap.geo_error && (
        <p className="m-0 text-secondary text-error-text">
          {t("proxyInfoPopover.geoError")}
          <span className="block text-secondary text-text-soft-400" data-selectable>{explainError(snap.geo_error)}</span>
        </p>
      )}
    </div>
  );
}

/// One fact of the last test: its value (IP, place, time zone, ISP) is one
/// a person copies, so it is selectable (WIG-04).
function Line({ icon, children }: { icon: ReactNode; children: ReactNode }) {
  return (
    <div className="flex items-center gap-2.5">
      <span className="inline-grid w-3.5 place-items-center text-icon-soft-400">{icon}</span>
      <span data-selectable className="min-w-0">{children}</span>
    </div>
  );
}

function ProfilesUsing({ proxyId }: { proxyId: string }) {
  const t = useT();
  const profiles = useProxy((s) => s.profiles);
  const using = profiles.filter((p) => p.proxy_id === proxyId);
  if (using.length === 0) return <p className="m-0 text-secondary text-text-soft-400">{t("proxyPeek.noProfiles")}</p>;
  return (
    <ul className="m-0 flex list-none flex-col gap-1 p-0">
      {using.map((p) => (
        <li key={p.id}>
          <Button variant="neutral" mode="ghost" size="3xsmall" onClick={() => useProfileUi.getState().showInList(p.id)}>
            {p.name}
          </Button>
        </li>
      ))}
    </ul>
  );
}
