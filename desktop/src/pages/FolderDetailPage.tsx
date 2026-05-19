import { ArrowLeft, FolderOpen, Trash2 } from "lucide-react";
import { useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { ConfirmRemoveDialog } from "@/components/dialogs/ConfirmRemoveDialog";
import { AuthorizedPeers } from "@/components/folders/AuthorizedPeers";
import { ConnectBlock } from "@/components/folders/ConnectBlock";
import { SnapshotControls } from "@/components/folders/SnapshotControls";
import { StatusBadge } from "@/components/layout/StatusDot";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { useEngine } from "@/hooks/useEngine";
import { runningUnderTauri } from "@/lib/api";
import { relativeTime } from "@/lib/format";

function Stat({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
        {label}
      </span>
      <span className="text-sm">{value}</span>
    </div>
  );
}

export function FolderDetailPage() {
  const { id = "" } = useParams();
  const navigate = useNavigate();
  const { vaults, statuses, setEnabled, setAllowConnections } = useEngine();
  const [confirm, setConfirm] = useState(false);

  const vault = vaults.find((v) => v.id === id);
  const status = statuses[id];

  if (!vault) {
    return (
      <div className="mx-auto max-w-3xl px-8 py-10">
        <Link
          to="/"
          className="inline-flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeft className="h-4 w-4" />
          Folders
        </Link>
        <p className="mt-8 text-muted-foreground">This folder is no longer tracked.</p>
      </div>
    );
  }

  async function reveal() {
    if (!vault || !runningUnderTauri) return;
    const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
    await revealItemInDir(vault.path);
  }

  return (
    <div className="mx-auto max-w-3xl space-y-5 px-8 py-10">
      <Link
        to="/"
        className="inline-flex items-center gap-1.5 rounded-md px-2 py-1 -ml-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
      >
        <ArrowLeft className="h-4 w-4" />
        Folders
      </Link>

      <header className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <div className="flex items-center gap-3">
            <h1 className="truncate text-[1.7rem] font-semibold leading-tight tracking-tight">
              {vault.displayName}
            </h1>
            <StatusBadge state={status?.state ?? "idle"} />
          </div>
          <p className="mt-1.5 truncate font-mono text-xs text-muted-foreground">{vault.path}</p>
        </div>
        <div className="flex shrink-0 gap-2">
          {runningUnderTauri && (
            <Button variant="outline" size="sm" onClick={() => void reveal()}>
              <FolderOpen className="h-4 w-4" />
              Reveal
            </Button>
          )}
          <Button
            variant="outline"
            size="sm"
            onClick={() => setConfirm(true)}
            className="text-destructive hover:text-destructive"
          >
            <Trash2 className="h-4 w-4" />
            Remove
          </Button>
        </div>
      </header>

      <div className="surface grid grid-cols-2 gap-5 p-5 sm:grid-cols-4">
        <Stat
          label="main"
          value={<span className="font-mono">{status?.mainShortSha ?? "·······"}</span>}
        />
        <Stat label="Known" value={status?.knownCount ?? 0} />
        <Stat label="Last commit" value={relativeTime(status?.lastCommit)} />
        <Stat label="Frontier" value={`${status?.frontierCount ?? 0} tip(s)`} />
      </div>

      <div className="surface divide-y divide-border">
        <div className="flex items-center justify-between gap-6 p-5">
          <div>
            <p className="font-medium">Sync</p>
            <p className="text-xs text-muted-foreground">
              Watch this folder and replicate with peers.
            </p>
          </div>
          <Switch
            aria-label="Toggle sync"
            checked={vault.enabled}
            onCheckedChange={(v) => void setEnabled(vault.id, v)}
          />
        </div>
        <div className="flex items-center justify-between gap-6 p-5">
          <div>
            <p className="font-medium">Allow connections</p>
            <p className="text-xs text-muted-foreground">
              Bind a listener so other nodes can connect to this folder.
            </p>
          </div>
          <Switch
            aria-label="Toggle allow connections"
            checked={vault.allowConnections}
            onCheckedChange={(v) => void setAllowConnections(vault.id, v)}
          />
        </div>
      </div>

      {vault.allowConnections && <ConnectBlock vaultId={vault.id} />}
      <AuthorizedPeers vaultId={vault.id} />
      <SnapshotControls vaultId={vault.id} />

      <ConfirmRemoveDialog
        vault={vault}
        open={confirm}
        onOpenChange={(v) => {
          setConfirm(v);
          if (!v && !vaults.find((x) => x.id === id)) navigate("/");
        }}
      />
    </div>
  );
}
