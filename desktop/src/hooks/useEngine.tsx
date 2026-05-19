// The app's single connection to the real engine boundary (`@/lib/api`).
// Loads engine-reported state, subscribes to the live event stream, and
// projects it into UI state + toasts. No fabricated data (spec §6.6/§12).

import { createContext, type ReactNode, useCallback, useContext, useEffect, useState } from "react";
import { toast } from "sonner";
import { api } from "@/lib/api";
import type {
  AggregateStatus,
  AppSettings,
  Identity,
  RestoreTarget,
  Snapshot,
  Vault,
  VaultStatus,
} from "@/lib/api.types";

interface EngineCtx {
  loading: boolean;
  vaults: Vault[];
  statuses: Record<string, VaultStatus>;
  aggregate: AggregateStatus | null;
  identity: Identity | null;
  settings: AppSettings | null;
  reloadAll: () => Promise<void>;
  reloadVault: (id: string) => Promise<void>;
  addLocalFolder: (path: string) => Promise<Vault>;
  cloneRemote: (dest: string, url: string) => Promise<Vault>;
  removeVault: (id: string) => Promise<void>;
  setEnabled: (id: string, on: boolean) => Promise<void>;
  setAllowConnections: (id: string, on: boolean) => Promise<void>;
  authorize: (id: string, pubkey: string) => Promise<void>;
  revoke: (id: string, fingerprint: string) => Promise<void>;
  createSnapshot: (id: string, name: string) => Promise<Snapshot>;
  restore: (id: string, target: RestoreTarget) => Promise<void>;
  saveSettings: (s: AppSettings) => Promise<void>;
}

const Ctx = createContext<EngineCtx | null>(null);

async function notifyNative(title: string, body: string) {
  try {
    const m = await import("@tauri-apps/plugin-notification");
    let granted = await m.isPermissionGranted();
    if (!granted) granted = (await m.requestPermission()) === "granted";
    if (granted) m.sendNotification({ title, body });
  } catch {
    /* best-effort */
  }
}

export function EngineProvider({ children }: { children: ReactNode }) {
  const [loading, setLoading] = useState(true);
  const [vaults, setVaults] = useState<Vault[]>([]);
  const [statuses, setStatuses] = useState<Record<string, VaultStatus>>({});
  const [aggregate, setAggregate] = useState<AggregateStatus | null>(null);
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [settings, setSettings] = useState<AppSettings | null>(null);

  const reloadVault = useCallback(async (id: string) => {
    try {
      const s = await api.getStatus(id);
      setStatuses((m) => ({ ...m, [id]: s }));
    } catch {
      setStatuses((m) => {
        const { [id]: _drop, ...rest } = m;
        return rest;
      });
    }
  }, []);

  const reloadAll = useCallback(async () => {
    const [vs, agg, ident, set] = await Promise.all([
      api.listVaults(),
      api.getAggregateStatus().catch(() => null),
      api.getIdentity().catch(() => null),
      api.getSettings().catch(() => null),
    ]);
    setVaults(vs);
    setAggregate(agg);
    setIdentity(ident);
    setSettings(set);
    const entries = await Promise.all(
      vs.map(async (v) => [v.id, await api.getStatus(v.id)] as const),
    );
    setStatuses(Object.fromEntries(entries));
    setLoading(false);
  }, []);

  // biome-ignore lint/correctness/useExhaustiveDependencies: one-time setup
  useEffect(() => {
    void reloadAll();
    const unsub = api.subscribe((e) => {
      switch (e.type) {
        case "statusTick":
          void reloadVault(e.id);
          break;
        case "aggregateTick":
          api
            .getAggregateStatus()
            .then(setAggregate)
            .catch(() => {});
          break;
        case "vaultsChanged":
          void reloadAll();
          break;
        case "committed":
          void reloadVault(e.id);
          break;
        case "error":
          toast.error(e.message);
          void notifyNative("Context Desktop — sync error", e.message);
          break;
      }
    });
    return unsub;
  }, []);

  function wrap<A extends unknown[], R>(fn: (...a: A) => Promise<R>, after?: () => void) {
    return async (...a: A): Promise<R> => {
      try {
        const r = await fn(...a);
        after?.();
        await api.refreshTray().catch(() => {});
        return r;
      } catch (err) {
        const msg =
          typeof err === "object" && err && "message" in err
            ? String((err as { message: unknown }).message)
            : String(err);
        toast.error(msg);
        throw err;
      }
    };
  }

  const value: EngineCtx = {
    loading,
    vaults,
    statuses,
    aggregate,
    identity,
    settings,
    reloadAll,
    reloadVault,
    addLocalFolder: wrap(
      (p: string) => api.addLocalFolder(p),
      () => void reloadAll(),
    ),
    cloneRemote: wrap(
      (d: string, u: string) => api.cloneRemote(d, u),
      () => void reloadAll(),
    ),
    removeVault: wrap(
      (id: string) => api.removeVault(id),
      () => void reloadAll(),
    ),
    setEnabled: wrap(
      (id: string, on: boolean) => api.setEnabled(id, on),
      () => void reloadAll(),
    ),
    setAllowConnections: wrap(
      async (id: string, on: boolean) => {
        await api.setAllowConnections(id, on);
      },
      () => void reloadAll(),
    ),
    authorize: wrap((id: string, k: string) => api.authorize(id, k)),
    revoke: wrap((id: string, fp: string) => api.revoke(id, fp)),
    createSnapshot: wrap((id: string, n: string) => api.createSnapshot(id, n)),
    restore: wrap((id: string, t: RestoreTarget) => api.restore(id, t)),
    saveSettings: wrap(async (s: AppSettings) => {
      const saved = await api.setSettings(s);
      setSettings(saved);
    }),
  };

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useEngine(): EngineCtx {
  const c = useContext(Ctx);
  if (!c) throw new Error("useEngine must be used within EngineProvider");
  return c;
}
