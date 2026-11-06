import { useEffect, useState } from "react";
import { Badge, Checkbox, DialogModal, SegmentControl } from "@proxyshard/shardx-ui-kit";
import { SelectField } from "../../../shared/ui/SelectField";
import { Field } from "../../../shared/ui/Field";
import { toast } from "../../../shared/model/toast";
import { useT } from "../../../shared/i18n";
import type { PsOrder, PsActiveProxy } from "../../../entities/proxyshard";
import { PS_SIGNATURES, psActive, psOrder, psSignatureSet } from "../../../entities/proxyshard";
import { proxyBulkSave, useProxy } from "../../../entities/proxy";

/// Active-proxy picker: fetch an order's proxies, choose SOCKS5/HTTP and which
/// IPs to import into the local proxy list (via proxy_bulk_save, which dedups).
export function PsImportModal({ open, order, onClose }: { open: boolean; order: PsOrder; onClose: () => void }) {
  const t = useT();
  const [items, setItems] = useState<PsActiveProxy[] | null>(null);
  const [err, setErr] = useState("");
  const [kind, setKind] = useState<"socks5" | "http">("socks5");
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [saving, setSaving] = useState(false);
  const [tag, setTag] = useState("");
  // The prefix the order came with: only a change of it is unsaved input.
  const [loadedTag, setLoadedTag] = useState("");
  // Per-IP p0f signature ("" = leave unchanged).
  const [sigByIp, setSigByIp] = useState<Record<string, string>>({});
  // p0f slot accounting from the order detail (available vs already used).
  const [slots, setSlots] = useState<{ avail: number; used: number } | null>(null);

  useEffect(() => {
    psActive(order.order_id)
      .then((r) => {
        const data: PsActiveProxy[] = r.data ?? [];
        setItems(data);
        setSel(new Set(data.map((d) => d.ip))); // select all by default
        // Prefill each row with its currently-set signature.
        setSigByIp(Object.fromEntries(data.map((d) => [d.ip, d.signature ?? ""])));
        const prefix = (r.order_tag && r.order_tag !== "none" ? r.order_tag : "") || t("psImportModal.orderName", { id: order.order_id });
        setTag(prefix);
        setLoadedTag(prefix);
      })
      .catch((e) => setErr(String(e)));
    psOrder(order.order_id)
      .then((r) => {
        const o = r.order ?? {};
        setSlots({ avail: o.p0f_slots_available ?? 0, used: o.p0f_slots_used ?? 0 });
      })
      .catch(() => { });
  }, [order.order_id]);

  const toggle = (ip: string) =>
    setSel((s) => { const n = new Set(s); n.has(ip) ? n.delete(ip) : n.add(ip); return n; });

  const allChecked = !!items && items.length > 0 && items.every((d) => sel.has(d.ip));
  const toggleAll = () =>
    setSel(allChecked ? new Set() : new Set((items ?? []).map((d) => d.ip)));

  // p0f can be assigned only while free slots remain.
  const canSetP0f = !!slots && slots.avail > slots.used;
  const free = slots ? Math.max(0, slots.avail - slots.used) : 0;
  const setSig = (ip: string, v: string) => setSigByIp((m) => ({ ...m, [ip]: v }));

  const save = async () => {
    if (!items) return;
    const chosen = items.filter((d) => sel.has(d.ip));
    if (chosen.length === 0) { toast.err(t("psImportModal.selectAtLeastOne")); return; }
    const label = tag.trim() || t("psImportModal.orderName", { id: order.order_id });
    const entries = chosen
      .map((d) => {
        const port = kind === "http" ? d.http_port : d.socks_port;
        if (!d.ip || !port) return null;
        return {
          id: "",
          name: `${label} (${d.ip})`,
          kind,
          host: d.ip,
          port,
          username: d.username ?? "",
          password: d.password ?? "",
          country: "",
          notes: t("psImportModal.orderNote", { id: order.order_id }),
        };
      })
      .filter(Boolean);
    setSaving(true);
    try {
      const n = await proxyBulkSave(entries);
      // The list reads again, as after any import: else they showed only after a restart.
      await useProxy.getState().reload();
      toast.ok(n > 0
        ? (n === 1 ? t("psImportModal.addedOne", { n }) : t("psImportModal.addedMany", { n }))
        : t("psImportModal.noNewProxies"));
      // Apply only the selected proxies whose signature actually changed
      // (a non-empty value differing from the one already set).
      const sigItems = chosen
        .filter((d) => { const v = sigByIp[d.ip] ?? ""; return v !== "" && v !== (d.signature ?? ""); })
        .map((d) => ({ ip: d.ip, signature: sigByIp[d.ip] }));
      if (sigItems.length > 0) {
        try {
          await psSignatureSet(order.order_id, sigItems);
          toast.ok(sigItems.length === 1
            ? t("psImportModal.p0fSetOne", { n: sigItems.length })
            : t("psImportModal.p0fSetMany", { n: sigItems.length }));
        } catch (e) { toast.fail({ title: t("psImportModal.signatureError"), detail: e }); }
      }
      onClose();
    } catch (e) { toast.fail({ title: t("psImportModal.failed", { id: order.order_id }), detail: e }); }
    finally { setSaving(false); }
  };

  return (
    <DialogModal
      open={open}
      onClose={onClose}
      title={t("psImportModal.title", { product: order.product_name, id: order.order_id })}
      dirty={tag.trim() !== loadedTag.trim()}
      size="lg"
      confirmLabel={saving ? t("psImportModal.adding") : t("psImportModal.addCount", { n: sel.size })}
      onConfirm={save}
      isLoading={saving}
      isDisabled={saving || !items || sel.size === 0}
      cancelLabel={t("psImportModal.cancel")}
      onCancel={onClose}
    >
      <div className="flex flex-col gap-3 py-4">
        <div className="mb-2.5 flex items-end gap-3">
          <div className="flex-1">
            <Field label={t("psImportModal.namePrefixLabel")} value={tag} onChange={setTag} />
          </div>
          <div>
            <SegmentControl
              // Named, not a bare "radio group" (Pair 28 review M7).
              aria-label={t("psImportModal.kindLabel")}
              size="xsmall"
              value={kind}
              items={[
                { value: "socks5", label: "SOCKS5" },
                { value: "http", label: "HTTP" },
              ]}
              onChange={(v) => setKind(v as "socks5" | "http")}
            />
          </div>
        </div>
        {slots && (
          <p className="m-0 mb-1.5 text-secondary text-text-soft-400">
            {t("psImportModal.p0fSlots", { used: slots.used, avail: slots.avail })}
            {canSetP0f ? t("psImportModal.p0fSlotsFree", { n: free }) : t("psImportModal.p0fSlotsNone")}
          </p>
        )}
        {!items && !err && <p className="m-0 text-secondary text-text-soft-400">{t("psImportModal.loading")}</p>}
        {err && <p className="m-0 text-secondary text-text-soft-400">{err}</p>}
        {items && items.length === 0 && <p className="m-0 text-secondary text-text-soft-400">{t("psImportModal.noActiveProxies")}</p>}
        {items && items.length > 0 && (
          <div className="mt-1.5 max-h-[320px] overflow-hidden overflow-y-auto tile">
            <div className="grid items-center gap-2.5 px-cell py-2" style={{ gridTemplateColumns: "20px 1fr 132px" }}>
              <Checkbox checked={allChecked} onChange={toggleAll} aria-label={t("psImportModal.selectAll")} />
              <span className="text-secondary text-text-soft-400">{t("psImportModal.selectedCount", { n: sel.size, total: items.length })}</span>
              <span className="text-right text-secondary text-text-soft-400">{canSetP0f ? "p0f" : ""}</span>
            </div>
            {items.map((d) => {
              const port = kind === "http" ? d.http_port : d.socks_port;
              return (
                <div
                  key={d.ip}
                  // A click anywhere on the row toggles its box; the box's own
                  // label click is left to the label (one toggle per click).
                  onClick={(e) => { if (!(e.target as HTMLElement).closest("label,input,button,select,[role=combobox],[role=listbox],[role=option]")) toggle(d.ip); }}
                  className="row-line row-line-hover relative grid cursor-pointer items-center gap-2.5 px-cell py-[9px] hover:bg-bg-hover"
                  style={{ gridTemplateColumns: "20px minmax(120px,1.3fr) 1fr auto 132px" }}
                >
                  <Checkbox checked={sel.has(d.ip)} onChange={() => toggle(d.ip)} aria-label={t("proxyBulkImporter.selectRow", { host: `${d.ip}:${port}` })} />
                  <span className="text-mono text-text-strong-950 transition-hover hover:text-primary-text">{d.ip}:{port}</span>
                  <span className="text-secondary text-text-soft-400">{d.username}</span>
                  <Badge color={d.status === "active" ? "success" : "gray"} size="small" dot>
                    {d.status}
                  </Badge>
                  {(canSetP0f || d.signature) ? (
                    // Editable when free slots exist, or this IP is already
                    // signed (re-assigning an OS doesn't consume a slot).
                    <div onClick={(e) => e.stopPropagation()}>
                      <SelectField aria-label={t("psImportModal.p0fFor", { ip: d.ip })} value={sigByIp[d.ip] ?? ""} onChange={(v) => setSig(d.ip, v)} options={PS_SIGNATURES} placeholder="p0f" />
                    </div>
                  ) : (
                    <span className="text-right text-secondary text-text-soft-400">—</span>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </DialogModal>
  );
}
