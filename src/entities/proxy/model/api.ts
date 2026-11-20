import { invoke } from "../../../shared/api/invoke";
import type { ProxyEntry, ProxyTestSnapshot } from "./types";

export const proxyList = () => invoke<ProxyEntry[]>("proxy_list");
export const proxySave = (entry: ProxyEntry) => invoke<ProxyEntry>("proxy_save", { entry });

/** A blank entry for the "new proxy" dialog. */
export const emptyProxy = (): ProxyEntry => ({
  id: "",
  name: "",
  kind: "socks5",
  host: "",
  port: 1080,
  username: "",
  password: "",
  country: "",
  notes: "",
});
export const proxyDelete = (id: string) => invoke("proxy_delete", { id });
export const proxyFullTest = (entry: ProxyEntry) => invoke<ProxyTestSnapshot>("proxy_full_test", { entry });
export const proxyLastTest = (id: string) => invoke<ProxyTestSnapshot | null>("proxy_last_test", { id });
export const proxyHistory = (id: string) => invoke<ProxyTestSnapshot[]>("proxy_history", { id });
export const proxyBulkParse = (text: string, kind: ProxyEntry["kind"]) => invoke<ProxyEntry[]>("proxy_bulk_parse", { text, kind });
export const proxyBulkSave = (entries: any[]) => invoke<number>("proxy_bulk_save", { entries });
export const proxyBulkImport = (text: string, kind: string) => invoke<number>("proxy_bulk_import", { text, kind });

/** #162: the proxies with a change-IP link; the link itself stays on this computer. */
export const proxyRotationIds = () => invoke<string[]>("proxy_rotation_ids");
export const proxyRotationGet = (id: string) => invoke<string | null>("proxy_rotation_get", { id });
export const proxyRotationSet = (id: string, url: string | null) => invoke<void>("proxy_rotation_set", { id, url });
export type ChangeIpResult = {
  before: string | null;
  after: { ip: string; country_code: string; city: string } | null;
  changed: boolean;
};
export const proxyChangeIp = (id: string) => invoke<ChangeIpResult>("proxy_change_ip", { id });
