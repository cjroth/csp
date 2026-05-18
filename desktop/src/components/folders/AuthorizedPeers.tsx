import { KeyRound, ShieldCheck, X } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Panel } from "@/components/layout/Panel";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useEngine } from "@/hooks/useEngine";
import { api } from "@/lib/api";
import type { AuthorizedKey } from "@/lib/api.types";

// Authorized peers (spec §6.2 / §10) — maps exactly to ctx authorize / revoke.
// authorized_keys is node-local and never synced.
export function AuthorizedPeers({ vaultId }: { vaultId: string }) {
  const { authorize, revoke } = useEngine();
  const [keys, setKeys] = useState<AuthorizedKey[]>([]);
  const [pubkey, setPubkey] = useState("");

  const load = useCallback(() => {
    api
      .listAuthorized(vaultId)
      .then(setKeys)
      .catch(() => setKeys([]));
  }, [vaultId]);

  useEffect(load, [load]);

  async function add() {
    if (!pubkey.trim()) return;
    await authorize(vaultId, pubkey.trim());
    setPubkey("");
    load();
  }

  async function remove(fp: string) {
    await revoke(vaultId, fp);
    load();
  }

  return (
    <Panel
      icon={ShieldCheck}
      title="Authorized peers"
      description="Keys allowed to connect to this folder. Node-local, never synced."
    >
      <div className="space-y-4">
        {keys.length === 0 ? (
          <div className="rounded-lg border border-dashed border-border px-4 py-6 text-center text-sm text-muted-foreground">
            None yet — the first peer to connect triggers a trust-on-first-use prompt.
          </div>
        ) : (
          <ul className="space-y-2">
            {keys.map((k) => (
              <li
                key={k.fingerprint}
                className="flex items-center gap-3 rounded-lg border border-border bg-background/40 p-3"
              >
                <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-muted/60">
                  <KeyRound className="h-4 w-4 text-muted-foreground" />
                </div>
                <div className="min-w-0 flex-1">
                  <p className="truncate font-mono text-xs">{k.fingerprint}</p>
                  <p className="truncate text-[11px] text-muted-foreground">
                    {k.comment || "no comment"}
                  </p>
                </div>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-7 w-7 text-muted-foreground hover:text-destructive"
                  onClick={() => void remove(k.fingerprint)}
                  title="Revoke"
                >
                  <X className="h-4 w-4" />
                </Button>
              </li>
            ))}
          </ul>
        )}

        <div className="flex gap-2">
          <Input
            value={pubkey}
            placeholder="ssh-ed25519 AAAA… comment"
            className="font-mono text-xs"
            onChange={(e) => setPubkey(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void add()}
          />
          <Button variant="outline" onClick={() => void add()} disabled={!pubkey.trim()}>
            Authorize
          </Button>
        </div>
      </div>
    </Panel>
  );
}
