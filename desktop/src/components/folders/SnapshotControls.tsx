import { History, Plus, Tag } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { RestoreDialog } from "@/components/dialogs/RestoreDialog";
import { Panel } from "@/components/layout/Panel";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useEngine } from "@/hooks/useEngine";
import { api } from "@/lib/api";
import type { Snapshot } from "@/lib/api.types";

// Recovery (spec §9): snapshot + restore. No conflict UI — CSP resolves
// deterministically; this only exposes restore points.
export function SnapshotControls({ vaultId }: { vaultId: string }) {
  const { createSnapshot } = useEngine();
  const [name, setName] = useState("");
  const [snaps, setSnaps] = useState<Snapshot[]>([]);
  const [restoreOpen, setRestoreOpen] = useState(false);

  const load = useCallback(() => {
    api
      .listSnapshots(vaultId)
      .then(setSnaps)
      .catch(() => setSnaps([]));
  }, [vaultId]);

  useEffect(load, [load]);

  async function create() {
    if (!name.trim()) return;
    await createSnapshot(vaultId, name.trim());
    setName("");
    load();
  }

  return (
    <Panel
      icon={History}
      title="Restore points"
      description="Named snapshots are exact and skew-free."
      action={
        <Button variant="outline" size="sm" onClick={() => setRestoreOpen(true)}>
          <History className="h-4 w-4" />
          Restore…
        </Button>
      }
    >
      <div className="space-y-4">
        <div className="flex gap-2">
          <Input
            value={name}
            placeholder="restore point name"
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void create()}
          />
          <Button variant="outline" onClick={() => void create()} disabled={!name.trim()}>
            <Plus className="h-4 w-4" />
            Create
          </Button>
        </div>

        {snaps.length > 0 && (
          <ul className="space-y-2">
            {snaps.map((s) => (
              <li
                key={s.name}
                className="flex items-center gap-3 rounded-lg border border-border bg-background/40 px-3 py-2.5 text-sm"
              >
                <Tag className="h-3.5 w-3.5 text-muted-foreground" />
                <span className="flex-1 font-medium">{s.name}</span>
                <span className="text-xs text-muted-foreground">
                  {new Date(s.createdAt).toLocaleString()}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>

      <RestoreDialog
        vaultId={vaultId}
        open={restoreOpen}
        onOpenChange={(v) => {
          setRestoreOpen(v);
          if (!v) load();
        }}
      />
    </Panel>
  );
}
