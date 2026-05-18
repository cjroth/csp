// The app's single connection to the engine boundary (`@/lib/api`).
//
// Loads engine-reported state, subscribes to the live event stream, and
// projects events into UI state + notifications. The app computes no merge
// and orders no commits — it only reflects what the engine reports (spec
// §10/§13).

import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from "react";
import { toast } from "sonner";
import { api, runningUnderTauri } from "@/lib/api";
import type {
  AggregateStatus,
  AppSettings,
  CloneOutcome,
  Identity,
  IdentitySource,
  RestoreTarget,
  Snapshot,
  TofuRequest,
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
  pendingTofu: TofuRequest | null;
  reloadAll: () => Promise<void>;
  reloadVault: (id: string) => Promise<void>;
  addLocalFolder: (path: string) => Promise<Vault>;
  cloneRemote: (dest: string, url: string) => Promise<CloneOutcome>;
  removeVault: (id: string) => Promise<void>;
  setEnabled: (id: string, on: boolean) => Promise<void>;
  setAllowConnections: (id: string, on: boolean) => Promise<void>;
  authorize: (id: string, pubkey: string) => Promise<void>;
  revoke: (id: string, fingerprint: string) => Promise<void>;
  respondTofu: (requestId: string, allow: boolean) => Promise<void>;
  createSnapshot: (id: string, name: string) => Promise<Snapshot>;
  restore: (id: string, target: RestoreTarget) => Promise<void>;
  setIdentitySource: (src: IdentitySource) => Promise<void>;
  saveSettings: (s: AppSettings) => Promise<void>;
}

const Ctx = createContext<EngineCtx | null>(null);

async function notifyNative(title: string, body: string) {
  if (!runningUnderTauri) return;
  try {
    const m = await import("@tauri-apps/plugin-notification");
    let granted = await m.isPermissionGranted();
    if (!granted) granted = (await m.requestPermission()) === "granted";
    if (granted) m.sendNotification({ title, body });
  } catch {
    /* notifications are best-effort */
  }
}

export function EngineProvider({ children }: { children: ReactNode }) {
  const [loading, setLoading] = useState(true);
  const [vaults, setVaults] = useState<Vault[]>([]);
  const [statuses, setStatuses] = useState<Record<string, VaultStatus>>({});
  const [aggregate, setAggregate] = useState<AggregateStatus | null>(null);
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [pendingTofu, setPendingTofu] = useState<TofuRequest | null>(null);

  const settingsRef = useRef<AppSettings | null>(null);
  settingsRef.current = settings;

  const notifyEnabled = (k: keyof AppSettings["behavior"]["notifications"]) =>
    settingsRef.current?.behavior.notifications[k] ?? true;

  const reloadVault = useCallback(async (id: string) => {
    try {
      const s = await api.getStatus(id);
      setStatuses((m) => ({ ...m, [id]: s }));
    } catch {
      setStatuses((m) => {
        const { [id]: _, ...rest } = m;
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

  // Subscribe exactly once on mount: the engine event stream is a single
  // long-lived projection; reloadAll/reloadVault are stable useCallbacks.
  // biome-ignore lint/correctness/useExhaustiveDependencies: intentional one-time setup
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
        case "peerConnected":
          if (notifyEnabled("peerConnect")) {
            toast.success(`Peer connected`, { description: e.peerFingerprint });
            void notifyNative("Peer connected", e.peerFingerprint);
          }
          void reloadVault(e.id);
          break;
        case "peerDisconnected":
          if (notifyEnabled("peerDisconnect")) {
            toast(`Peer disconnected`, { description: e.peerFingerprint });
          }
          void reloadVault(e.id);
          break;
        case "tofuRequested":
          setPendingTofu(e.request);
          if (notifyEnabled("tofu")) {
            void notifyNative(
              "New peer wants to connect",
              `${e.request.peerFingerprint} → ${e.request.vaultId}`,
            );
          }
          break;
        case "tofuResolved":
          setPendingTofu((p) => (p?.requestId === e.requestId ? null : p));
          break;
        case "supersededEdit":
          if (notifyEnabled("supersededEdit")) {
            toast.warning("A same-region edit was superseded", {
              description: `${e.path} — the engine resolved this. Recover from ${e.snapshotHint}.`,
            });
          }
          break;
        case "snapshotCreated":
          toast.success(`Restore point created`, { description: e.snapshot.name });
          break;
        case "listenerChanged":
          void reloadVault(e.id);
          break;
        case "error":
          if (notifyEnabled("syncError")) toast.error(e.message);
          break;
      }
    });
    return unsub;
  }, []);

  const wrap =
    <A extends unknown[], R>(fn: (...a: A) => Promise<R>, after?: (r: R) => void) =>
    async (...a: A): Promise<R> => {
      try {
        const r = await fn(...a);
        after?.(r);
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

  const value: EngineCtx = {
    loading,
    vaults,
    statuses,
    aggregate,
    identity,
    settings,
    pendingTofu,
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
    respondTofu: wrap((rid: string, allow: boolean) => api.respondTofu(rid, allow)),
    createSnapshot: wrap((id: string, n: string) => api.createSnapshot(id, n)),
    restore: wrap((id: string, t: RestoreTarget) => api.restore(id, t)),
    setIdentitySource: wrap(async (src: IdentitySource) => {
      const i = await api.setIdentitySource(src);
      setIdentity(i);
    }),
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
