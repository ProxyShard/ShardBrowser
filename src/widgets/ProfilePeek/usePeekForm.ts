import { useCallback, useEffect, useState } from "react";
import {
  editsSettled, isPlaceholder, isProtected, onFormSaved, peekForm, resolveId, selectVisible, useProfile,
  type ProfileForm,
} from "../../entities/profile";
import { whenIdle } from "../../shared/lib/idle";

/// The form Peek reads for the parts of a profile its list row does not
/// carry (the fingerprint). Peek only shows it: nothing here writes. A
/// profile Peek has seen shows at once from the form cache, and the
/// backend's copy follows (PERF-14). A save made anywhere (the profile
/// form, a card's fix) reads it again, so Peek never keeps the old values.
/// A protected profile's is never read: it opens only with its password (#127).
export function usePeekForm(given: string | null) {
  const loadForm = useProfile((s) => s.loadForm);
  const locked = useProfile((s) => !!given && isProtected(s.profiles.find((p) => p.id === given)));
  const id = locked ? null : given;
  // Keyed by the profile asked for: a read that lands after Peek moved on
  // never shows on the next one.
  const [read, setRead] = useState<{ id: string; form: ProfileForm } | null>(null);
  const [failed, setFailed] = useState<{ id: string; error: string } | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [saves, setSaves] = useState(0);

  useEffect(() => {
    if (!id || isPlaceholder(id)) return;
    return onFormSaved((form) => { if (form.id === id) setSaves((n) => n + 1); });
  }, [id]);

  // A row still being created, or coming back from the Trash, is read once
  // it has its real id; an edit the list still has on its way lands first.
  useEffect(() => {
    if (!id || isPlaceholder(id)) return;
    let alive = true;
    resolveId(id)
      .then((real) => (real ? editsSettled(real).then(() => loadForm(real)) : null))
      .then(
        (form) => { if (alive && form) { setRead({ id, form }); setFailed(null); } },
        (e) => { if (alive) setFailed({ id, error: String(e) }); },
      );
    return () => { alive = false; };
  }, [id, loadForm, attempt, saves]);

  // The rows above and below are where the arrows go next: their forms load
  // while the browser is idle, so the next move shows no skeleton (PERF-14).
  useEffect(() => {
    if (!id || isPlaceholder(id)) return;
    return whenIdle(() => {
      const shown = selectVisible();
      const at = shown.indexById.get(id);
      if (at === undefined) return;
      useProfile.getState().prefetchForms([shown.ids[at - 1], shown.ids[at + 1]].filter((x): x is string => !!x));
    });
  }, [id]);

  /** Reads the form again after a failure. */
  const retry = useCallback(() => { setFailed(null); setAttempt((n) => n + 1); }, []);

  // The first render on another profile already has its cached form, so no
  // frame shows the fingerprint without one.
  const form = id && !isPlaceholder(id) ? (read?.id === id ? read.form : peekForm(id) ?? null) : null;
  const loadError = failed?.id === id && !form ? failed.error : null;
  return { form, loadError, retry };
}
