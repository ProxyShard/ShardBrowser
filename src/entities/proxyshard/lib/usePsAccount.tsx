import { create } from "zustand";
import { toast } from "../../../shared/model/toast";
import { t } from "../../../shared/i18n";
import { PsMe } from "../model/types";
import { psGetKey, psSetKey, psMe } from "../model/api";

export type PsStatus = "idle" | "checking" | "ok" | "err";

export type PsAccountStore = {
    // null = still loading the saved key from disk.
    key: string | null;
    me: PsMe | null;
    status: PsStatus;
    err: string;
    keyLoaded: boolean;

    init: () => Promise<void>;
    refreshMe: () => Promise<void>;
    /** Reads the wallet and the order count again in place: the page keeps
     *  what it shows, says nothing while it reads, and a failed read changes
     *  nothing (after a buy, a top-up or a renewal). */
    reloadMe: () => Promise<void>;
    saveKey: (next: string) => Promise<void>;
};

let initing = false;

export const usePsAccount = create<PsAccountStore>((set, get) => ({
    key: null,
    me: null,
    status: "idle",
    err: "",
    keyLoaded: false,

    init: async () => {
        // Once: a second call while the first is on its way (React runs
        // effects twice in development) would check the key twice.
        if (get().keyLoaded || initing) return;
        initing = true;
        try {
            const k = await psGetKey();
            // "checking" as the key lands: a waiter (the plans' keyWorks) never settles on "idle" in between.
            set({ key: k, keyLoaded: true, ...(k ? { status: "checking" as const, err: "" } : {}) });
            if (k) get().refreshMe();
            else set({ status: "idle" });
        } catch {
            set({ key: "", keyLoaded: true });
        } finally {
            initing = false;
        }
    },
    // Validate the saved key against user-api.shardx.com and pull the wallet.
    refreshMe: async () => {
        set({ status: "checking", err: "" });
        try {
            const m = await psMe();
            set({ me: m, status: "ok" });
        } catch (e) {
            set({ me: null, status: "err", err: String(e) });
        }
    },
    reloadMe: async () => {
        try {
            const m = await psMe();
            set({ me: m });
        } catch { /* quiet: the tiles keep what they show */ }
    },
    saveKey: async (next) => {
        const value = next.trim();
        try {
            await psSetKey(value);
            set({ key: value });
            toast.ok(t("usePsAccount.apiKeySaved"));
            if (value) get().refreshMe();
            else set({ me: null, status: "idle" });
        } catch (e) { toast.fail({ title: t("usePsAccount.saveKeyFailed"), detail: e }); }
    },
}));
