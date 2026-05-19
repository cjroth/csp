// Coverage for the two IdentityIO backends: the mobile vault-adapter store
// and the desktop `~/.context/id_ed25519` Node-fs store. The pure
// load/create logic is covered by identity-store.test.ts.

import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import {
  NodeHomeIdentityIO,
  VAULT_IDENTITY_PATH,
  VaultAdapterIdentityIO,
  loadIdentity,
  loadOrCreateIdentity,
} from '../../src/identity-store.js';
import { FakeDataAdapter } from '../mocks/obsidian.js';

describe('VaultAdapterIdentityIO (mobile)', () => {
  test('read() is null when nothing is on disk', async () => {
    const io = new VaultAdapterIdentityIO(new FakeDataAdapter());
    expect(await io.read()).toBeNull();
  });

  test('write() creates .context, persists the body + .pub sidecar', async () => {
    const fs = new FakeDataAdapter();
    const io = new VaultAdapterIdentityIO(fs);
    await io.write('csp-identity-v1 BODY', 'ssh-ed25519 PUB\n');
    expect(await fs.exists('.context')).toBe(true);
    expect(await fs.read(VAULT_IDENTITY_PATH)).toBe('csp-identity-v1 BODY');
    expect(await fs.read(`${VAULT_IDENTITY_PATH}.pub`)).toBe('ssh-ed25519 PUB\n');
    expect(await io.read()).toBe('csp-identity-v1 BODY');
    expect(io.describe()).toBe(`<vault>/${VAULT_IDENTITY_PATH}`);
  });

  test('write() does not re-mkdir when .context already exists', async () => {
    const fs = new FakeDataAdapter();
    await fs.mkdir('.context');
    const io = new VaultAdapterIdentityIO(fs);
    await io.write('body', 'pub\n');
    expect(await io.read()).toBe('body');
  });

  test('read() treats a whitespace-only file as absent', async () => {
    const fs = new FakeDataAdapter();
    await fs.mkdir('.context');
    await fs.write(VAULT_IDENTITY_PATH, '   \n  ');
    expect(await new VaultAdapterIdentityIO(fs).read()).toBeNull();
  });

  test('loadOrCreateIdentity round-trips through the vault adapter', async () => {
    const fs = new FakeDataAdapter();
    const io = new VaultAdapterIdentityIO(fs);
    const { identity, created } = await loadOrCreateIdentity(io);
    expect(created).toBe(true);
    const ssh = identity.pubkey().toSshString();
    identity.free();

    const reloaded = await loadIdentity(new VaultAdapterIdentityIO(fs));
    expect(reloaded).not.toBeNull();
    expect(reloaded?.pubkey().toSshString()).toBe(ssh);
    reloaded?.free();
  });
});

describe('NodeHomeIdentityIO (desktop ~/.context)', () => {
  let home: string;

  beforeEach(() => {
    home = mkdtempSync(join(tmpdir(), 'csp-id-home-'));
  });
  afterEach(() => {
    rmSync(home, { recursive: true, force: true });
  });

  test('describe() points at <home>/.context/id_ed25519', () => {
    const io = new NodeHomeIdentityIO(home);
    expect(io.describe()).toBe(join(home, '.context', 'id_ed25519'));
  });

  test('read() is null before anything is written', async () => {
    expect(await new NodeHomeIdentityIO(home).read()).toBeNull();
  });

  test('write() creates the dir, the 0600 keyfile, and the .pub sidecar', async () => {
    const io = new NodeHomeIdentityIO(home);
    await io.write('csp-identity-v1 SEED', 'ssh-ed25519 PUB\n');
    const file = join(home, '.context', 'id_ed25519');
    expect(existsSync(file)).toBe(true);
    expect(readFileSync(file, 'utf8')).toBe('csp-identity-v1 SEED');
    expect(readFileSync(`${file}.pub`, 'utf8')).toBe('ssh-ed25519 PUB\n');
    // 0600 — owner read/write only (low 9 perm bits).
    expect(statSync(file).mode & 0o777).toBe(0o600);
    expect(await io.read()).toBe('csp-identity-v1 SEED');
  });

  test('read() treats a whitespace-only keyfile as absent', async () => {
    const io = new NodeHomeIdentityIO(home);
    await io.write('   \n', 'pub\n');
    expect(await io.read()).toBeNull();
  });

  test('loadOrCreateIdentity persists then reuses the same key', async () => {
    const first = await loadOrCreateIdentity(new NodeHomeIdentityIO(home));
    expect(first.created).toBe(true);
    const ssh = first.identity.pubkey().toSshString();
    first.identity.free();

    const second = await loadOrCreateIdentity(new NodeHomeIdentityIO(home));
    expect(second.created).toBe(false);
    expect(second.identity.pubkey().toSshString()).toBe(ssh);
    second.identity.free();
  });
});
