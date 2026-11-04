import { Switch, Textarea } from "@proxyshard/shardx-ui-kit";
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
