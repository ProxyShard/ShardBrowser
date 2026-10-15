import { create } from 'zustand'
import { ProxyEntry, ProxyTestSnapshot } from '../model/types'
import { proxyFullTest, proxyLastTest, proxyList, proxyDelete, proxySave, proxyBulkImport } from '../model/api';
import { profileBindProxy, profileList } from '../../profile/model/api';
import { toast } from '../../../shared/lib/toast';
import { clip } from '../../../shared/lib/clipboard';
import { confirmModal } from '../../../shared/lib/confirm';
import { storeBus } from '../../../shared/lib/storeBus';
import { t } from '../../../shared/i18n';
import { ProfileMeta } from '../../profile/model/types';

export type ProxyInfoTarget = { proxy: ProxyEntry; anchor: { x: number; y: number } };

/// Shared by the table and by shift-click, so a range covers what is visible.
export function filterProxies(
    proxies: ProxyEntry[],
    snapshots: Record<string, ProxyTestSnapshot>,
    search: string,
): ProxyEntry[] {
    const q = search.trim().toLowerCase();
    if (!q) return proxies;
    return proxies.filter((p) => {
        const snap = snapshots[p.id];
        return (
            p.name.toLowerCase().includes(q) ||
            p.host.toLowerCase().includes(q) ||
            String(p.port).includes(q) ||
            p.country.toLowerCase().includes(q) ||
            p.notes.toLowerCase().includes(q) ||
            p.username.toLowerCase().includes(q) ||
            (snap?.ip ?? '').toLowerCase().includes(q) ||
            (snap?.city ?? '').toLowerCase().includes(q) ||
            (snap?.isp ?? '').toLowerCase().includes(q)
        );
    });
}

export type ProxyStore = {

    status: 'idle' | 'loading' | 'ready' | 'error';
    error: string | null;

    proxyTesting: Record<string, boolean>,
    proxies: ProxyEntry[],
    snapshots: Record<string, ProxyTestSnapshot>,
    proxySel: Set<string>,
    profiles: ProfileMeta[],

    // UI state lives in the store so feature buttons stay prop-free.
    editing: ProxyEntry | null,
    bulkOpen: boolean,
    infoFor: ProxyInfoTarget | null,
    search: string,
    /// The bulk "spread these across profiles" dialog.
    distributeOpen: boolean,
    /// Row the last plain click landed on; a shift-click selects the run from it.
    anchorId: string | null,

    init: () => Promise<void>,
    testProxy: (p: ProxyEntry) => Promise<'error' | 'ok'>,
    reload: () => Promise<void>,
    setProxies: (proxies: ProxyEntry[]) => void,
    setSnapshots: (snapshots: Record<string, ProxyTestSnapshot>) => void,
    selectProxy: (isChecked: boolean, proxies: ProxyEntry[]) => void,
    /** Shift-click: selects every row between the anchor and `id`. */
    selectRangeTo: (id: string) => void,
    clearSelected: () => void,

    setEditing: (p: ProxyEntry | null) => void,
    setBulkOpen: (open: boolean) => void,
    setInfoFor: (target: ProxyInfoTarget | null) => void,
    setSearch: (q: string) => void,
    setDistributeOpen: (open: boolean) => void,

    renameProxy: (id: string, name: string) => Promise<void>,
    removeProxy: (id: string) => Promise<void>,
    bulkTest: () => Promise<void>,
    bulkDelete: () => Promise<void>,
    bulkExport: () => void,
    bulkImportClipboard: () => Promise<void>,
    /** Binds the selected proxies to `profileIds`, one each; returns how many. */
    distribute: (profileIds: string[]) => Promise<number>,
}
export const useProxy = create<ProxyStore>((set, get) => ({
    status: 'idle',
    error: null,
    proxyTesting: {},
    proxies: new Array<ProxyEntry>(),
    snapshots: {},
    proxySel: new Set<string>(),
    profiles: new Array<ProfileMeta>(),
    editing: null,
    bulkOpen: false,
    infoFor: null,
    search: '',
    distributeOpen: false,
    anchorId: null,
    testProxy: async (p: ProxyEntry) => {
        set({ proxyTesting: { ...get().proxyTesting, [p.id]: true } });
        try {
            const snap = await proxyFullTest(p);
            set({ snapshots: { ...get().snapshots, [p.id]: snap } });
            // Refresh: backend may have just populated the country tag.
            get().reload();
        } catch (e) {
            return 'error';
        } finally {
            set({ proxyTesting: { ...get().proxyTesting, [p.id]: false } });
            return 'ok';
        }
    },
    init: async () => {
        // защита от повторного запуска
        if (get().status === 'loading' || get().status === 'ready') return;
        set({ status: 'loading' });
        try {
            const proxies = await proxyList();
            const profiles = await profileList();
            set({ proxies, profiles, status: 'ready' });
            // A profile bound elsewhere changes the count in the Profiles column,
            // and a proxy added from the profile editor belongs in this table.
            // Reload never emits, so the two stores cannot ping-pong.
            storeBus.on('profiles', () => { void get().reload(); });
            storeBus.on('proxies', () => { void get().reload(); });

            if (proxies.length === 0) return;
            const entries = await Promise.all(
                proxies.map(async (p) => {
                    try {
                        const snap = await proxyLastTest(p.id);
                        return [p.id, snap] as const;
                    } catch {
                        return [p.id, null] as const;
                    }
                }),
            )
            const next: Record<string, ProxyTestSnapshot> = {};
            for (const [id, snap] of entries) if (snap) next[id] = snap;
            set({ snapshots: next });

        } catch (e) {
            set({ status: 'error', error: (e as Error).message });
        }
    },
    reload: async () => {
        try {
            set({ proxies: await proxyList() });
            set({ profiles: await profileList() });
        } catch (e) { toast.err(String(e)); }
    },
    setProxies: (proxies: ProxyEntry[]) => set({ proxies }),
    setSnapshots: (snapshots: Record<string, ProxyTestSnapshot>) => set({ snapshots }),
    selectProxy: (isChecked: boolean, proxies: ProxyEntry[]) => {
        const next = new Set(get().proxySel);
        if (isChecked) {
            for (const p of proxies) next.add(p.id);
        } else {
            for (const p of proxies) next.delete(p.id);
        }
        const single = proxies.length === 1 ? proxies[0].id : null;
        set({ proxySel: next, anchorId: single ?? get().anchorId });
    },
    // Runs over the list as the table orders it. The row you click decides the
    // direction: a ticked one unticks the run, an unticked one ticks it.
    selectRangeTo: (id: string) => {
        const order = filterProxies(get().proxies, get().snapshots, get().search).map((p) => p.id);
        const to = order.indexOf(id);
        if (to < 0) return;
        const anchor = get().anchorId;
        // Only a still-ticked anchor has a run to extend; see useProfile.
        const from = anchor && get().proxySel.has(anchor) ? order.indexOf(anchor) : -1;
        if (from < 0) {
            const row = get().proxies.filter((p) => p.id === id);
            get().selectProxy(!get().proxySel.has(id), row);
            return;
        }
        const [lo, hi] = from <= to ? [from, to] : [to, from];
        const removing = get().proxySel.has(id);
        const next = new Set(get().proxySel);
        for (let i = lo; i <= hi; i++) {
            if (removing) next.delete(order[i]); else next.add(order[i]);
        }
        set({ proxySel: next });
    },
    clearSelected: () => set({ proxySel: new Set<string>(), anchorId: null }),

    setEditing: (editing) => set({ editing }),
    setBulkOpen: (bulkOpen) => set({ bulkOpen }),
    setInfoFor: (infoFor) => set({ infoFor }),
    setSearch: (search) => set({ search }),
    setDistributeOpen: (distributeOpen) => set({ distributeOpen }),

    renameProxy: async (id, name) => {
        const entry = get().proxies.find((p) => p.id === id);
        if (!entry) return;
        const newName = name.trim();
        if (newName === entry.name) return;
        try {
            await proxySave({ ...entry, name: newName });
            get().reload();
            storeBus.emit('proxies');
        } catch (e) { toast.err(String(e)); }
    },
    removeProxy: async (id) => {
        if ((await confirmModal({ title: t("useProxy.deleteTitle"), message: t("useProxy.deleteMessage"), danger: true })) !== true) return;
        try { await proxyDelete(id); get().reload(); storeBus.emit('proxies'); toast.ok(t("useProxy.deleted")); }
        catch (e) { toast.err(String(e)); }
    },
    // Capped-parallel bulk TCP/UDP/geo to avoid socket fan-out.
    bulkTest: async () => {
        const { proxySel, proxies, testProxy } = get();
        const ids = [...proxySel];
        if (ids.length === 0) return;
        toast.info(ids.length === 1 ? t("useProxy.testingOne", { n: ids.length }) : t("useProxy.testingMany", { n: ids.length }));
        const targets = proxies.filter((p) => proxySel.has(p.id));
        const CONCURRENCY = 5;
        let i = 0;
        await Promise.all(
            Array.from({ length: Math.min(CONCURRENCY, targets.length) }, async () => {
                while (i < targets.length) {
                    const p = targets[i++];
                    if (!p) break;
                    await testProxy(p);
                }
            }),
        );
        toast.ok(t("useProxy.bulkTestDone"));
    },
    bulkDelete: async () => {
        const { proxySel } = get();
        const ids = [...proxySel];
        if (ids.length === 0) return;
        if ((await confirmModal({ title: t("useProxy.bulkDeleteTitle"), message: ids.length === 1 ? t("useProxy.bulkDeleteOneMessage", { n: ids.length }) : t("useProxy.bulkDeleteManyMessage", { n: ids.length }), danger: true })) !== true) return;
        for (const id of ids) {
            try { await proxyDelete(id); } catch (e) { toast.err(String(e)); }
        }
        get().clearSelected();
        get().reload();
        storeBus.emit('proxies');
        toast.ok(t("useProxy.deletedCount", { n: ids.length }));
    },
    // Export in bulk-import format so a round-trip preserves the name.
    bulkExport: () => {
        const { proxySel, proxies } = get();
        const targets = proxies.filter((p) => proxySel.has(p.id));
        if (targets.length === 0) return;
        const lines = targets.map((p) => {
            const auth = p.username || p.password ? `${p.username}:${p.password}@` : "";
            const base = `${p.kind}://${auth}${p.host}:${p.port}`;
            // Only the name: the country is derived by the test on import, so
            // writing it down would just be a second copy to go stale.
            const named = p.name && p.name !== `${p.host}:${p.port}`;
            return named ? `${base}  # ${p.name}` : base;
        });
        const text = lines.join("\n");
        clip.write(text).then(
            () => toast.ok(t("useProxy.copiedCount", { n: targets.length })),
            (e) => toast.err(t("useProxy.copyFailed", { error: String(e) })),
        );
    },
    // One proxy per profile, in the order shown, stopping when the proxies run
    // out. Never twice: two profiles behind one IP is the thing an operator
    // distributing proxies is avoiding.
    distribute: async (profileIds) => {
        const { proxySel, proxies } = get();
        const picked = proxies.filter((p) => proxySel.has(p.id));
        const n = Math.min(picked.length, profileIds.length);
        if (n === 0) return 0;
        let bound = 0;
        for (let i = 0; i < n; i++) {
            try {
                await profileBindProxy(profileIds[i], picked[i].id);
                bound++;
            } catch (e) { toast.err(String(e)); }
        }
        set({ distributeOpen: false });
        get().reload();
        storeBus.emit('profiles');
        toast.ok(bound === 1 ? t("useProxy.boundOne", { n: bound }) : t("useProxy.boundMany", { n: bound }));
        return bound;
    },

    // Import from clipboard (one per line, bulkExport format).
    bulkImportClipboard: async () => {
        try {
            let text: string;
            try {
                text = await clip.read();
            } catch {
                toast.err(t("useProxy.clipboardEmpty"));
                return;
            }
            if (!text.trim()) { toast.err(t("useProxy.clipboardEmpty")); return; }
            const n = await proxyBulkImport(text, "socks5");
            get().reload();
            storeBus.emit('proxies');
            toast.ok(n === 1 ? t("useProxy.importedOne", { n }) : t("useProxy.importedMany", { n }));
        } catch (e) { toast.err(t("useProxy.importFailed", { error: String(e) })); }
    },
}))
