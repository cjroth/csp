import { Check, Copy, ShieldAlert, ShieldCheck, Wifi } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Panel } from "@/components/layout/Panel";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { api, runningUnderTauri } from "@/lib/api";
import type { ConnectAddress } from "@/lib/api.types";

// Letting another node connect (spec §8). Address + honest, OS-accurate
// firewall guidance. The strong exposure caveat is engine-gated: it only
// appears in the genuinely risky state (non-loopback + empty authorized set
// + TOFU on, spec §8.4); otherwise a brief, non-alarming note.
export function ConnectBlock({ vaultId }: { vaultId: string }) {
  const [addr, setAddr] = useState<ConnectAddress | null>(null);
  const [copied, setCopied] = useState(false);

  const refresh = useCallback(() => {
    api
      .getConnectAddress(vaultId)
      .then(setAddr)
      .catch(() => {});
  }, [vaultId]);

  useEffect(() => {
    refresh();
    // Re-derive after engine events (status ticks guarantee convergence
    // after authorize/revoke/TOFU/settings changes within a tick).
    const unsub = api.subscribe(() => refresh());
    return unsub;
  }, [refresh]);

  async function copy() {
    if (!addr) return;
    if (runningUnderTauri) {
      const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
      await writeText(addr.address);
    } else {
      await navigator.clipboard.writeText(addr.address);
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }

  if (!addr) return null;

  return (
    <Panel
      icon={Wifi}
      title="Connect address"
      description="Share this with a peer to let them clone or sync this folder."
    >
      <div className="space-y-4">
        <div className="flex items-center gap-2">
          <code className="flex-1 truncate rounded-lg border border-border bg-background/60 px-3.5 py-2.5 font-mono text-sm text-foreground">
            {addr.address}
          </code>
          <Button
            variant={copied ? "default" : "outline"}
            size="sm"
            onClick={() => void copy()}
            className="shrink-0"
          >
            {copied ? <Check className="h-4 w-4" /> : <Copy className="h-4 w-4" />}
            {copied ? "Copied" : "Copy"}
          </Button>
        </div>

        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <span className="rounded-md bg-muted/60 px-2 py-1 font-mono">
            {addr.lanIp}:{addr.port}
          </span>
          <span>detected LAN endpoint</span>
        </div>

        <div className="rounded-lg border border-border bg-background/40 p-3.5 text-xs leading-relaxed text-muted-foreground">
          {addr.firewallGuidance}
        </div>

        {addr.noAuthorizedKeys && addr.note ? (
          <Alert variant="destructive">
            <ShieldAlert className="h-4 w-4" />
            <AlertTitle>No authorized keys</AlertTitle>
            <AlertDescription>{addr.note}</AlertDescription>
          </Alert>
        ) : (
          <div className="flex items-start gap-2 rounded-lg border border-border bg-background/40 p-3 text-xs text-muted-foreground">
            <ShieldCheck className="mt-0.5 h-3.5 w-3.5 shrink-0 text-emerald-400" />
            <span>
              Trust-on-first-use is disabled: only the keys you authorize below can connect. Keep
              this on LAN or a private overlay for any public exposure.
            </span>
          </div>
        )}
      </div>
    </Panel>
  );
}
