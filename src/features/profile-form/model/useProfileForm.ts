import { create } from "zustand";
import { t } from "../../../shared/i18n";
import { toast } from "../../../shared/model/toast";
import { storeBus } from "../../../shared/lib/storeBus";
import { onSpaceMoved } from "../../../shared/api/invoke";
import { explainError } from "../../../shared/lib/errors";
import { HOST_OS } from "../../../shared/lib/utils";
import { hostScreen, type HostScreen } from "../../../shared/lib/hostScreen";
import { AUTO_LANG, AUTO_TZ, matchesOs } from "../../../shared/constants";
import { useGpuCompat, verdictsLoaded } from "../../../shared/model/gpuCompat";
import { inFolder, teamRefusal } from "../../../entities/space";
import {
  cookiesImport, editsSettled, forgetForm, isProtected, passwordProblem, profileProtect, profileRefusal, enrichPicksForPreset, fromStored, resolveId,
  selectVisible, useProfile, useProfileUi, type ProfileForm,
} from "../../../entities/profile";
import { askPassword } from "../../../shared/model/passwordAsk";
import type { FingerprintEntry } from "../../../entities/fingerprint";
import { proxyBulkParse, proxySave } from "../../../entities/proxy";
import { clean, useTags } from "../../../entities/tag";

/// The profile form (manual save, owner 2026-10-07): one dialog creates a
/// profile and edits one. Nothing is written until Create profile or Save;
/// the draft is the form's own copy, compared with what it opened with to
/// know what changed.

export type ProfileFormTab = "basics" | "proxy" | "fingerprint" | "extensions";
export const PROFILE_FORM_TABS: readonly ProfileFormTab[] = ["basics", "proxy", "fingerprint", "extensions"];

/** What the form edits: the profile's own fields, and the parts that live
 *  elsewhere (its Folder, its tags, a proxy to add from a pasted line).
 *  `newTags` are tag names no tag has yet: made on Save, like a new Folder. */
export type ProfileFormDraft = {
  form: ProfileForm; folder: string; tagIds: string[]; newTags: string[]; newProxyLine: string;
  /** A cookies file picked in the form, imported once the profile is saved. */
  cookies: { name: string; items: unknown[] } | null;
};

export type ProfileFormStatus = "idle" | "saving" | "error" | "conflict";

type ProfileFormState = {
  open: boolean;
  mode: "edit" | "create";
  /** The profile edited; null while creating. */
  id: string | null;
  tab: ProfileFormTab;
  /** Null while it loads (or when it could not be read: `error`). */
  draft: ProfileFormDraft | null;
  /** What the form opened with: the draft differs from it when something changed. */
  saved: ProfileFormDraft | null;
  status: ProfileFormStatus;
  /** Why the last save, or the read, failed. */
  error: string | null;
  /** The backend's words for it (or a sentence for a failure it knows), under the line. */
  errorDetail: string | null;
  /** Each open is a session: a read or a save that lands after the form
   *  closed, or opened again, changes nothing. */
  session: number;
  /** After a conflict, the profile as it is on disk now: Save then writes
   *  this form's version over it, Reload takes it. */
  theirs: ProfileForm | null;
  /** Save was pressed: every invalid field says why. */
  tried: boolean;
  /** Number fields whose text is not a number now (a letter, an emptied
   *  field): the draft keeps their last good value, which is not what they
   *  show, so Save waits until none is left. */
  badFields: string[];
  /** This computer's screen, asked as a new profile's form opens; null when unknown. */
  host: HostScreen | null;
  /** The Screen field was set by hand in this form, "As in the fingerprint" too. */
  screenPicked: boolean;
  /** A new profile's password and its repeat (#127): set once everything else
   *  landed. Kept only while the form is open, never stored. */
  password: string;
  passwordRepeat: string;
  /** The password a protected profile was opened with: its saves carry it. Dropped on close. */
  unlocked: string | null;
};

export const useProfileForm = create<ProfileFormState>(() => ({
  open: false, mode: "edit", id: null, tab: "basics", draft: null, saved: null,
  status: "idle", error: null, errorDetail: null, session: 0, theirs: null, tried: false, badFields: [], host: null, screenPicked: false,
  password: "", passwordRepeat: "", unlocked: null,
}));

const get = () => useProfileForm.getState();
const set = (s: Partial<ProfileFormState>) => useProfileForm.setState(s);
const copy = (d: ProfileFormDraft): ProfileFormDraft => structuredClone(d);
/** A draft as compared: the order tags were given in does not count, and a
 *  picked cookies file counts by its name and size, not its (large) content. */
const key = (d: ProfileFormDraft) => JSON.stringify({
  ...d, tagIds: [...d.tagIds].sort(), newTags: [...d.newTags].sort(),
  cookies: d.cookies ? [d.cookies.name, d.cookies.items.length] : null,
});
const sameTags = (a: ProfileFormDraft, b: ProfileFormDraft) =>
  JSON.stringify([[...a.tagIds].sort(), [...a.newTags].sort()]) === JSON.stringify([[...b.tagIds].sort(), [...b.newTags].sort()]);

const BASICS = new Set<keyof ProfileForm>(["name", "color", "notes"]);
const PROXY = new Set<keyof ProfileForm>(["proxy_id"]);
const EXTENSIONS = new Set<keyof ProfileForm>(["extensions"]);
/** Not the person's to change: never counted as a change. */
const OWN = new Set<keyof ProfileForm>(["id", "rev", "cookies_file"]);
const tabOf = (k: keyof ProfileForm): ProfileFormTab =>
  BASICS.has(k) ? "basics" : PROXY.has(k) ? "proxy" : EXTENSIONS.has(k) ? "extensions" : "fingerprint";

export function isDirty(s: Pick<ProfileFormState, "draft" | "saved"> & Partial<Pick<ProfileFormState, "password" | "passwordRepeat">>): boolean {
  if (!s.draft || !s.saved) return false;
  // A password typed is a change: it is set on Save, and Cancel drops it.
  return key(s.draft) !== key(s.saved) || !!s.password || !!s.passwordRepeat;
}

/** The tabs holding unsaved changes. */
export function changedTabs(s: Pick<ProfileFormState, "draft" | "saved"> & Partial<Pick<ProfileFormState, "password" | "passwordRepeat">>): Set<ProfileFormTab> {
  const out = new Set<ProfileFormTab>();
  const { draft: d, saved: o } = s;
  if (!d || !o) return out;
  if (d.folder !== o.folder || !sameTags(d, o) || (d.cookies?.name ?? null) !== (o.cookies?.name ?? null) || s.password || s.passwordRepeat) out.add("basics");
  if (d.newProxyLine !== o.newProxyLine) out.add("proxy");
  for (const k of Object.keys(d.form) as (keyof ProfileForm)[]) {
    if (OWN.has(k) || JSON.stringify(d.form[k]) === JSON.stringify(o.form[k])) continue;
    out.add(tabOf(k));
  }
  return out;
}

/** Why the form cannot be saved as it is, per field; empty when it can. */
export function problemsOf(d: ProfileFormDraft | null): { name?: string } {
  if (!d) return {};
  return d.form.name.trim() ? {} : { name: t("profileForm.nameRequired") };
}

/** Why the new password is not taken yet, in words; null when there is none or it is fine. */
export function passwordProblemOf(s: Pick<ProfileFormState, "password" | "passwordRepeat"> = get()): string | null {
  const problem = passwordProblem(s.password, s.passwordRepeat);
  return problem === "short" ? t("errors.passwordTooShort") : problem === "mismatch" ? t("profileLock.mismatch") : null;
}

/** The lowest "Profile N" nobody uses yet. */
function nextName(): string {
  const taken = new Set(useProfile.getState().profiles.map((p) => p.name));
  let n = 1;
  while (taken.has(t("useProfile.instantName", { n }))) n++;
  return t("useProfile.instantName", { n });
}

/** A new profile's fingerprint: one of this machine's OS that its GPU can back, at random. */
function pickFingerprint(fingerprints: FingerprintEntry[]): FingerprintEntry | undefined {
  const compat = useGpuCompat.getState().byId;
  const hostOs = fingerprints.filter((f) => matchesOs(f.platform, HOST_OS));
  const base = hostOs.length > 0 ? hostOs : fingerprints;
  const fitting = base.filter((f) => compat[f.id]?.compatible !== false);
  const pool = fitting.length > 0 ? fitting : base;
  return pool[Math.floor(Math.random() * pool.length)];
}

const desktopOs = (platform: string) => matchesOs(platform, "Windows") || matchesOs(platform, "Linux");

/** As launch.rs `claim_host_screen` decides: a desktop, and not a phone whose platform reads "Linux". */
function takesHostScreen(fp: FingerprintEntry): boolean {
  const p = fp.payload ?? {};
  const phone = p.client_hints?.mobile === true || /^android/i.test(p.client_hints?.platform ?? "")
    || /android/i.test(p.navigator?.user_agent ?? "");
  return desktopOs(fp.platform) && !phone;
}

/** A new profile's screen (owner 2026-10-23): Windows/Linux claim this
 *  computer's, macOS and phones (or an unknown host) the template's own. */
function screenFor(fp: FingerprintEntry, host: HostScreen | null): Partial<ProfileForm> {
  return takesHostScreen(fp) && host
    ? { screen_w: host[0], screen_h: host[1], screen_custom: false, screen_host: true, screen_dpr: host[2] ?? 0 }
    : { screen_w: 0, screen_h: 0, screen_custom: false, screen_host: false, screen_dpr: 0 };
}

const draftOf = (form: ProfileForm): ProfileFormDraft => ({
  form,
  folder: useProfile.getState().profiles.find((p) => p.id === form.id)?.folder ?? "",
  tagIds: [...(useTags.getState().byProfile[form.id] ?? [])],
  newTags: [],
  newProxyLine: "",
  cookies: null,
});

/** The form's opened state, as both draft and what it compares with. */
const show = (draft: ProfileFormDraft) => set({ draft, saved: copy(draft), status: "idle", error: null, errorDetail: null, theirs: null, tried: false, badFields: [] });

async function loadEdit(session: number, id: string) {
  try {
    // A row still being created, or coming back from the Trash, is read
    // once it has its real id; an edit the list still has on its way lands first.
    const real = await resolveId(id);
    if (!real) throw t("profileForm.gone");
    await editsSettled(real);
    const form = await useProfile.getState().loadForm(real);
    if (get().session !== session) return;
    set({ id: real });
    show(draftOf(form));
  } catch (e) {
    if (get().session === session) set({ status: "error", error: String(e), errorDetail: null });
  }
}

async function loadCreate(session: number, proxyId: string | null, preset?: Partial<ProfileForm>) {
  const { fingerprints } = useProfile.getState();
  if (fingerprints.length === 0) { set({ status: "error", error: t("useProfile.noFingerprints") }); return; }
  // A fingerprint the caller chose (the library's "New profile"), else one at random.
  const chosen = preset?.gpu_preset_id ? useProfile.getState().fingerprints.find((f) => f.id === preset.gpu_preset_id) : undefined;
  if (!chosen) {
    // The random pick waits up to a second for this machine's GPU verdicts, then
    // goes on without them: the check can hang, the form must not (badges come later).
    await verdictsLoaded();
    if (get().session !== session) return;
  }
  const fp = chosen ?? pickFingerprint(useProfile.getState().fingerprints);
  if (!fp) { set({ status: "error", error: t("useProfile.noFingerprints") }); return; }
  // Every field as the template has it, as the backend's create from a
  // template makes it: timezone, language and location follow the proxy
  // at launch (lib.rs `merge_library_fingerprint`).
  // Cores, memory and platform version drawn for this computer from the same
  // tables the backend uses (lib.rs `enrich_picks_for_preset`), as a GPU pick
  // in the form does. The template's own values would give every profile made
  // from it the same pair, and on Windows/Linux one this computer may not have.
  let picks: { hardware_concurrency?: number; device_memory?: number; platform_version?: string } = {};
  try { picks = await enrichPicksForPreset(fp.id); } catch { /* the template's own values */ }
  const host = await hostScreen();
  if (get().session !== session) return;
  set({ host });
  const templated = fromStored({ ...structuredClone(fp.payload ?? {}), _meta: { gpu_preset_id: fp.id } });
  const base = {
    ...templated,
    hardware_concurrency: picks.hardware_concurrency ?? templated.hardware_concurrency,
    device_memory: picks.device_memory ?? templated.device_memory,
    platform_version: picks.platform_version ?? templated.platform_version,
    timezone: AUTO_TZ, language: AUTO_LANG, geo_mode: "auto" as const,
    // This computer's screen or the template's own, which the backend fits to this display.
    ...screenFor(fp, host),
  };
  const folder = useProfileUi.getState().folder;
  show({
    form: { ...base, id: "", rev: 0, name: nextName(), proxy_id: proxyId, ...preset, gpu_preset_id: fp.id },
    // The Folder the list shows; none from All profiles.
    folder: folder !== "all" ? folder : "",
    tagIds: [],
    newTags: [],
    newProxyLine: "",
    cookies: null,
  });
}

/// Opens the form: on a profile (edit), or on a new one that exists only
/// once Create profile is pressed. `proxyId` starts a new profile on that
/// proxy (the setup checklist's step). `tab` is the tab it opens on. `preset`
/// starts a new profile with these fields set; a `gpu_preset_id` in it picks
/// that library fingerprint (all its fields come with it) instead of a random one.
export function openProfileForm(opts: { mode: "edit"; id: string; tab?: ProfileFormTab } | { mode: "create"; tab?: ProfileFormTab; proxyId?: string | null; preset?: Partial<ProfileForm> }): void {
  // A protected profile opens only once its password is given (#127).
  if (opts.mode === "edit" && isProtected(useProfile.getState().profiles.find((p) => p.id === opts.id))) { void openLocked(opts); return; }
  const session = get().session + 1;
  set({
    open: true, mode: opts.mode, id: opts.mode === "edit" ? opts.id : null, tab: opts.tab ?? "basics",
    draft: null, saved: null, status: "idle", error: null, errorDetail: null, session, theirs: null, tried: false, badFields: [], screenPicked: false,
    password: "", passwordRepeat: "", unlocked: null,
  });
  if (opts.mode === "edit") void loadEdit(session, opts.id);
  else void loadCreate(session, opts.proxyId ?? null, opts.preset);
}

/// A protected profile's form: its password first, in the password dialog, which
/// reads the profile with it (`profile_unlock`) and stays for another try after a
/// wrong one. Cancel opens nothing. The password stays with the form until it closes.
async function openLocked(opts: { id: string; tab?: ProfileFormTab }): Promise<void> {
  const name = useProfile.getState().profiles.find((p) => p.id === opts.id)?.name ?? "";
  const real = await resolveId(opts.id);
  if (!real) return;
  await editsSettled(real);
  const answer = await askPassword({
    kind: "ask",
    title: t("profileLock.editTitle", { name }),
    message: t("profileLock.editMessage"),
    confirmLabel: t("profileLock.open"),
    attempt: async ({ password }) => ({ form: await useProfile.getState().unlockForm(real, password), password }),
  });
  if (!answer) return;
  set({
    open: true, mode: "edit", id: real, tab: opts.tab ?? "basics", draft: null, saved: null, status: "idle", error: null, errorDetail: null,
    session: get().session + 1, theirs: null, tried: false, badFields: [], screenPicked: false,
    password: "", passwordRepeat: "", unlocked: answer.value.password,
  });
  show(draftOf(answer.value.form));
}

/** Closes it; what was not saved is dropped (the dialog asked first), and any password with it. */
export function closeProfileForm(): void {
  set({ open: false, session: get().session + 1, status: "idle", password: "", passwordRepeat: "", unlocked: null });
}

/** The new profile's password or its repeat, as typed. */
export function setFormPassword(changes: Partial<Pick<ProfileFormState, "password" | "passwordRepeat">>): void {
  if (get().status === "saving") return;
  set(changes);
}

// Another space: a profile of the old one is not edited, or made, in it.
onSpaceMoved(() => { if (get().open) closeProfileForm(); });

export function setProfileFormTab(tab: ProfileFormTab): void {
  set({ tab });
}

/** Changes some of the profile's fields; several calls in a row all land. */
export function patchForm(changes: Partial<ProfileForm>): void {
  const s = get();
  const d = s.draft;
  if (!d || s.status === "saving") return;
  const form = { ...d.form, ...changes };
  // A new profile's screen follows another template's OS; a size picked by
  // hand stays only on Windows/Linux (macOS and phones keep their own).
  const fp = s.mode === "create" && changes.gpu_preset_id && changes.gpu_preset_id !== d.form.gpu_preset_id
    ? useProfile.getState().fingerprints.find((x) => x.id === changes.gpu_preset_id) : undefined;
  // A pick by hand ("As in the fingerprint" too) stays on another Windows/Linux one.
  let screenPicked = s.screenPicked || "screen_host" in changes;
  if (fp && !((screenPicked || form.screen_custom) && takesHostScreen(fp))) {
    Object.assign(form, screenFor(fp, s.host));
    screenPicked = false;
  }
  set({ draft: { ...d, form }, screenPicked });
}

/** A number field says its text is (or is no longer) not a number. */
export function setFieldInvalid(field: string, bad: boolean): void {
  const now = get().badFields;
  if (bad === now.includes(field)) return;
  set({ badFields: bad ? [...now, field] : now.filter((f) => f !== field) });
}

/** Changes the parts that live elsewhere: Folder, tags, new tag names, the new proxy line. */
export function patchDraft(changes: Partial<Omit<ProfileFormDraft, "form">>): void {
  const d = get().draft;
  if (!d || get().status === "saving") return;
  set({ draft: { ...d, ...changes } });
}

/** The form's tags with a name typed in: the tag of that name if there is one
 *  (any case), else a new name the form keeps until Save. */
export function addTagName(name: string): void {
  const d = get().draft;
  const n = clean(name);
  if (!d || !n) return;
  const lower = n.toLocaleLowerCase();
  const existing = useTags.getState().tags.find((tag) => tag.name.toLocaleLowerCase() === lower);
  if (existing) { if (!d.tagIds.includes(existing.id)) patchDraft({ tagIds: [...d.tagIds, existing.id] }); return; }
  if (!d.newTags.some((x) => x.toLocaleLowerCase() === lower)) patchDraft({ newTags: [...d.newTags, n] });
}

/** After a conflict: the profile as it is now, with nothing of this form's kept. */
export function reloadProfileForm(): void {
  const theirs = get().theirs;
  if (theirs) show(draftOf(theirs));
}

/** A team's profile this person may only read: the form opens to read. */
export function readOnlyWhy(s: Pick<ProfileFormState, "mode" | "id"> = get()): string | null {
  if (s.mode !== "edit") return null;
  const row = useProfile.getState().profiles.find((p) => p.id === s.id);
  return row ? profileRefusal(row, "edit") : null;
}

/** Why a team does not take the form as it is: Create where the profile goes, or for a new tag. */
export function rightsProblem(s: Pick<ProfileFormState, "mode" | "id" | "draft" | "saved"> = get()): string | null {
  const d = s.draft;
  if (!d) return null;
  const readOnly = readOnlyWhy(s);
  if (readOnly) return readOnly;
  if (s.mode === "create" || d.folder !== s.saved?.folder) {
    const where = teamRefusal("create", inFolder(d.folder));
    if (where) return where;
  }
  return d.newTags.length > 0 ? teamRefusal("create") : null;
}

/** True when Save or Create profile can run now. */
export function canSave(s: ProfileFormState = get()): boolean {
  if (!s.open || !s.draft || s.status === "saving" || problemsOf(s.draft).name || s.badFields.length > 0 || passwordProblemOf(s)) return false;
  if (rightsProblem(s)) return false;
  return s.mode === "create" || isDirty(s);
}

/// Brings a cookies file picked in the form into the saved profile. A running
/// profile cannot take them (the backend writes its cookie store), and a
/// failure is said but does not undo the save: the profile's menu can import
/// them again later.
async function importPickedCookies(id: string, name: string, cookies: ProfileFormDraft["cookies"]): Promise<void> {
  if (!cookies) return;
  if (useProfile.getState().running[id] !== undefined) { toast.err(t("useProfile.stopBeforeCookieImport")); return; }
  try {
    const n = await cookiesImport(id, cookies.items);
    toast.ok(n === 1 ? t("useProfile.cookieImportedOne") : t("useProfile.cookiesImportedMany", { n }));
  } catch (e) {
    toast.fail({ title: t("useProfile.cookiesImportFailed", { name }), detail: e, subject: name });
  }
}

/// The last step of a new profile with a password (#127): sealed once all else
/// landed. A failure is said; the profile is there, open, and its menu sets one later.
async function protectNew(id: string, name: string, password: string): Promise<void> {
  try {
    useProfile.getState().applyRows([await profileProtect(id, password)]);
    // Peek's copy of the fingerprint goes: a sealed profile shows none.
    forgetForm(id);
  } catch (e) {
    toast.fail({ title: t("profileLock.createdOpen", { name }), detail: e, subject: name });
  }
}

/// Saves the form (edit) or creates the profile (create), once: a second
/// press while one runs is ignored. True when it was written; on a failure
/// the form stays open with every value as typed and says why.
export async function saveProfileForm(): Promise<boolean> {
  const s = get();
  if (!s.open || !s.draft || s.status === "saving") return false;
  if (problemsOf(s.draft).name) { set({ tried: true, tab: "basics" }); return false; }
  // A number field shows text that is not a number: it is on screen, so nothing is saved behind it.
  if (s.badFields.length > 0) return false;
  if (passwordProblemOf(s)) { set({ tried: true, tab: "basics" }); return false; }
  if (s.mode === "edit" && !isDirty(s)) return true;
  // A team's rights refuse it: the form says why, and nothing is sent.
  if (rightsProblem(s)) return false;
  const session = s.session;
  const here = () => get().session === session;
  set({ status: "saving", error: null, tried: true });
  let draft = s.draft;
  const name = draft.form.name.trim();
  // A protected profile is read again with the password it was opened with.
  const reread = (id: string) => (s.unlocked !== null ? useProfile.getState().unlockForm(id, s.unlocked) : useProfile.getState().loadForm(id));
  // Closed: the password goes with the form.
  const close = () => { if (here()) set({ open: false, status: "idle", session: get().session + 1, password: "", passwordRepeat: "", unlocked: null }); };
  const failed = (error: string, title: string, detail?: unknown) => {
    if (here()) set({ status: "error", error, errorDetail: explainError(detail) ?? null });
    toast.fail({ title, detail, subject: name });
    return false;
  };

  // The profile exists but one step did not land: the form goes on as its
  // edit form, with what did not land still to save, and says what that was.
  const stayOpen = async (id: string, line: string, why: unknown) => {
    const stored = await reread(id).catch(() => null);
    if (!here()) return false;
    if (!stored) { closeProfileForm(); return false; }
    const opened = draftOf(stored);
    set({
      mode: "edit", id, saved: opened,
      draft: { ...draft, form: { ...draft.form, name, id: stored.id, rev: stored.rev }, tagIds: opened.tagIds, newTags: [] },
      status: "error", error: line, errorDetail: explainError(why) ?? null,
    });
    return false;
  };

  // 1. A proxy from a pasted line is added now, and the profile takes it.
  if (draft.newProxyLine) {
    try {
      const [entry] = await proxyBulkParse(draft.newProxyLine, "socks5");
      if (!entry) throw t("proxySelect.parseFailed");
      const proxy = await proxySave(entry);
      // The Proxies page keeps its own copy; the profile store reads its own.
      storeBus.emit("proxies", "profiles");
      await useProfile.getState().reload();
      draft = { ...draft, newProxyLine: "", form: { ...draft.form, proxy_id: proxy.id } };
      // A retry after a later failure must not add it twice.
      if (here()) set({ draft });
    } catch (e) {
      return failed(t("profileForm.proxyFailed"), t("proxySelect.addFailed"), e);
    }
  }

  if (s.mode === "create") {
    const made = await useProfile.getState().createProfile({ ...draft, form: { ...draft.form, name } });
    if (!made.meta) return failed(t("profileForm.createFailed"), t("useProfile.createFailed"), made.error);
    const meta = made.meta;
    if (made.failed) {
      // It exists: never made twice. The form goes on as its edit form, with
      // what did not land still to save, and says what that was.
      const line = made.failed.step === "folder" ? t("profileForm.createdNoFolder", { folder: draft.folder })
        : made.failed.step === "proxy" ? t("profileForm.createdNoProxy")
        : t("profileForm.createdNoFingerprint");
      toast.fail({ title: t("profileForm.createdPartly", { name: meta.name }), detail: made.failed.error, subject: meta.name });
      return stayOpen(meta.id, line, made.failed.error);
    }
    await importPickedCookies(meta.id, meta.name, draft.cookies);
    if (s.password) await protectNew(meta.id, meta.name, s.password);
    close();
    const ui = useProfileUi.getState();
    // The list brought it into view already (or said that it hides it).
    if (selectVisible(ui).indexById.has(meta.id)) toast.saved(t("profileForm.created", { name: meta.name }));
    ui.openPeek(meta.id);
    return true;
  }

  // 2. The profile, with the rev it was opened at (or, after a conflict,
  // the one on disk now: this version goes over it).
  const id = s.id!;
  const rev = s.theirs?.rev ?? draft.form.rev;
  let why: unknown;
  const result = await useProfile.getState().saveProfile({ ...draft.form, name, rev }, { onFailed: (e) => { why = e; }, password: s.unlocked ?? undefined });
  if (result === "conflict") {
    const theirs = await reread(id).catch(() => null);
    if (here()) set({ status: "conflict", theirs });
    return false;
  }
  if (result === "failed") return failed(t("profileForm.saveFailed"), t("useProfile.saveFailed", { name }), why);

  // 3 and 4. Its tags and its Folder, only what changed.
  const opened = s.saved!;
  if (!sameTags(draft, opened)) {
    // A name typed in becomes a tag only now.
    const tags = useTags.getState();
    const made = draft.newTags.map((n) => tags.create(n)?.id).filter((x): x is string => !!x);
    tags.setTags(id, [...draft.tagIds, ...made]);
  }
  if (draft.folder !== opened.folder) {
    const store = useProfile.getState();
    if (draft.folder && !store.folderRegistry.includes(draft.folder)) store.rememberFolder(draft.folder);
    // Quiet: this form's own "saved" says it. A move that fails has told why
    // already; the form stays open with the Folder still to save.
    if (!(await store.moveMany([id], draft.folder, { quiet: true }))) {
      return stayOpen(id, t("profileForm.savedNoFolder", { folder: draft.folder }), null);
    }
  }

  // 5. Cookies picked in the form, now that it is saved.
  await importPickedCookies(id, name, draft.cookies);
  // 6. The password of a new profile whose create stopped short, as its last step.
  if (s.password) await protectNew(id, name, s.password);

  // 7.
  toast.saved(t("profileForm.saved", { name }));
  close();
  return true;
}
