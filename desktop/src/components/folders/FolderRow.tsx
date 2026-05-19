import {
  ChevronRight,
  FolderOpen,
  GitCommitHorizontal,
  Layers,
  MoreVertical,
  Trash2,
} from "lucide-react";
import { useState } from "react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { ConfirmRemoveDialog } from "@/components/dialogs/ConfirmRemoveDialog";
import { StatusDot } from "@/components/layout/StatusDot";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Switch } from "@/components/ui/switch";
import { useEngine } from "@/hooks/useEngine";
import { runningUnderTauri } from "@/lib/api";
import type { Vault } from "@/lib/api.types";
import { relativeTime } from "@/lib/format";
import { cn } from "@/lib/utils";

function Toggle({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <div className="flex flex-col items-center gap-1.5">
      <span className="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
        {label}
      </span>
      <Switch aria-label={label} checked={checked} onCheckedChange={onChange} />
    </div>
  );
}

export function FolderRow({ vault }: { vault: Vault }) {
  const { statuses, setEnabled, setAllowConnections } = useEngine();
  const navigate = useNavigate();
  const [confirm, setConfirm] = useState(false);
  const status = statuses[vault.id];

  async function reveal() {
    if (!runningUnderTauri) {
      toast.info(vault.path, { description: "Reveal works in the desktop build." });
      return;
    }
    const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
    await revealItemInDir(vault.path);
  }

  return (
    <div className="surface surface-hover group flex items-center gap-4 p-4">
      <button
        type="button"
        className="flex min-w-0 flex-1 items-center gap-4 text-left"
        onClick={() => navigate(`/folders/${vault.id}`)}
      >
        <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border border-border bg-background/40">
          <StatusDot state={status?.state ?? "idle"} />
        </div>

        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="truncate font-medium">{vault.displayName}</span>
            <span
              className={cn(
                "inline-flex items-center gap-1 rounded-md border border-border bg-muted/60 px-1.5 py-0.5",
                "font-mono text-[10px] text-muted-foreground",
              )}
            >
              <GitCommitHorizontal className="h-3 w-3" />
              {status?.mainShortSha ?? "·······"}
            </span>
          </div>
          <p className="mt-0.5 truncate text-xs text-muted-foreground">{vault.path}</p>
        </div>

        <div className="hidden items-center gap-5 text-xs text-muted-foreground md:flex">
          <span className="inline-flex items-center gap-1.5" title="Known primitives">
            <Layers className="h-3.5 w-3.5" />
            {status?.knownCount ?? 0}
          </span>
          <span className="w-24 text-right tabular-nums">{relativeTime(status?.lastCommit)}</span>
        </div>

        <ChevronRight className="h-4 w-4 shrink-0 text-muted-foreground transition-transform group-hover:translate-x-0.5 group-hover:text-foreground" />
      </button>

      <div className="flex items-center gap-5 border-l border-border pl-4">
        <Toggle
          label="Sync"
          checked={vault.enabled}
          onChange={(v) => void setEnabled(vault.id, v)}
        />
        <Toggle
          label="Serve"
          checked={vault.allowConnections}
          onChange={(v) => void setAllowConnections(vault.id, v)}
        />

        <DropdownMenu>
          <DropdownMenuTrigger className="rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground">
            <MoreVertical className="h-4 w-4" />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem onClick={() => void reveal()}>
              <FolderOpen className="h-4 w-4" />
              Open in file manager
            </DropdownMenuItem>
            <DropdownMenuItem variant="destructive" onClick={() => setConfirm(true)}>
              <Trash2 className="h-4 w-4" />
              Remove folder
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      <ConfirmRemoveDialog vault={vault} open={confirm} onOpenChange={setConfirm} />
    </div>
  );
}
