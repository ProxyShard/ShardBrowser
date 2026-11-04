import { useRef, useState } from "react";
import { DialogModal, Input, useLatched } from "@proxyshard/shardx-ui-kit";
import { KeyIcon } from "../../shared/icons";
import { useT } from "../../shared/i18n";
import { explainError } from "../../shared/lib/errors";
import { usePasswordAsk, type PasswordReq } from "../../shared/model/passwordAsk";
import { isTooShort, isWrongPassword, PASSWORD_MIN, passwordProblem } from "../../entities/profile";

/// The profile password dialog (#127), fed by `askPassword`. A wrong password,
/// or one too short, is said under its field and the dialog stays for another
/// try; any other failure is said under the buttons.
export function PasswordHost() {
  const req = usePasswordAsk((s) => s.req);
  // The dialog keeps its words while it leaves.
  const shown = useLatched(req);
  if (!shown) return null;
  // A new ask starts with empty fields.
  return <PasswordDialog key={shown.id} req={shown} open={req === shown} />;
}

function PasswordDialog({ req, open }: { req: PasswordReq; open: boolean }) {
  const t = useT();
  const [password, setPassword] = useState("");
  const [next, setNext] = useState("");
  const [repeat, setRepeat] = useState("");
  const [busy, setBusy] = useState(false);
  const [wrong, setWrong] = useState<string | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const first = useRef<HTMLInputElement>(null);
  const asksCurrent = req.kind !== "set";
  const setsNew = req.kind === "set" || req.kind === "change";
  const problem = setsNew ? passwordProblem(next, repeat) ?? (next === "" ? "short" : null) : null;
  const missing = asksCurrent && password === "";
  const problemWords = problem === "short" ? t("errors.passwordTooShort") : problem === "mismatch" ? t("profileLock.mismatch") : null;

  // Once per ask: an attempt that lands after Cancel changes nothing (passwordAsk).
  const close = (answer: { value: unknown } | null) => req.done(answer);
  const cancel = () => {
    // A start on its way ends with the dialog.
    if (busy) req.onBusyCancel?.();
    close(null);
  };
  const submit = async () => {
    if (busy || missing || problem) return;
    setBusy(true);
    setWrong(null);
    setFailed(null);
    try {
      const value = await req.attempt({ password, newPassword: next });
      close({ value });
    } catch (e) {
      setBusy(false);
      if (isWrongPassword(e)) {
        setWrong(t("errors.wrongPassword"));
        // Typed again from the start.
        requestAnimationFrame(() => first.current?.select());
      } else if (isTooShort(e)) setFailed(t("errors.passwordTooShort"));
      else setFailed(explainError(e) ?? t("profileLock.failed"));
    }
  };

  // Why the confirm waits, in words (never a grey button alone), or why the last try failed.
  const note = failed ? <span role="alert" className="text-error-text">{failed}</span>
    : missing ? t("profileLock.needPassword")
    : problemWords;
  const field = { inputSize: "xsmall", secret: true, type: "password", leftIcon: <KeyIcon className="size-3.5" />, disabled: busy } as const;
  return (
    <DialogModal
      open={open}
      onOpenChange={(o) => { if (!o) cancel(); }}
      onCancel={cancel}
      size="sm"
      title={req.title}
      subtitle={req.message}
      initialFocusRef={first}
      confirmLabel={req.confirmLabel}
      confirmVariant={req.kind === "remove" ? "error" : "primary"}
      onConfirm={() => void submit()}
      isLoading={busy}
      isDisabled={missing || !!problem}
      footerNote={note ?? undefined}
    >
      <div data-password-dialog={req.kind} className="flex flex-col gap-3 py-4">
        {asksCurrent && (
          <Input
            {...field}
            ref={first}
            label={req.kind === "ask" ? t("profileLock.passwordLabel") : t("profileLock.currentLabel")}
            value={password}
            error={wrong ?? undefined}
            aria-invalid={!!wrong || undefined}
            onChange={(e) => { setPassword(e.target.value); setWrong(null); setFailed(null); }}
          />
        )}
        {setsNew && (
          <>
            <Input
              {...field}
              ref={asksCurrent ? undefined : first}
              label={t("profileLock.newLabel")}
              hint={t("profileLock.minHint", { n: PASSWORD_MIN })}
              value={next}
              aria-invalid={(next !== "" && problem === "short") || undefined}
              onChange={(e) => { setNext(e.target.value); setFailed(null); }}
            />
            <Input
              {...field}
              label={t("profileLock.repeatLabel")}
              value={repeat}
              aria-invalid={(repeat !== "" && problem === "mismatch") || undefined}
              onChange={(e) => { setRepeat(e.target.value); setFailed(null); }}
            />
            <p data-no-recovery className="m-0 text-help text-warning-text">{t("profileLock.noRecovery")}</p>
          </>
        )}
      </div>
    </DialogModal>
  );
}
