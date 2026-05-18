// Browser mock of the engine. Semantics mirror `engine/src/stub.rs`:
// the same fixtures, the same 3s tick cadence, the same peer churn and
// superseded-edit schedule, the same dev triggers. This is what makes
// `bun run dev:web` feel like the eventual native build.

import type {
  AggregateStatus,
  AppSettings,
  CloneOutcome,
  ConnectAddress,
  EngineApi,
  EngineEvent,
  Identity,
  IdentitySource,
  ListenerInfo,
  RestoreTarget,
  Snapshot,
  SyncState,
  VaultId,
  VaultStatus,
} from "@/lib/api.types";
import {
  CLONE_NODEID_WARNING,
  type MockVault,
  now,
  seedIdentity,
  seedSettings,
  seedVaults,
} from "@/lib/mock/fixtures";

function aggregate(vaults: MockVault[]): SyncState {
  const enabled = vaults.filter((v) => v.vault.enabled).map((v) => v.state);
  if (enabled.length === 0) return "idle";
  if (enabled.some((s) => s === "attention")) return "attention";
  if (enabled.some((s) => s === "syncing")) return "syncing";
  if (enabled.every((s) => s === "offline")) return "offline";
  if (enabled.some((s) => s === "synced")) return "synced";
  return "idle";
}

const notFound = (msg: string) => ({ kind: "notFound" as const, message: msg });

class MockEngine implements EngineApi {
  private vaults = seedVaults();
  private identity: Identity = seedIdentity();
  private settings: AppSettings = seedSettings();
  private listeners = new Set<(e: EngineEvent) => void>();
  private tick = 0;
  private seq = 0;

  constructor() {
    setInterval(() => this.step(), 3000);
  }

  private emit(e: EngineEvent) {
    for (const l of this.listeners) l(e);
  }

  private find(id: VaultId): MockVault {
    const v = this.vaults.find((x) => x.vault.id === id);
    if (!v) throw notFound(`no vault ${id}`);
    return v;
  }

  private step() {
    this.tick += 1;
    const t = this.tick;
    for (const v of this.vaults.filter((x) => x.vault.enabled)) {
      const prev = v.state;
      const m = t % 4;
      if (v.vault.id === "notes") v.state = m === 0 ? "syncing" : "synced";
      else if (v.vault.id === "design-docs") v.state = m === 0 || m === 2 ? "syncing" : "synced";
      else if (v.vault.id === "photos-backup")
        v.state = m === 1 ? "syncing" : m === 2 ? "synced" : "offline";
      if (v.state !== prev) {
        if (v.state === "syncing") {
          v.mainShortSha = Math.abs(((t + v.vault.port) * 2654435761) & 0xfffffff)
            .toString(16)
            .padStart(7, "0");
          v.lastActivity = now();
        }
        this.emit({
          type: "statusTick",
          id: v.vault.id,
          state: v.state,
          peersConnected: v.peers.length,
          mainShortSha: v.mainShortSha,
        });
      }
    }

    if (t % 4 === 0) {
      const n = this.vaults.find((x) => x.vault.id === "notes");
      if (n) {
        if (n.peers.length === 0) {
          const fp = "SHA256:ax9Qm2tLpqf0bL3kZ7vYwRn8sJ4cH1dE6gT0uI2oP";
          n.peers.push({
            fingerprint: fp,
            address: "192.168.1.42:51820",
            connectedSince: now(),
            syncState: "synced",
          });
          this.emit({ type: "peerConnected", id: "notes", peerFingerprint: fp });
        } else {
          const fp = n.peers.shift()?.fingerprint ?? "";
          this.emit({ type: "peerDisconnected", id: "notes", peerFingerprint: fp });
        }
      }
    }

    if (t % 9 === 0) {
      this.emit({
        type: "supersededEdit",
        id: "design-docs",
        path: "architecture/overview.md",
        snapshotHint: "auto/2026-05-17-pre-merge",
      });
    }

    this.emit({ type: "aggregateTick", state: aggregate(this.vaults) });
  }

  // ---- lifecycle ----
  async listVaults() {
    return this.vaults.map((v) => v.vault);
  }

  async addLocalFolder(path: string) {
    this.seq += 1;
    const seq = this.seq;
    const name = path.split("/").filter(Boolean).pop() ?? "folder";
    const vault = {
      id: `vault-${seq}`,
      displayName: name,
      path,
      enabled: true,
      allowConnections: false,
      port: 51820 + seq,
      isCspVault: seq % 2 === 0,
    };
    this.vaults.push({
      vault,
      state: "idle",
      mainShortSha: "0000000",
      lastActivity: now(),
      listener: null,
      peers: [],
      authorized: [],
      snapshots: [],
      pendingTofu: [],
    });
    this.emit({
      type: "statusTick",
      id: vault.id,
      state: "idle",
      peersConnected: 0,
      mainShortSha: "0000000",
    });
    return vault;
  }

  async cloneRemote(dest: string, url: string): Promise<CloneOutcome> {
    this.seq += 1;
    const seq = this.seq;
    const name = url.replace(/\/+$/, "").split("/").filter(Boolean).pop() ?? "cloned-vault";
    const vault = {
      id: `clone-${seq}`,
      displayName: name,
      path: dest,
      enabled: true,
      allowConnections: false,
      port: 51820 + seq,
      isCspVault: true,
    };
    this.vaults.push({
      vault,
      state: "syncing",
      mainShortSha: "0000000",
      lastActivity: now(),
      listener: null,
      peers: [
        {
          fingerprint: "SHA256:remote-peer-just-cloned",
          address: url,
          connectedSince: now(),
          syncState: "syncing",
        },
      ],
      authorized: [],
      snapshots: [],
      pendingTofu: [],
    });
    this.emit({
      type: "statusTick",
      id: vault.id,
      state: "syncing",
      peersConnected: 1,
      mainShortSha: "0000000",
    });
    return { vault, nodeIdWarning: CLONE_NODEID_WARNING };
  }

  async removeVault(id: VaultId) {
    const before = this.vaults.length;
    this.vaults = this.vaults.filter((v) => v.vault.id !== id);
    if (this.vaults.length === before) throw notFound(`no vault ${id}`);
  }

  async setEnabled(id: VaultId, on: boolean) {
    const v = this.find(id);
    v.vault.enabled = on;
    v.state = on ? "syncing" : "idle";
    if (!on) v.peers = [];
    this.emit({
      type: "statusTick",
      id,
      state: v.state,
      peersConnected: v.peers.length,
      mainShortSha: v.mainShortSha,
    });
  }

  async setAllowConnections(id: VaultId, on: boolean): Promise<ListenerInfo> {
    const v = this.find(id);
    v.vault.allowConnections = on;
    const info: ListenerInfo = {
      bound: on,
      port: v.vault.port,
      bindScope: "lan",
      tlsExpected: true,
    };
    v.listener = on ? info : null;
    this.emit({ type: "listenerChanged", id, listener: info });
    return info;
  }

  async getConnectAddress(id: VaultId): Promise<ConnectAddress> {
    const v = this.find(id);
    const lanIp = "192.168.1.50";
    const isNonLoopback = true;
    // Mirror engine/src/stub.rs: the strong caveat applies only in the
    // genuinely risky state — non-loopback + empty authorized set + TOFU on
    // (spec §8.4 / CSP §13.2). Pre-seeded keys or disabled TOFU remove that
    // specific risk, so the warning is not shown then.
    const trustsFirstComer =
      isNonLoopback && v.authorized.length === 0 && this.settings.newListener.tofuEnabled;
    return {
      scheme: "wss",
      lanIp,
      port: v.vault.port,
      address: `wss://${lanIp}:${v.vault.port}`,
      firewallGuidance:
        "macOS: the Application Firewall is per-application, not per-port. On first " +
        'listen, accept the OS prompt to allow incoming connections for "Context ' +
        'Desktop". If you previously denied it, re-enable it under System Settings → ' +
        "Network → Firewall. The port itself needs no separate macOS rule.",
      isNonLoopback,
      exposureCaveat: trustsFirstComer
        ? "This listener has an empty authorized set and TOFU is on, so it will " +
          "trust whichever device connects first. Prefer LAN or a private overlay " +
          "(VPN/Tailscale); pre-seed authorized keys or disable TOFU before exposing " +
          "it publicly."
        : null,
    };
  }

  // ---- authorization ----
  async listAuthorized(id: VaultId) {
    return this.find(id).authorized;
  }

  async authorize(id: VaultId, pubkey: string) {
    const v = this.find(id);
    v.authorized.push({
      fingerprint: `SHA256:${(pubkey.length * 0x9e37).toString(16)}`,
      openssh: pubkey,
      comment: pubkey.split(/\s+/)[2] ?? "",
      addedAt: now(),
    });
  }

  async revoke(id: VaultId, fingerprint: string) {
    const v = this.find(id);
    v.authorized = v.authorized.filter((k) => k.fingerprint !== fingerprint);
  }

  async respondTofu(requestId: string, allow: boolean) {
    for (const v of this.vaults) {
      const idx = v.pendingTofu.findIndex((t) => t.requestId === requestId);
      if (idx >= 0) {
        const [req] = v.pendingTofu.splice(idx, 1);
        if (allow) {
          v.authorized.push({
            fingerprint: req.peerFingerprint,
            openssh: req.peerOpenssh,
            comment: "tofu-approved",
            addedAt: now(),
          });
        }
        this.emit({ type: "tofuResolved", requestId, allowed: allow });
        return;
      }
    }
    throw notFound(`no pending TOFU ${requestId}`);
  }

  // ---- identity ----
  async getIdentity() {
    return this.identity;
  }

  async setIdentitySource(src: IdentitySource) {
    this.identity = { ...this.identity, source: src };
    return this.identity;
  }

  // ---- settings ----
  async getSettings() {
    return this.settings;
  }

  async setSettings(settings: AppSettings) {
    this.settings = settings;
    return this.settings;
  }

  // ---- recovery ----
  async createSnapshot(id: VaultId, name: string): Promise<Snapshot> {
    const v = this.find(id);
    const snap: Snapshot = {
      name,
      createdAt: now(),
      frontierShas: [v.mainShortSha],
    };
    v.snapshots.push(snap);
    this.emit({ type: "snapshotCreated", id, snapshot: snap });
    return snap;
  }

  async listSnapshots(id: VaultId) {
    return this.find(id).snapshots;
  }

  async restore(id: VaultId, _target: RestoreTarget) {
    const v = this.find(id);
    v.state = "syncing";
    v.lastActivity = now();
    this.emit({
      type: "statusTick",
      id,
      state: "syncing",
      peersConnected: v.peers.length,
      mainShortSha: v.mainShortSha,
    });
  }

  // ---- status ----
  async getStatus(id: VaultId): Promise<VaultStatus> {
    const v = this.find(id);
    return {
      id: v.vault.id,
      state: v.state,
      peersConnected: v.peers.length,
      mainShortSha: v.mainShortSha,
      lastActivity: v.lastActivity,
      listener: v.listener,
      peers: v.peers,
      pendingTofu: v.pendingTofu,
    };
  }

  async getAggregateStatus(): Promise<AggregateStatus> {
    return {
      state: aggregate(this.vaults),
      vaultCount: this.vaults.length,
      syncingCount: this.vaults.filter((v) => v.state === "syncing").length,
      attentionCount: this.vaults.filter((v) => v.state === "attention").length,
    };
  }

  // ---- dev triggers ----
  async devTriggerTofu() {
    this.seq += 1;
    const seq = this.seq;
    const v = this.vaults.find((x) => x.authorized.length === 0) ?? this.vaults[0];
    if (!v) throw notFound("no vaults");
    const req = {
      requestId: `tofu-${seq}`,
      vaultId: v.vault.id,
      peerFingerprint: `SHA256:newpeer${seq.toString(16).padStart(4, "0")}deadbeefcafef00d`,
      peerOpenssh: `ssh-ed25519 AAAAC3NzaC1lZDI1NTE5newpeer${seq} unknown@peer`,
      address: "192.168.1.77:51999",
    };
    v.pendingTofu.push(req);
    this.emit({ type: "tofuRequested", request: req });
  }

  async devTriggerSuperseded() {
    const v = this.vaults[0];
    if (!v) throw notFound("no vaults");
    this.emit({
      type: "supersededEdit",
      id: v.vault.id,
      path: "notes/2026-05-17.md",
      snapshotHint: "auto/pre-merge-2026-05-17",
    });
  }

  // ---- stream ----
  subscribe(cb: (e: EngineEvent) => void) {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }

  async refreshTray() {
    /* no native tray in the browser */
  }
}

let singleton: MockEngine | null = null;

export function mockEngine(): EngineApi {
  if (!singleton) {
    singleton = new MockEngine();
    if (typeof window !== "undefined") {
      window.mockTriggerTofu = () => void singleton?.devTriggerTofu();
      window.mockTriggerSuperseded = () => void singleton?.devTriggerSuperseded();
    }
  }
  return singleton;
}
