import { Check, Copy, Fingerprint, SlidersHorizontal } from "lucide-react";
import { useState } from "react";
import { PageHeader } from "@/components/layout/PageHeader";
import { Panel } from "@/components/layout/Panel";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useEngine } from "@/hooks/useEngine";
import type { AppSettings } from "@/lib/api.types";

const selectClass =
  "rounded-md border border-border bg-background px-2.5 py-1.5 text-sm outline-none focus:ring-2 focus:ring-ring";

function Row({
  title,
  desc,
  children,
}: {
  title: string;
  desc: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-6 py-3.5">
      <div>
        <p className="text-sm font-medium">{title}</p>
        <p className="text-xs text-muted-foreground">{desc}</p>
      </div>
      {children}
    </div>
  );
}

export function SettingsPage() {
  const { identity, settings, saveSettings } = useEngine();
  const [copied, setCopied] = useState(false);

  if (!settings) {
    return <div className="p-10 text-sm text-muted-foreground">Loading settings…</div>;
  }

  const patch = (fn: (s: AppSettings) => AppSettings) =>
    void saveSettings(fn(structuredClone(settings)));

  async function copyKey() {
    if (!identity) return;
    const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
    await writeText(identity.openssh);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }

  return (
    <div className="mx-auto max-w-3xl px-8 py-10">
      <PageHeader
        title="Settings"
        subtitle={
          <>
            Stored in app config on this device — never inside any vault’s{" "}
            <code className="rounded bg-muted/60 px-1 py-0.5 font-mono text-xs">.context/</code>,
            never synced.
          </>
        }
      />

      <Tabs defaultValue="identity" className="gap-5">
        <TabsList className="grid w-full grid-cols-2">
          <TabsTrigger value="identity">Identity</TabsTrigger>
          <TabsTrigger value="behavior">Behavior</TabsTrigger>
        </TabsList>

        <TabsContent value="identity">
          <Panel
            icon={Fingerprint}
            title="Device identity"
            description="Device-global ed25519 key (~/.context/id_ed25519). The private key is never copied or synced. csp-core supports no other key source."
          >
            <div className="space-y-3">
              <div className="flex items-center gap-2">
                <code className="flex-1 truncate rounded-lg border border-border bg-background/60 px-3.5 py-2.5 font-mono text-xs">
                  {identity?.openssh ?? "…"}
                </code>
                <Button
                  variant={copied ? "default" : "outline"}
                  size="sm"
                  onClick={() => void copyKey()}
                  className="shrink-0"
                >
                  {copied ? <Check className="h-4 w-4" /> : <Copy className="h-4 w-4" />}
                  {copied ? "Copied" : "Copy"}
                </Button>
              </div>
              <p className="font-mono text-xs text-muted-foreground">{identity?.fingerprint}</p>
            </div>
          </Panel>
        </TabsContent>

        <TabsContent value="behavior">
          <Panel
            icon={SlidersHorizontal}
            title="App behavior"
            description="Local preferences for this install."
          >
            <div className="divide-y divide-border">
              <Row title="Start at login" desc="Keep syncing in the background.">
                <Switch
                  aria-label="Start at login"
                  checked={settings.startAtLogin}
                  onCheckedChange={(v) =>
                    patch((s) => {
                      s.startAtLogin = v;
                      return s;
                    })
                  }
                />
              </Row>
              <Row title="Listen by default" desc="New folders allow connections automatically.">
                <Switch
                  aria-label="Listen by default"
                  checked={settings.listenByDefault}
                  onCheckedChange={(v) =>
                    patch((s) => {
                      s.listenByDefault = v;
                      return s;
                    })
                  }
                />
              </Row>
              <Row
                title="Plaintext ws://"
                desc="Bind ws:// instead of self-signed wss:// (only behind a TLS-terminating proxy / trusted LAN)."
              >
                <Switch
                  aria-label="Plaintext websocket"
                  checked={settings.noTlsByDefault}
                  onCheckedChange={(v) =>
                    patch((s) => {
                      s.noTlsByDefault = v;
                      return s;
                    })
                  }
                />
              </Row>
              <Row title="Log level" desc="csp_core / ctx tracing filter.">
                <select
                  className={selectClass}
                  value={settings.logLevel}
                  onChange={(e) =>
                    patch((s) => {
                      s.logLevel = e.target.value;
                      return s;
                    })
                  }
                >
                  {["error", "warn", "info", "debug", "trace"].map((l) => (
                    <option key={l} value={l}>
                      {l}
                    </option>
                  ))}
                </select>
              </Row>
            </div>
          </Panel>
        </TabsContent>
      </Tabs>
    </div>
  );
}
