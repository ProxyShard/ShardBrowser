import { useEffect, useMemo } from "react";
import { Badge, Button, DataList, EmptyState, Tooltip } from "@proxyshard/shardx-ui-kit";
import { PageBody, PageHeader } from "../../widgets/PageHeader";
import { useHeldLoading } from "../../shared/hooks/useHeldLoading";
import { DeleteIcon, NavTrashIcon, RestoreIcon, SearchIcon } from "../../shared/icons";
import { daysUntil, fmtTs } from "../../shared/lib/utils";
import { fmtCountry } from "../../shared/lib/format";
import { LoadError, LoadingRows } from "../../shared/ui/ListStates";
import { ListColumn } from "../../shared/ui/ListColumn";
import { useListLayout, type ListColumnDef } from "../../shared/ui/listHeader";
import { t, useT } from "../../shared/i18n";
import { refocusPageFilter } from "../../shared/lib/focus";
import { toast } from "../../shared/model/toast";
import { activeTeam, useAccess, useSpace } from "../../entities/space";
import { useTeamAdmin, type TeamTrashKind, type TeamTrashRow } from "../../entities/team";
import {
  deletedByName, loadTeamTrash, purgeTeamTrash, restoreTeamTrash, trashActRefusal, useTeamTrash,
} from "../../features/team-trash";

type Column = "name" | "kind" | "from" | "by" | "deleted" | "left";
/** A row with its words, worked out once per read. */
type Item = TeamTrashRow & { key: string; kindWord: string; fromWord: string; byWord: string };

/** Each kind's word; each key called literally. */
function kindWord(kind: TeamTrashKind): string {
  switch (kind) {
    case "profile": return t("team.trashKindProfile");
    case "proxy": return t("team.trashKindProxy");
    case "bookmark": return t("team.trashKindBookmark");
    case "extension": return t("team.trashKindExtension");
    case "automation": return t("team.trashKindAutomation");
    case "tag": return t("team.trashKindTag");
    case "template": return t("team.trashKindTemplate");
  }
}

/** Where it was: a profile's folder, a proxy's type and country. */
function fromWord(r: TeamTrashRow): string {
  if (r.kind === "profile") return r.folder || t("team.trashNoFolder");
  if (r.kind !== "proxy") return "";
  const kind = r.proxy_kind.toUpperCase();
  return r.country ? t("team.trashProxyFrom", { kind, country: fmtCountry(r.country) }) : kind;
}

const leftWord = (r: TeamTrashRow) => {
  const left = daysUntil(r.purge_at);
  return left === 0 ? t("trash.expiresToday") : left === 1 ? t("trash.daysLeftOne") : t("trash.daysLeft", { n: left });
};
/** A small Badge's sides (`px-[5px]`). */
const BADGE = 10;

// One list recipe, as the personal Trash: the columns size to their content
// and are the person's to size, move, hide and sort.
const COLUMNS: readonly ListColumnDef<Column, Item>[] = [
  {
    id: "name", title: (t) => t("trash.colName"), need: 160, min: 140, max: 480, shown: true, hideable: false, movable: false,
    text: (r) => r.name, compare: (a, b) => a.name.localeCompare(b.name),
  },
  {
    id: "kind", title: (t) => t("team.trashColKind"), need: 88, min: 72, max: 200, shown: true, hideable: true, movable: true,
    text: (r) => r.kindWord, role: "secondary", compare: (a, b) => a.kindWord.localeCompare(b.kindWord),
  },
  {
    id: "from", title: (t) => t("team.trashColFrom"), need: 120, min: 96, max: 320, shown: true, hideable: true, movable: true,
    text: (r) => r.fromWord, role: "secondary", compare: (a, b) => a.fromWord.localeCompare(b.fromWord),
  },
  {
    id: "by", title: (t) => t("team.trashColDeletedBy"), need: 112, min: 88, max: 280, shown: true, hideable: true, movable: true,
    text: (r) => r.byWord, role: "secondary", compare: (a, b) => a.byWord.localeCompare(b.byWord),
  },
  {
    id: "deleted", title: (t) => t("trash.colDeleted"), need: 104, min: 104, max: 200, shown: true, hideable: true, movable: true,
    text: (r) => fmtTs(`@${r.deleted_at}`), role: "tabular", compare: (a, b) => a.deleted_at - b.deleted_at,
  },
  {
    id: "left", title: (t) => t("trash.colLeft"), need: 72, min: 72, max: 160, shown: true, hideable: true, movable: true,
    compare: (a, b) => a.purge_at - b.purge_at,
    fit: (r, m) => BADGE + m(leftWord(r), "medium"),
    cell: (r) => {
      const word = leftWord(r);
      // The last day is amber, as in the personal Trash.
      return (
        <Tooltip content={word} whenTruncated>
          <Badge color={daysUntil(r.purge_at) <= 1 ? "warning" : "gray"} size="small" className="max-w-full">
            <span className="truncate">{word}</span>
          </Badge>
        </Tooltip>
      );
    },
  },
];
/** Restore (74 px) and More (24), 4 apart. */
const ACTIONS_TRACK = 104;

/// The team's trash, from the sidebar's Trash in a team (30 days on its server):
/// Restore on the row, Delete for good in its menu and on the Delete key. A member
/// sees what they may See where it was deleted from; acting needs Delete there too.
export function TeamTrashPage() {
  const t = useT();
  const team = useSpace((s) => activeTeam(s.state));
  const status = useTeamTrash((s) => s.status);
  const rows = useTeamTrash((s) => s.rows);
  const loadError = useTeamTrash((s) => s.error);
  const search = useTeamTrash((s) => s.search);
  const setSearch = useTeamTrash((s) => s.setSearch);
  // Names of who deleted what come with the admin's member list.
  const overview = useTeamAdmin((s) => s.overview);
  // A member's buttons follow their rules as they move.
  const grants = useAccess((s) => s.read?.grants);
  useEffect(() => { void loadTeamTrash(); }, []);
  useEffect(() => {
    if (!team?.admin) return;
    const a = useTeamAdmin.getState();
    if (a.teamId !== team.id || !a.overview) void a.load(team.id, true);
  }, [team?.id, team?.admin]);

  const items = useMemo((): Item[] => rows.map((r) => ({
    ...r, key: `${r.kind}:${r.id}`, kindWord: kindWord(r.kind), fromWord: fromWord(r), byWord: deletedByName(r),
  })), [rows, overview, t]);
  const shown = useMemo(() => {
    const q = search.trim().toLowerCase();
    return q ? items.filter((r) => [r.name, r.kindWord, r.fromWord].some((w) => w.toLowerCase().includes(q))) : items;
  }, [items, search]);
  const loading = useHeldLoading(status === "idle" || status === "loading");
  const list = useListLayout({
    storageKey: "shardx.teamTrash.layout",
    defs: COLUMNS,
    items: shown,
    sizingRows: items,
    selectable: false,
    tail: ACTIONS_TRACK,
    actionsLabel: t("profileTable.colActions"),
  });
  const stateShown = !loading && (status === "error" || shown.length === 0);
  const layout = stateShown
    ? { ...list.layout, scroll: { ...list.layout.scroll, role: "region", "aria-label": t("team.trashTitle") } }
    : list.layout;
  const refusalOf = useMemo(() => new Map(items.map((r) => [r.key, trashActRefusal(r)])), [items, grants]);

  return (
    <>
      <PageHeader
        title={t("team.trashTitle")}
        count={!loading && status === "ready" && items.length > 0 ? (search.trim() ? { shown: shown.length, total: items.length } : items.length) : undefined}
        help={{ title: t("team.trashTitle"), body: t("team.trashHint") }}
        filter={!loading && status === "ready" && items.length === 0 ? undefined : { value: search, onChange: setSearch }}
      />
      <PageBody scroll={false} className="relative">
        <ListColumn>
          <DataList<Item>
            aria-label={t("team.trashTitle")}
            items={list.items}
            columns={list.columns}
            layout={layout}
            getId={(r) => r.key}
            getName={(r) => r.name}
            labels={{
              selectAll: t("dataList.selectAll"),
              select: (name) => t("dataList.select", { name }),
              more: t("pageHeader.more"),
              actions: t("profileTable.colActions"),
            }}
            status={loading ? "loading" : status === "error" ? "error" : "ready"}
            loading={<LoadingRows rows={4} inGrid />}
            error={<LoadError what={t("listStates.trash")} error={loadError} onRetry={() => void loadTeamTrash()} />}
            empty={
              items.length > 0 && search.trim()
                ? (
                  <EmptyState
                    icon={<SearchIcon />}
                    title={t("team.trashNoMatch", { q: search.trim() })}
                    actions={<Button variant="neutral" mode="stroke" size="xsmall" onClick={() => { setSearch(""); refocusPageFilter(); }}>{t("listStates.clearSearch")}</Button>}
                  />
                )
                : <EmptyState icon={<NavTrashIcon />} title={t("team.trashEmptyTitle")} hint={t("team.trashEmptyHint")} />
            }
            onDelete={(keys) => {
              const r = items.find((x) => x.key === keys[0]);
              if (!r) return;
              const why = refusalOf.get(r.key);
              // The key says why, as a row's blocked Delete for good does.
              if (why) toast.fail({ title: t("useTrash.purgeFailed", { name: r.name }), detail: why, subject: r.name });
              else void purgeTeamTrash(r);
            }}
            rowActions={(r) => {
              const why = refusalOf.get(r.key) ?? null;
              return (
                <Tooltip content={why ?? undefined}>
                  <Button
                    variant="neutral"
                    mode="ghost"
                    size="3xsmall"
                    leftIcon={<RestoreIcon className="size-3.5" />}
                    // Drawn as the blocked Start is: dimmed, the hover says why.
                    disabled={!!why}
                    data-blocked={why ? "" : undefined}
                    aria-description={why ?? undefined}
                    onClick={(ev) => { ev.stopPropagation(); if (ev.detail > 1 || why) return; void restoreTeamTrash(r); }}
                  >
                    {t("trash.restore")}
                  </Button>
                </Tooltip>
              );
            }}
            menuItems={(r) => {
              const why = refusalOf.get(r.key) ?? undefined;
              return [
                { label: t("trash.restore"), icon: <RestoreIcon className="size-3.5" />, disabled: !!why, hint: why, onSelect: () => void restoreTeamTrash(r) },
                { type: "separator" },
                { label: t("trash.deleteForGood"), icon: <DeleteIcon className="size-3.5" />, danger: true, disabled: !!why, hint: why, onSelect: () => void purgeTeamTrash(r) },
              ];
            }}
          />
        </ListColumn>
      </PageBody>
    </>
  );
}
