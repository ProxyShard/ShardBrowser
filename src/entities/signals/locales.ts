import { create } from "zustand";
import { onFormSaved, useProfile } from "../profile/lib/useProfile";
import { isPlaceholder } from "../profile/lib/optimistic";
import { profileGet } from "../profile/model/api";
import { fromStored } from "../profile/model/form";
import { limiter } from "../../shared/lib/limiter";
import type { ProfileLocale } from "./signals";

/// Each judged profile's timezone and language, for the Location leak card.
/// The list's rows do not carry them (backend: profile_list could, then this
/// read goes), so they are read from the stored profile: only for profiles
/// whose proxy passed a test, each once per session, and kept current by
/// every save. At most four reads are in flight across the whole app: every
/// call shares one queue, so calls that overlap (each proxy test that comes
/// in starts one) wait their turn rather than adding four more.

export const useProfileLocales = create<{ byId: Record<string, ProfileLocale> }>(() => ({ byId: {} }));

/** The one queue every `profile_get` read here goes through. */
const read = limiter(4);
/** Read, or being read, this session. */
const asked = new Set<string>();

const localeOf = (form: { timezone: string; language: string }): ProfileLocale => ({ timezone: form.timezone, language: form.language });

let listening = false;
function listen() {
  if (listening) return;
  listening = true;
  onFormSaved((form) => {
    asked.add(form.id);
    useProfileLocales.setState((s) => ({ byId: { ...s.byId, [form.id]: localeOf(form) } }));
  });
}

/** Reads the ones not read yet; resolves when they are in (a failed read is
 *  tried again on the next call). The answers land in one write. */
export async function readLocales(ids: string[]): Promise<void> {
  listen();
  // A protected one opens only with its password: never read here.
  const locked = new Set(useProfile.getState().profiles.filter((p) => p.protected).map((p) => p.id));
  const todo = ids.filter((id) => !asked.has(id) && !isPlaceholder(id) && !locked.has(id));
  if (todo.length === 0) return;
  for (const id of todo) asked.add(id);
  const got: Record<string, ProfileLocale> = {};
  await Promise.all(todo.map(async (id) => {
    try { got[id] = localeOf(fromStored(await read(() => profileGet(id)))); } catch { asked.delete(id); }
  }));
  if (Object.keys(got).length > 0) useProfileLocales.setState((s) => ({ byId: { ...s.byId, ...got } }));
}
