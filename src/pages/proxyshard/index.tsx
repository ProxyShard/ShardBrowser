import { useEffect, useState } from "react";
import { Button } from "@proxyshard/shardx-ui-kit";
import { HeaderMenu, PageBody, PageHeader } from "../../widgets/PageHeader";
import { usePsAccount } from "../../entities/proxyshard";
import { OpenDashboardButton, PsBuyDialog, PsKeyDialog, PsKeySetup } from "../../features/proxyshard";
import { PsHeaderFacts } from "../../widgets/ProxyShard/PsHeaderFacts";
import { PsManagementPanels } from "../../widgets/ProxyShard/PsManagementPanels";
import { KeyIcon, RefreshIcon } from "../../shared/icons";
import { useDialogLife } from "../../shared/lib/useDialogLife";
import { useT } from "../../shared/i18n";
import { showMeAround } from "../../features/tour";

type View = "panels" | "setup";

/// The stat tiles, Usage and Orders, across the window's width (owner,
/// 2026-10-23: no 1120 px cap). Buy proxies sits in the header's actions,
/// before Open dashboard (owner, 2026-10-02). Without a
/// working key the key card is the page; with one, the key moves to the `…`
/// menu (craft target state, rank 2).
export function ProxyShardPage() {
  const t = useT();
  const init = usePsAccount((s) => s.init);
  const key = usePsAccount((s) => s.key);
  const status = usePsAccount((s) => s.status);
  const refreshMe = usePsAccount((s) => s.refreshMe);
  const [keyOpen, setKeyOpen] = useState(false);
  const keyLife = useDialogLife(keyOpen);
  const [buying, setBuying] = useState(false);
  const buyLife = useDialogLife(buying);
  useEffect(() => { init(); }, [init]);
  // While a key is being read or checked, the body keeps what it showed (the
  // header says "Checking your key…"): empty before the first answer, so the
  // setup card never flashes, and no swap during Check again or a new key.
  const now: View | null = status === "ok" ? "panels" : status === "err" || key === "" ? "setup" : null;
  const [last, setLast] = useState<View | null>(null);
  if (now && now !== last) setLast(now);
  const view = now ?? last;

  return (
    <>
      <PageHeader
        title="ShardX"
        help={{ title: t("pageHelp.proxyshard.title"), body: t("pageHelp.proxyshard.body"), onTour: () => showMeAround("proxyshard") }}
        after={<PsHeaderFacts />}
        actions={
          <>
            {view === "panels" && (
              <Button variant="neutral" mode="stroke" size="xsmall" onClick={() => setBuying(true)}>{t("psBuyCard.open")}</Button>
            )}
            <OpenDashboardButton />
            {view === "panels" && (
              <HeaderMenu
                items={[
                  { label: t("psKey.changeItem"), icon: <KeyIcon className="size-3.5" />, onSelect: () => setKeyOpen(true) },
                  { label: t("psKey.checkItem"), icon: <RefreshIcon className="size-3.5" />, onSelect: () => void refreshMe() },
                ]}
              />
            )}
          </>
        }
      />
      <PageBody>
        {view === "panels" ? <PsManagementPanels /> : view === "setup" ? <PsKeySetup /> : null}
      </PageBody>
      {buyLife.mounted && <PsBuyDialog key={buyLife.session} open={buying} onClose={() => setBuying(false)} />}
      {keyLife.mounted && <PsKeyDialog key={keyLife.session} open={keyOpen} onClose={() => setKeyOpen(false)} />}
    </>
  );
}
