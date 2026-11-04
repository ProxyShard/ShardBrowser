import { useEffect, type ReactNode } from "react";
import { ACCENT_HEX, AUTO_LANG, AUTO_TZ, LABEL_COLORS, LOCALES, OS_OPTIONS, osIdFor } from "../../../shared/constants";
import { AlertIcon } from "../../../shared/icons";
import { HOST_OS } from "../../../shared/lib/utils";
import { CountryFlag } from "../../../shared/ui/CountryFlag";
import { FolderChip } from "../../../shared/ui/FolderChip";
import { MULTICOLOR } from "../../../shared/ui/AccentRadios";
import { labelNames } from "../../../shared/ui/ColorRadios";
import { PropertyButton, PropertyRow, PropertyValue as Value } from "../../../shared/ui/peek";
import { ProtocolPill } from "../../../shared/ui/ProtocolPill";
import { TagChip } from "../../../shared/ui/TagChip";
import { useT } from "../../../shared/i18n";
import { isProtected, openElsewhere, proxyHidden, proxyMissing, useFolderLook, useProfile, type ProfileForm, type ProfileMeta, type ProfileRight } from "../../../entities/profile";
import { rightName } from "../../../entities/space";
import { useExtensions } from "../../../entities/extension";
import { useProfileTags } from "../../../entities/tag";
import { useSync } from "../../../entities/sync";
import { useScreenChoice } from "../../../features/manage-profiles/ui/fields";
import { openProfileForm, type ProfileFormTab } from "../../../features/profile-form";

/// Peek's Profile and Fingerprint sections as rows of label and value: what
/// the profile has, read. Nothing here changes it; a section's Edit, and the
/// Proxy and Extensions values, open the profile form on their tab.

/** A value that opens the form on `tab`, named "<label> <value>" ("Proxy DC Tokyo"). */
function Link({ labelId, id, tab, empty, children }: { labelId: string; id: string; tab: ProfileFormTab; empty?: boolean; children: ReactNode }) {
  return (
    <PropertyButton labelledBy={labelId} empty={empty} onClick={() => openProfileForm({ mode: "edit", id, tab })}>
      {children}
    </PropertyButton>
  );
}

export function ProfileDetails({ profile }: { profile: ProfileMeta }) {
  return (
    // 2 px between 32 px rows: their text sits 14 px apart (owner, "more air").
    <div className="flex flex-col gap-[2px]">
      <FolderDetail folder={profile.folder} />
      <TagsDetail profileId={profile.id} />
      <ColorDetail color={profile.color ?? ""} />
      {isProtected(profile) && <PasswordDetail />}
      <ProxyDetail profile={profile} />
      <ExtensionsDetail profile={profile} />
      <NotesDetail notes={profile.notes} locked={isProtected(profile)} />
      {profile.sync && <SyncDetail profile={profile} />}
      {profile.rights && <AccessDetail profile={profile} />}
      {openElsewhere(profile) && <OpenOnDetail profile={profile} />}
    </div>
  );
}

function FolderDetail({ folder }: { folder: string }) {
  const t = useT();
  const look = useFolderLook(folder);
  return (
    <PropertyRow id="folder" label={t("peek.folder")}>
      {() => (
        <Value empty={!folder}>
          {folder ? <FolderChip name={folder} color={look.color} /> : <span className="truncate">{t("peek.noFolder")}</span>}
        </Value>
      )}
    </PropertyRow>
  );
}

function TagsDetail({ profileId }: { profileId: string }) {
  const t = useT();
  const tags = useProfileTags(profileId);
  return (
    <PropertyRow id="tags" label={t("peek.tags")}>
      {() => (
        <Value empty={tags.length === 0} wrap>
          {tags.length === 0 ? <span className="truncate">{t("peek.noTags")}</span> : tags.map((tag) => <TagChip key={tag.id} tag={tag} />)}
        </Value>
      )}
    </PropertyRow>
  );
}

/** "" is Automatic (derived from the name). */
function ColorDetail({ color }: { color: string }) {
  const t = useT();
  const names = labelNames(t);
  const known = LABEL_COLORS.find((c) => ACCENT_HEX[c.id].toLowerCase() === color.toLowerCase());
  const name = !color ? t("colorSwatches.autoTitle") : known ? names[known.id] : t("accent.custom");
  return (
    <PropertyRow id="color" label={t("inlineEditor.colorLabel")}>
      {() => (
        <Value>
          <span className="flex size-3.5 shrink-0 items-center justify-center">
            <span aria-hidden="true" className="size-[10px] rounded-full ring-1 ring-inset ring-stroke-image forced-content" style={{ background: color || MULTICOLOR }} />
          </span>
          <span className="truncate">{name}</span>
        </Value>
      )}
    </PropertyRow>
  );
}

/** A protected profile: its password is asked at every start (#127). */
function PasswordDetail() {
  const t = useT();
  return (
    <PropertyRow id="password" label={t("peek.password")}>
      {() => (
        <Value>
          <span className="truncate">{t("peek.passwordOn")}</span>
        </Value>
      )}
    </PropertyRow>
  );
}

/** What a sealed value shows while its profile is locked. */
function Locked() {
  const t = useT();
  return <span data-locked className="truncate">{t("profileRow.locked")}</span>;
}

function ProxyDetail({ profile }: { profile: ProfileMeta }) {
  const t = useT();
  const proxies = useProfile((s) => s.proxies);
  const proxy = profile.proxy_id ? proxies.find((p) => p.id === profile.proxy_id) : undefined;
  const missing = proxyMissing(profile, proxies);
  const hidden = proxyHidden(profile, proxies);
  return (
    <PropertyRow id="proxy" label={t("peek.proxy")}>
      {(labelId) => (
        <Link labelId={labelId} id={profile.id} tab="proxy" empty={!profile.proxy_id}>
          {isProtected(profile) ? (
            <Locked />
          ) : missing ? (
            <>
              <AlertIcon className="size-3.5 shrink-0 text-warning-base" />
              <span className="truncate text-warning-text">{t("profileRow.proxyMissing")}</span>
            </>
          ) : hidden ? (
            <span data-proxy-hidden className="truncate">{t("profileRow.proxyHidden")}</span>
          ) : proxy ? (
            <>
              {proxy.country && <CountryFlag cc={proxy.country} height={14} />}
              <span className="min-w-0 truncate">{proxy.name || `${proxy.host}:${proxy.port}`}</span>
              <ProtocolPill kind={proxy.kind} />
            </>
          ) : (
            <span className="truncate">{t("peek.noProxy")}</span>
          )}
        </Link>
      )}
    </PropertyRow>
  );
}

function ExtensionsDetail({ profile }: { profile: ProfileMeta }) {
  const t = useT();
  const items = useExtensions((s) => s.items);
  const ready = useExtensions((s) => s.status === "ready");
  const init = useExtensions((s) => s.init);
  useEffect(() => { void init(); }, [init]);
  const n = profile.extensions.length;
  // A team's the member's list does not show (rights on its folder only) still come at start;
  // "N of M" would count them among the M they do see.
  const unseen = profile.origin === "team" && (!ready || profile.extensions.some((id) => !items.some((e) => e.id === id)));
  const words = isProtected(profile) ? t("profileRow.locked")
    : n === 0 ? t("peek.noExtensions")
    : unseen ? (n === 1 ? t("peek.teamExtensionsOne") : t("peek.teamExtensions", { n }))
      : t("peek.extensionsCount", { n, total: items.length });
  return (
    <PropertyRow id="extensions" label={t("peek.extensions")}>
      {(labelId) => (
        <Link labelId={labelId} id={profile.id} tab="extensions" empty={n === 0}>
          <span className="truncate">{words}</span>
        </Link>
      )}
    </PropertyRow>
  );
}

function NotesDetail({ notes, locked }: { notes: string | null; locked: boolean }) {
  const t = useT();
  if (locked) {
    return (
      <PropertyRow id="notes" label={t("peek.notes")}>
        {() => <Value empty><Locked /></Value>}
      </PropertyRow>
    );
  }
  return (
    <PropertyRow id="notes" label={t("peek.notes")}>
      {() => (
        <Value empty={!notes} wrap>
          {/* Copyable (WIG-04); its line breaks stay. */}
          {notes ? <span data-selectable className="min-w-0 whitespace-pre-wrap break-words">{notes}</span> : <span className="truncate">{t("peek.noNotes")}</span>}
        </Value>
      )}
    </PropertyRow>
  );
}

/** Whether it syncs to the person's other computers, and where it is open; shown while sync is connected. */
function SyncDetail({ profile }: { profile: ProfileMeta }) {
  const t = useT();
  const all = useSync((s) => s.state?.kinds.profiles === "all");
  const sync = profile.sync;
  const value = !sync?.on ? t("peek.syncOff")
    : sync.open_on ? t("peek.syncOpenOn", { device: sync.open_on })
    : sync.conflict ? t("peek.syncConflict")
    : all ? t("peek.syncAll")
    : t("peek.syncOn");
  // A conflict copy says whose changes it holds and why not to run both, whole.
  const conflict = !!sync?.on && !sync.open_on && !!sync.conflict;
  return (
    <PropertyRow id="sync" label={t("peek.sync")}>
      {() => (
        <Value empty={!sync?.on} wrap={conflict}>
          <span className={conflict ? "min-w-0 break-words" : "truncate"}>{value}</span>
        </Value>
      )}
    </PropertyRow>
  );
}

const RIGHTS: readonly ProfileRight[] = ["see", "start", "edit", "delete", "cookies", "take_out"];

/** A team profile: what this person may do with it, and what Start without Cookies means. */
function AccessDetail({ profile }: { profile: ProfileMeta }) {
  const t = useT();
  const r = profile.rights!;
  // The language's own list separator ("查看、启动" in Chinese).
  const allowed = RIGHTS.filter((k) => r[k]).map((k) => rightName(k)).join(t("format.listSeparator"));
  // Start without Cookies: it opens with the team's session, but nothing may drive it.
  const noCookies = r.start && !r.cookies;
  return (
    <PropertyRow id="access" label={t("peek.access")}>
      {() => (
        <Value wrap={noCookies}>
          <span className="flex min-w-0 flex-col">
            <span data-access className="truncate">{allowed}</span>
            {noCookies && <span data-start-no-cookies className="text-help text-text-sub-600">{t("peek.startNoCookies")}</span>}
          </span>
        </Value>
      )}
    </PropertyRow>
  );
}

/** A team profile another device has open: who and where; it starts here once closed there. */
function OpenOnDetail({ profile }: { profile: ProfileMeta }) {
  const t = useT();
  const on = openElsewhere(profile)!;
  return (
    <PropertyRow id="open-on" label={t("peek.openOn")}>
      {() => (
        <Value wrap>
          <span data-open-on className="min-w-0 break-words">
            {on.member_name ? t("peek.openOnWhere", { device: on.device_name || t("rights.anotherDevice"), member: on.member_name }) : on.device_name || t("rights.anotherDevice")}
          </span>
        </Value>
      )}
    </PropertyRow>
  );
}

/** OS, library fingerprint, Screen (Windows and Linux), Timezone, Language. */
export function FingerprintDetails({ form }: { form: ProfileForm }) {
  const t = useT();
  const fingerprints = useProfile((s) => s.fingerprints);
  const current = fingerprints.find((g) => g.id === form.gpu_preset_id);
  // Its OS is the template's; a profile whose template left the library says
  // so under the rows and shows this computer's.
  const os = current ? osIdFor(current.platform as string) : HOST_OS;
  const screen = useScreenChoice(form, () => {}, os);
  const timezone = form.timezone === AUTO_TZ ? t("inlineEditor.timezoneAuto") : form.timezone;
  const language = form.language === AUTO_LANG
    ? t("inlineEditor.languageAuto")
    // Every other entry is the language's own name for itself.
    : LOCALES.find((l) => l.code === form.language)?.label ?? form.language;
  const row = (id: string, label: string, value: string, empty = false) => (
    <PropertyRow id={id} label={label}>
      {() => <Value empty={empty}><span className="truncate">{value}</span></Value>}
    </PropertyRow>
  );
  return (
    <>
      {fingerprints.length > 0 && !current && (
        <p data-template-missing className="m-0 text-help text-text-sub-600">{t("peek.templateMissing")}</p>
      )}
      <div className="flex flex-col gap-[2px]">
        {row("os", t("inlineEditor.osLabel"), OS_OPTIONS.find((o) => o.id === os)?.label ?? os)}
        {row("template", t("peek.template"), current?.label ?? t("peek.noTemplate"), !current)}
        {screen.shown && row("screen", t("inlineEditor.resolutionLabel"), screen.options.find((o) => o.value === screen.value)?.label ?? screen.value)}
        {row("timezone", t("inlineEditor.timezoneLabel"), timezone)}
        {row("language", t("inlineEditor.languageLabel"), language)}
      </div>
    </>
  );
}
