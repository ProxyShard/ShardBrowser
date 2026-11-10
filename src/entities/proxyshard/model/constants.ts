import type { ResiType } from "./types";
import { t } from "../../../shared/i18n";

// p0f OS-fingerprint signatures (signature/set endpoint enum).
// `label` is a getter: the list is built once at import time, so the language
// has to be looked up when the label is read, not here.
export const PS_SIGNATURES: { value: string; label: string }[] = [
  { value: "", get label() { return t("constants.signatureNone"); } },
  { value: "ios", label: "iOS" },
  { value: "macos", label: "macOS" },
  { value: "android", label: "Android" },
  { value: "linux", label: "Linux" },
  { value: "win10", label: "Windows 10" },
  { value: "win11", label: "Windows 11" },
];

// Residential relay gateway hosts for generated proxy strings (port depends
// on protocol — see PS_PORT).
export const PS_RELAYS = [
  "relay-eu.shardx.com",
  "relay-ru.shardx.com",
  "relay-ua.shardx.com",
];

// Relay ports differ by protocol: HTTP 8080, SOCKS5 1080.
export const PS_PORT = { http: 8080, socks5: 1080 } as const;

// Username plan token per residential tier.
export const PS_PLAN: Record<ResiType, string> = { standart: "limited", premium: "premium", unmetered: "unlimited" };

// proxy_type query param accepted by /proxies/{profile,countries,regions,cities}.
export const PS_PROXY_TYPE: Record<ResiType, string> = { standart: "standart", premium: "premium", unmetered: "unlimited" };
