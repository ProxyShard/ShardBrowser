import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { DialogModal, IconButton, Input, useLatched } from "@proxyshard/shardx-ui-kit";
import { EyeIcon, EyeOffIcon } from "../../../shared/icons";
import { isMod } from "../../../shared/lib/keyboard";
import { commitFocusedField } from "../../../shared/lib/commitField";
import { useDialogLife } from "../../../shared/lib/useDialogLife";
import { toast } from "../../../shared/model/toast";
import { useUnsaved } from "../../../shared/model/unsaved";
import { Field } from "../../../shared/ui/Field";
import { NumField } from "../../../shared/ui/NumField";
import { SelectField } from "../../../shared/ui/SelectField";
import { useT } from "../../../shared/i18n";
import { proxyRotationGet, proxyRotationSet, useProxy, useRotation, type ProxyEntry } from "../../../entities/proxy";

const FIELDS = ["name", "kind", "host", "port", "username", "password", "country", "notes"] as const;

/// Edit proxy (manual save, owner 2026-10-07): one dialog changes a proxy,
/// and nothing is written until Save. Opened by the proxy panel's Edit;
/// `id` null is closed. The dialog's body is kept through its exit.
export function EditProxyDialog({ id, onClose }: { id: string | null; onClose: () => void }) {
  const life = useDialogLife(id != null);
  const shown = useLatched(id);
  if (!life.mounted || !shown) return null;
  // Keyed so each opening starts from the proxy as it is now.
  return <EditProxyBody key={life.session} id={shown} open={id != null} onClose={onClose} />;
}

function EditProxyBody({ id, open, onClose }: { id: string; open: boolean; onClose: () => void }) {
  const t = useT();
  const saveProxy = useProxy((s) => s.saveProxy);
  const stored = useProxy((s) => s.proxies.find((p) => p.id === id));
  // What it opened with: the draft is compared with it to know what changed.
  const [opened] = useState(() => useProxy.getState().proxies.find((p) => p.id === id));
  const [draft, setDraft] = useState(opened);
  const [status, setStatus] = useState<"idle" | "saving" | "error">("idle");
  // The host says why it is empty once it was left, or Save was pressed.
  const [blurred, setBlurred] = useState(false);
  const [tried, setTried] = useState(false);
  const [shownPassword, setShownPassword] = useState(false);
  // The port field shows text that is not a port (emptied, a letter): the
  // draft keeps the last good one, which is not what the field shows, so
  // Save waits until it is a port again.
  const [portBad, setPortBad] = useState(false);
  const nameRef = useRef<HTMLInputElement>(null);
  const busy = useRef(false);
  // #162: the change-IP link, kept on this computer apart from the proxy.
  const [openedLink, setOpenedLink] = useState<string | null>(null);
  const [link, setLink] = useState("");
  useEffect(() => {
    let alive = true;
    proxyRotationGet(id).then((l) => { if (alive) { setOpenedLink(l ?? ""); setLink(l ?? ""); } }, () => { if (alive) setOpenedLink(""); });
    return () => { alive = false; };
  }, [id]);
  const linkChanged = openedLink != null && link.trim() !== openedLink;
  const linkOk = link.trim() === "" || /^https?:\/\/\S+$/i.test(link.trim());

  const changed = opened && draft ? FIELDS.filter((k) => draft[k] !== opened[k]) : [];
  const dirty = changed.length > 0 || linkChanged;

  // Gone before it opened, or while it is open (deleted elsewhere): saving
  // would bring it back, so the dialog closes and says why. "Could not be
  // saved" only when there was something to save.
  const gone = !stored;
  useEffect(() => {
    if (!open || !gone || busy.current) return;
    const who = { name: opened ? opened.name || `${opened.host}:${opened.port}` : "" };
    toast.err(dirty ? t("editProxy.gone", who) : t("editProxy.goneClean", who));
    onClose();
  }, [open, gone, dirty, opened, onClose, t]);

  const hostOk = !!draft?.host.trim();
  const saving = status === "saving";
  const canSave = !!draft && hostOk && !portBad && linkOk && dirty && !saving;
  const name = opened ? opened.name || `${opened.host}:${opened.port}` : "";

  const patch = (changes: Partial<ProxyEntry>) => {
    setDraft((d) => d && { ...d, ...changes });
    if (status === "error") setStatus("idle");
  };

  /** Saves what was changed over the proxy as it is now, so a change made
   *  elsewhere meanwhile (a test filling in the country) survives. True when written. */
  const save = async (): Promise<boolean> => {
    if (!opened || !draft || busy.current) return false;
    if (!hostOk || portBad || !linkOk) { setTried(true); return false; }
    if (!dirty) return true;
    const base = useProxy.getState().proxies.find((p) => p.id === id);
    if (!base) return false;
    busy.current = true;
    setStatus("saving");
    const merged = { ...base, ...Object.fromEntries(changed.map((k) => [k, draft[k]])) } as ProxyEntry;
    // A pasted value often carries a stray space, and the proxy then fails
    // for no visible reason (WIG-03). The password stays as typed.
    const entry = { ...merged, host: merged.host.trim(), username: merged.username.trim(), country: merged.country.trim().toUpperCase() };
    let ok = changed.length === 0 || await saveProxy(entry);
    if (ok && linkChanged) {
      ok = await proxyRotationSet(id, link.trim() || null).then(() => true, () => false);
      void useRotation.getState().load();
    }
    busy.current = false;
    if (!ok) { setStatus("error"); return false; }
    toast.saved(t("editProxy.saved", { name: entry.name || `${entry.host}:${entry.port}` }));
    onClose();
    return true;
  };

  // Every exit the dialog cannot see (a page change, quitting) asks first.
  useUnsaved(open && dirty ? { id: "edit-proxy", label: name, save, discard: onClose } : null);

  // Cmd/Ctrl+S saves: an app command cannot run while a dialog has the
  // keyboard, so the dialog listens for it itself.
  const latest = useRef({ canSave, save });
  useLayoutEffect(() => { latest.current = { canSave, save }; });
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (!isMod(e) || e.altKey || e.shiftKey || e.key.toLowerCase() !== "s") return;
      // Never the browser's own Save page; with nothing to save, nothing happens.
      e.preventDefault();
      // The field that has the keyboard commits first; `latest` is the dialog as it draws after that.
      commitFocusedField();
      if (!e.repeat && latest.current.canSave) void latest.current.save();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open]);

  if (!draft) return null;
  const note = status === "error" ? (
    <span role="alert" className="text-error-text">{t("editProxy.saveFailed")}</span>
  ) : saving ? null
    : !hostOk ? t("editProxy.hostRequired")
    : portBad ? t("numField.invalidRange", { min: 1, max: 65535 })
    : !dirty ? t("editProxy.noChanges")
    : null;

  return (
    <DialogModal
      open={open}
      onOpenChange={(o) => { if (!o) onClose(); }}
      title={t("editProxy.title", { name })}
      dirty={dirty}
      initialFocusRef={nameRef}
      onConfirm={() => { commitFocusedField(); void latest.current.save(); }}
      confirmLabel={t("editProxy.save")}
      isLoading={saving}
      isDisabled={!canSave}
      footerNote={note}
    >
      <fieldset disabled={saving} className="m-0 min-w-0 border-0 p-0">
        <div className="flex flex-col gap-4 py-4">
          <Input
            ref={nameRef}
            label={t("proxyEditor.nameLabel")}
            inputSize="xsmall"
            wrapperClassName="gap-[6px]"
            placeholder={`${draft.host}:${draft.port}`}
            value={draft.name}
            onChange={(e) => patch({ name: e.target.value })}
          />
          <div className="grid grid-cols-2 gap-stack-md">
            <SelectField
              label={t("proxyEditor.typeLabel")}
              value={draft.kind}
              onChange={(kind) => patch({ kind })}
              options={[
                { value: "socks5" as const, label: "SOCKS5" },
                { value: "http" as const, label: "HTTP" },
                { value: "https" as const, label: "HTTPS" },
              ]}
            />
            <Field label={t("proxyEditor.countryLabel")} value={draft.country} onChange={(country) => patch({ country })} />
          </div>
          <div className="grid grid-cols-[1fr_96px] gap-stack-md">
            <Input
              label={t("proxyEditor.hostLabel")}
              inputSize="xsmall"
              wrapperClassName="gap-[6px]"
              className="mono"
              required
              aria-invalid={!hostOk || undefined}
              value={draft.host}
              error={!hostOk && (blurred || tried) ? t("editProxy.hostRequired") : undefined}
              onChange={(e) => patch({ host: e.target.value })}
              onBlur={() => setBlurred(true)}
            />
            <NumField label={t("proxyEditor.portLabel")} value={draft.port} min={1} max={65535} onChange={(port) => patch({ port })} onInvalid={setPortBad} />
          </div>
          <div className="grid grid-cols-2 gap-stack-md">
            <Field label={t("proxyEditor.usernameLabel")} value={draft.username} onChange={(username) => patch({ username })} />
            {/* The eye sits inside the field, so the field keeps the column's
                trailing edge (LAYOUT-15). One name, "Show password"; whether it
                is shown is `aria-pressed` (PC-073). One ring, never cut off
                (Pair 18 review, I2): while the eye has focus the field's own
                ring stands down, and the eye draws the 2 px ring inside its box,
                within the field's frame. */}
            <Input
              label={t("proxyEditor.passwordLabel")}
              inputSize="xsmall"
              wrapperClassName="gap-[6px]"
              className="[&:has(button:focus)]:outline-hidden"
              secret
              type={shownPassword ? "text" : "password"}
              value={draft.password}
              onChange={(e) => patch({ password: e.target.value })}
              rightIcon={
                <IconButton
                  size="3xsmall"
                  className="focus-ring-inset"
                  label={t("proxyPeek.showPassword")}
                  aria-pressed={shownPassword}
                  icon={shownPassword ? <EyeOffIcon className="size-3.5" /> : <EyeIcon className="size-3.5" />}
                  onClick={() => setShownPassword((v) => !v)}
                />
              }
            />
          </div>
          <Field label={t("proxyPeek.notes")} value={draft.notes} onChange={(notes) => patch({ notes })} />
          {/* #162: kept on this computer only, like a password; never sent to a team. */}
          <Field
            label={t("editProxy.changeIpLink")}
            value={link}
            mono
            secret
            placeholder="https://"
            error={!linkOk ? t("editProxy.changeIpLinkBad") : undefined}
            onChange={(v) => { setLink(v); if (status === "error") setStatus("idle"); }}
          />
          <p className="m-0 -mt-2 text-secondary text-text-soft-400">{t("editProxy.changeIpLinkHelp")}</p>
        </div>
      </fieldset>
    </DialogModal>
  );
}
