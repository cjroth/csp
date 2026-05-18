import { Bell, Check, Copy, Fingerprint, Network, SlidersHorizontal } from "lucide-react";
import { useState } from "react";
import { PageHeader } from "@/components/layout/PageHeader";
import { Panel } from "@/components/layout/Panel";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useEngine } from "@/hooks/useEngine";
import { runningUnderTauri } from "@/lib/api";
import type { AppSettings, IdentitySource } from "@/lib/api.types";

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
  const { identity, settings, setIdentitySource, saveSettings } = useEngine();
  const [copied, setCopied] = useState(false);

  if (!settings) {
    return <div className="p-10 text-sm text-muted-foreground">Loading settings…</div>;
  }

  const patch = (fn: (s: AppSettings) => AppSettings) =>
    void saveSettings(fn(structuredClone(settings)));

  const sourceKind = identity?.source.kind ?? "deviceGlobal";

  async function copyKey() {
    if (!identity) return;
    if (runningUnderTauri) {
      const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
      await writeText(identity.openssh);
    } else {
      await navigator.clipboard.writeText(identity.openssh);
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }

  const setSource = (s: IdentitySource) => void setIdentitySource(s);

  const sources = [
    ["deviceGlobal", "Device-global key", "~/.context/id_ed25519 (default)"],
    ["sshAgent", "Running SSH agent", "Delegate signing to ssh-agent"],
    ["sshKey", "Reuse an ~/.ssh key", "~/.ssh/id_ed25519"],
  ] as const;

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
        <TabsList className="grid w-full grid-cols-3">
          <TabsTrigger value="identity">Identity</TabsTrigger>
          <TabsTrigger value="listener">Listener defaults</TabsTrigger>
          <TabsTrigger value="behavior">App behavior</TabsTrigger>
        </TabsList>

        <TabsContent value="identity">
          <Panel
            icon={Fingerprint}
            title="Device identity"
            description="This device’s public key. The private key is never copied or synced."
          >
            <div className="space-y-5">
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

              <div className="space-y-2">
                <Label className="text-xs uppercase tracking-wide text-muted-foreground">
                  Key source
                </Label>
                {sources.map(([kind, title, desc]) => {
                  const active = sourceKind === kind;
                  return (
                    <button
                      type="button"
                      key={kind}
                      onClick={() =>
                        setSource(
                          kind === "sshKey"
                            ? { kind: "sshKey", path: "~/.ssh/id_ed25519" }
                            : ({ kind } as IdentitySource),
                        )
                      }
                      className={`flex w-full items-center gap-3 rounded-lg border p-3 text-left transition-colors ${
                        active
                          ? "border-primary/50 bg-primary/10"
                          : "border-border bg-background/40 hover:border-border hover:bg-accent/40"
                      }`}
                    >
                      <span
                        className={`flex h-4 w-4 items-center justify-center rounded-full border ${
                          active ? "border-primary" : "border-muted-foreground/40"
                        }`}
                      >
                        {active && <span className="h-2 w-2 rounded-full bg-primary" />}
                      </span>
                      <span className="flex-1">
                        <span className="block text-sm font-medium">{title}</span>
                        <span className="block text-xs text-muted-foreground">{desc}</span>
                      </span>
                    </button>
                  );
                })}
                <p className="pt-1 text-xs text-muted-foreground">
                  A per-vault key is an explicit opt-in per folder (stronger isolation, key-sprawl
                  cost).
                </p>
              </div>
            </div>
          </Panel>
        </TabsContent>

        <TabsContent value="listener">
          <Panel
            icon={Network}
            title="Defaults for new listeners"
            description="Applied when a folder first starts allowing connections."
          >
            <div className="divide-y divide-border">
              <Row title="Port assignment" desc="How new listeners pick a port.">
                <select
                  className={selectClass}
                  value={settings.newListener.portStrategy}
                  onChange={(e) =>
                    patch((s) => {
                      s.newListener.portStrategy = e.target
                        .value as AppSettings["newListener"]["portStrategy"];
                      return s;
                    })
                  }
                >
                  <option value="auto">Auto from range</option>
                  <option value="fixed">Fixed</option>
                </select>
              </Row>
              <Row title="Port range start" desc="First port for auto-assignment.">
                <Input
                  type="number"
                  className="w-28"
                  value={settings.newListener.portRangeStart}
                  onChange={(e) =>
                    patch((s) => {
                      s.newListener.portRangeStart = Number(e.target.value);
                      return s;
                    })
                  }
                />
              </Row>
              <Row title="Bind scope" desc="LAN bind is opt-in; loopback is the safe default.">
                <select
                  className={selectClass}
                  value={settings.newListener.bindScope}
                  onChange={(e) =>
                    patch((s) => {
                      s.newListener.bindScope = e.target
                        .value as AppSettings["newListener"]["bindScope"];
                      return s;
                    })
                  }
                >
                  <option value="loopback">Loopback only</option>
                  <option value="lan">LAN</option>
                </select>
              </Row>
              <Row
                title="Trust-on-first-use"
                desc="Prompt on first peer when authorized set is empty."
              >
                <Switch
                  aria-label="Trust on first use"
                  checked={settings.newListener.tofuEnabled}
                  onCheckedChange={(v) =>
                    patch((s) => {
                      s.newListener.tofuEnabled = v;
                      return s;
                    })
                  }
                />
              </Row>
              <Row title="Expect TLS" desc="Off only behind a reverse proxy terminating TLS.">
                <Switch
                  aria-label="Expect TLS"
                  checked={settings.newListener.tlsExpected}
                  onCheckedChange={(v) =>
                    patch((s) => {
                      s.newListener.tlsExpected = v;
                      return s;
                    })
                  }
                />
              </Row>
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
                  checked={settings.behavior.startAtLogin}
                  onCheckedChange={(v) =>
                    patch((s) => {
                      s.behavior.startAtLogin = v;
                      return s;
                    })
                  }
                />
              </Row>
              <Row title="Log level" desc="CTX_LOG equivalent.">
                <select
                  className={selectClass}
                  value={settings.behavior.logLevel}
                  onChange={(e) =>
                    patch((s) => {
                      s.behavior.logLevel = e.target.value;
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
              <div className="py-4">
                <div className="mb-1 flex items-center gap-2">
                  <Bell className="h-3.5 w-3.5 text-muted-foreground" />
                  <p className="text-sm font-medium">Notifications</p>
                </div>
                <p className="mb-3 text-xs text-muted-foreground">
                  Each category can be toggled. Notifications never contain file contents.
                </p>
                <div className="grid grid-cols-1 gap-x-8 sm:grid-cols-2">
                  {(
                    [
                      ["tofu", "New peer / TOFU"],
                      ["peerConnect", "Peer connected"],
                      ["peerDisconnect", "Peer disconnected"],
                      ["offline", "Folder offline"],
                      ["syncError", "Sync error"],
                      ["supersededEdit", "Superseded edit"],
                    ] as const
                  ).map(([k, label]) => (
                    <div
                      key={k}
                      className="flex items-center justify-between border-b border-border/50 py-2.5 text-sm last:border-0"
                    >
                      {label}
                      <Switch
                        aria-label={label}
                        checked={settings.behavior.notifications[k]}
                        onCheckedChange={(v) =>
                          patch((s) => {
                            s.behavior.notifications[k] = v;
                            return s;
                          })
                        }
                      />
                    </div>
                  ))}
                </div>
              </div>
            </div>
          </Panel>
        </TabsContent>
      </Tabs>
    </div>
  );
}
