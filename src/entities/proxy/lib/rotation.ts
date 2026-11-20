import { create } from "zustand";
import { proxyRotationIds } from "../model/api";

/// #162: which proxies have a change-IP link, so Change IP shows only there.
type RotationStore = {
  ids: ReadonlySet<string>;
  load: () => Promise<void>;
};

export const useRotation = create<RotationStore>((set) => ({
  ids: new Set(),
  load: async () => {
    try { set({ ids: new Set(await proxyRotationIds()) }); } catch { /* the list stays as it was */ }
  },
}));
