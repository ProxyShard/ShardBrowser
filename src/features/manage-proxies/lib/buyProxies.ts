import { openUrl } from "@tauri-apps/plugin-opener";
import { withUtm } from "../../../shared/lib/utils";
import { toast } from "../../../shared/model/toast";
import { t } from "../../../shared/i18n";

/// Opens ProxyShard's shop in the browser, from the Proxies `…` menu and
/// from the empty list (CRAFT-25). When the browser cannot open, it says so
/// instead of doing nothing.
export function buyProxies() {
  openUrl(withUtm("https://shardx.com")).catch(() => { toast.err(t("proxiesMoreMenu.buyFailed")); });
}
