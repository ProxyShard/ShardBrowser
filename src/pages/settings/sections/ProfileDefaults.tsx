import { Input, Switch, Textarea } from "@proxyshard/shardx-ui-kit";
import { useT } from "../../../shared/i18n";
import { riskyLaunchArgs, type LaunchArgRisk } from "../../../shared/lib/launchArgs";
import { SelectField } from "../../../shared/ui/SelectField";
import { SettingRow, SettingsGroup, type SettingsProps } from "../SettingRow";

export function ProfileDefaultsSection({ s, setS, p, setP }: SettingsProps) {
  const t = useT();
  return (
    <SettingsGroup>
      <SettingRow
        id="geo"
        title={t("settings.geoCheckerTitle")}
        help={t("settings.geoCheckerHelp")}
        control={({ labelId, helpId }) => (
          <div className="w-[240px]">
            <SelectField
              aria-labelledby={labelId}
              aria-describedby={helpId}
              value={s.geo_checker ?? "ip-api.com"}
              onChange={(v) => setS({ ...s, geo_checker: v })}
              options={[
                // Plain HTTP says so in a badge that is never cut, in the
                // list and in the closed field (SEC-12, controller ruling).
                { value: "ip-api.com", label: t("settings.geoIpApiCom"), badge: { label: t("settings.geoNotEncrypted"), color: "warning" } },
                { value: "ipapi.co", label: t("settings.geoIpapiCo") },
                { value: "ipwho.is", label: t("settings.geoIpwhoIs") },
              ]}
            />
          </div>
        )}
      />
      <ProbeField id="geo-custom" field="geo_custom_url" kind="url" title={t("settings.geoCustomTitle")} help={t("settings.geoCustomHelp")} placeholder="https://ipinfo.io/json" s={s} setS={setS} />
      <ProbeField id="probe-host" field="probe_host" kind="host" title={t("settings.probeHostTitle")} help={t("settings.probeHostHelp")} placeholder="example.com:443" s={s} setS={setS} />
      <ProbeField id="stun-server" field="stun_server" kind="host" title={t("settings.stunServerTitle")} help={t("settings.stunServerHelp")} placeholder="stun.l.google.com:19302" s={s} setS={setS} />
      <ChangeIpKeyField s={s} setS={setS} />
      <SettingRow
        id="screen"
        title={t("settings.screenModeLabel")}
        help={t("settings.screenHelp")}
        control={({ labelId, helpId }) => (
          // As wide as its longest choice, so its help keeps one line.
          <div className="w-[180px]">
            <SelectField
              aria-labelledby={labelId}
              aria-describedby={helpId}
              value={s.screen_resolution_mode ?? "fingerprint"}
              onChange={(v) => setS({ ...s, screen_resolution_mode: v })}
              options={[
                { value: "fingerprint", label: t("settings.screenModeFingerprint") },
                { value: "real", label: t("settings.screenModeReal") },
              ]}
            />
          </div>
        )}
      />
      <SettingRow
        id="camera"
        title={t("settings.cameraSwitchLabel")}
        help={t("settings.cameraHelp")}
        control={({ labelId, helpId }) => (
          <Switch aria-labelledby={labelId} aria-describedby={helpId} checked={s.camera_enabled ?? true} onChange={(on) => setS({ ...s, camera_enabled: on })} />
        )}
      />
      <SettingRow
        id="profile-name"
        title={t("settings.profileNameLabel")}
        help={t("settings.profileNameHelp")}
        control={({ labelId, helpId }) => (
          <Switch aria-labelledby={labelId} aria-describedby={helpId} checked={s.show_profile_name ?? true} onChange={(on) => setS({ ...s, show_profile_name: on })} />
        )}
      />
      {/* Off, no password can be set; profiles that have one keep it. */}
      <SettingRow
        id="profile-passwords"
        title={t("settings.profilePasswordsLabel")}
        help={t("settings.profilePasswordsHelp")}
        control={({ labelId, helpId }) => (
          <Switch aria-labelledby={labelId} aria-describedby={helpId} checked={s.profile_passwords ?? false} onChange={(on) => setS({ ...s, profile_passwords: on })} />
        )}
      />
      {/* Lives in this window's storage, not in settings.json; changed on Save. */}
      <SettingRow
        id="direct-start"
        title={t("settings.directAsk")}
        help={t("settings.directHelp")}
        control={({ labelId, helpId }) => (
          <Switch aria-labelledby={labelId} aria-describedby={helpId} checked={p.askDirect} onChange={(on) => setP("askDirect", on)} />
        )}
      />
      <SettingRow
        id="extra-args"
        title={t("settings.extraArgsTitle")}
        help={t("settings.extraArgsHelp")}
        below={
          <>
            <Textarea
              aria-label={t("settings.extraArgsTitle")}
              rows={3}
              className="mono"
              value={s.extra_args ?? ""}
              // Part of the draft like every field; the question about a
              // risky switch comes on Save (SEC-10).
              onChange={(e) => setS({ ...s, extra_args: e.target.value })}
              placeholder={"--disable-background-timer-throttling\n--window-size=1280,800"}
            />
            <RiskyArgsNote raw={s.extra_args ?? ""} />
          </>
        }
      />
    </SettingsGroup>
  );
}

const HOST_PORT = /^(?:[a-z][a-z0-9+.-]*:\/\/)?[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?(?::\d{1,5})?\/?$/i;

/// What a proxy test reaches (#157): empty keeps the built-in target.
function ProbeField({ id, field, kind, title, help, placeholder, s, setS }: {
  id: string;
  field: "geo_custom_url" | "probe_host" | "stun_server";
  kind: "url" | "host";
  title: string;
  help: string;
  placeholder: string;
} & Pick<SettingsProps, "s" | "setS">) {
  const t = useT();
  const value = s[field] ?? "";
  const bad = value.trim() !== "" && (kind === "url" ? !/^https?:\/\/\S+$/i.test(value.trim()) : !HOST_PORT.test(value.trim()));
  return (
    <SettingRow
      id={id}
      title={title}
      help={help}
      control={({ labelId, helpId }) => (
        <div className="w-[240px]">
          <Input
            aria-label={title}
            aria-labelledby={labelId}
            aria-describedby={helpId}
            inputSize="xsmall"
            className="mono"
            spellCheck={false}
            value={value}
            placeholder={placeholder}
            error={!bad ? undefined : kind === "url" ? t("settings.probeBadUrl") : t("settings.probeBadHost")}
            onChange={(e) => setS({ ...s, [field]: e.target.value.trim() === "" ? null : e.target.value })}
          />
        </div>
      )}
    />
  );
}

/// A key and its modifiers as the browser reads them (#162), or null for no key yet.
export function shortcutOf(e: Pick<KeyboardEvent, "code" | "ctrlKey" | "altKey" | "shiftKey" | "metaKey">): string | null {
  const key = /^Key([A-Z])$/.exec(e.code)?.[1] ?? /^Digit(\d)$/.exec(e.code)?.[1] ?? /^(F(?:[1-9]|1\d|2[0-4]))$/.exec(e.code)?.[1];
  if (!key) return null;
  const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Cmd"].filter(Boolean);
  // A bare letter or digit would fire while typing; a bare F key is fine.
  if (mods.length === 0 && !key.startsWith("F")) return null;
  return [...mods, key].join("+");
}

/// The in-browser "Change IP" shortcut (#162): pressed, not typed. Backspace clears it.
function ChangeIpKeyField({ s, setS }: Pick<SettingsProps, "s" | "setS">) {
  const t = useT();
  return (
    <SettingRow
      id="change-ip-key"
      title={t("settings.changeIpKeyTitle")}
      help={t("settings.changeIpKeyHelp")}
      control={({ labelId, helpId }) => (
        <div className="w-[240px]">
          <Input
            aria-label={t("settings.changeIpKeyTitle")}
            aria-labelledby={labelId}
            aria-describedby={helpId}
            inputSize="xsmall"
            className="mono"
            readOnly
            value={s.change_ip_hotkey ?? ""}
            placeholder={t("settings.changeIpKeyPlaceholder")}
            onKeyDown={(e) => {
              if (e.key === "Tab") return;
              e.preventDefault();
              if ((e.key === "Backspace" || e.key === "Delete") && !e.ctrlKey && !e.altKey && !e.metaKey) {
                setS({ ...s, change_ip_hotkey: null });
                return;
              }
              const key = shortcutOf(e.nativeEvent);
              if (key) setS({ ...s, change_ip_hotkey: key });
            }}
          />
        </div>
      )}
    />
  );
}

/// Each risky switch in the field, named with what it does (SEC-10).
function RiskyArgsNote({ raw }: { raw: string }) {
  const t = useT();
  const risky = riskyLaunchArgs(raw);
  const riskText = (flag: string, risk: LaunchArgRisk) =>
    risk === "proxy" ? t("settings.argProxy", { flag })
    : risk === "exec" ? t("settings.argExec", { flag })
    : risk === "debug" ? t("settings.argDebug", { flag })
    : risk === "profile" ? t("settings.argProfile", { flag })
    : risk === "extension" ? t("settings.argExtension", { flag })
    : risk === "cert" ? t("settings.argCert", { flag })
    : t("settings.argUnsafe", { flag });
  if (risky.length === 0) return null;
  return (
    <ul data-args-warnings className="m-0 mt-2 flex list-none flex-col gap-1 p-0">
      {risky.map((r) => (
        <li key={r.flag} className="flex items-start gap-1.5 text-secondary text-text-sub-600">
          <span aria-hidden="true" className="mt-1.5 size-[5px] shrink-0 rounded-full bg-warning-base" />
          {riskText(r.flag, r.risk)}
        </li>
      ))}
    </ul>
  );
}
