// Seed state for the browser mock. Mirrors `engine/src/stub.rs` so
// `bun run dev:web` behaves like the eventual native build.

import type {
  AppSettings,
  AuthorizedKey,
  Identity,
  ListenerInfo,
  PeerInfo,
  Snapshot,
  TofuRequest,
  Vault,
} from "@/lib/api.types";

export interface MockVault {
  vault: Vault;
  state: "idle" | "syncing" | "synced" | "offline" | "attention";
  mainShortSha: string;
  lastActivity: string | null;
  listener: ListenerInfo | null;
  peers: PeerInfo[];
  authorized: AuthorizedKey[];
  snapshots: Snapshot[];
  pendingTofu: TofuRequest[];
}

export const now = () => new Date().toISOString();

const sha = (seed: number) =>
  (Math.abs((seed * 2654435761) & 0xfffffff) >>> 0).toString(16).padStart(7, "0");

const key = (fingerprint: string, comment: string): AuthorizedKey => ({
  fingerprint,
  openssh: `ssh-ed25519 AAAAC3NzaC1lZDI1NTE5${fingerprint.slice(7)} ${comment}`,
  comment,
  addedAt: now(),
});

export function seedVaults(): MockVault[] {
  return [
    {
      vault: {
        id: "notes",
        displayName: "Notes",
        path: "/Users/chris/Notes",
        enabled: true,
        allowConnections: true,
        port: 51820,
        isCspVault: true,
      },
      state: "synced",
      mainShortSha: sha(1),
      lastActivity: now(),
      listener: { bound: true, port: 51820, bindScope: "lan", tlsExpected: true },
      peers: [
        {
          fingerprint: "SHA256:ax9Qm2tLpqf0bL3kZ7vYwRn8sJ4cH1dE6gT0uI2oP",
          address: "192.168.1.42:51820",
          connectedSince: now(),
          syncState: "synced",
        },
      ],
      authorized: [
        key("SHA256:ax9Qm2tLpqf0bL3kZ7vYwRn8sJ4cH1dE6gT0uI2oP", "chris@laptop"),
        key("SHA256:bk2Wp7rNvc1dF8hM4jX9yQs5tU6wA3eR0gB7nL1mZ", "chris@desktop"),
      ],
      snapshots: [{ name: "before-cleanup", createdAt: now(), frontierShas: [sha(11), sha(12)] }],
      pendingTofu: [],
    },
    {
      vault: {
        id: "design-docs",
        displayName: "Design Docs",
        path: "/Users/chris/Work/design-docs",
        enabled: true,
        allowConnections: false,
        port: 51821,
        isCspVault: true,
      },
      state: "syncing",
      mainShortSha: sha(2),
      lastActivity: now(),
      listener: null,
      peers: [],
      authorized: [key("SHA256:cm3Xq8sOwd2eG9iN5kY0zRt6uV7xB4fS1hC8oM2nA", "team-relay")],
      snapshots: [],
      pendingTofu: [],
    },
    {
      vault: {
        id: "photos-backup",
        displayName: "Photos Backup",
        path: "/Users/chris/Pictures/backup",
        enabled: true,
        allowConnections: true,
        port: 51822,
        isCspVault: true,
      },
      state: "offline",
      mainShortSha: sha(3),
      lastActivity: null,
      listener: { bound: true, port: 51822, bindScope: "lan", tlsExpected: false },
      peers: [],
      // Empty authorized set on purpose: lets TOFU fire (spec §8.3).
      authorized: [],
      snapshots: [],
      pendingTofu: [],
    },
  ];
}

export const seedIdentity = (): Identity => ({
  openssh:
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIH8sJ4cH1dE6gT0uI2oPax9Qm2tLpqf0bL3kZ7vYw chris@this-device",
  fingerprint: "SHA256:dn4Yr9tPxe3fH0jO6lZ1aSu7vW8yC5gT2iD9pN3oB",
  source: { kind: "deviceGlobal" },
});

export const seedSettings = (): AppSettings => ({
  newListener: {
    portStrategy: "auto",
    portRangeStart: 51820,
    bindScope: "loopback",
    tofuEnabled: true,
    tlsExpected: true,
  },
  behavior: {
    startAtLogin: true,
    logLevel: "info",
    notifications: {
      tofu: true,
      peerConnect: true,
      peerDisconnect: true,
      offline: true,
      syncError: true,
      supersededEdit: true,
    },
  },
});

export const CLONE_NODEID_WARNING =
  "This clone forked a fresh NodeId. If you intended to resume an existing node " +
  "identity instead, stop now and reconfigure: reusing a possibly-live key on two " +
  "nodes violates CSP §5.1 and can break deterministic convergence.";
