import { useEffect, useState } from "react";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useEngine } from "@/hooks/useEngine";
import { api } from "@/lib/api";
import type { Snapshot } from "@/lib/api.types";

// CSP §8 clock-skew warning, shown verbatim and never smoothed.
const SKEW_WARNING =
  "Time-based restore is best-effort: it resolves to the latest snapshot or commit " +
  "at or before the given instant using recorded timestamps, which are subject to " +
  "clock skew across devices. The result may be approximate. Named snapshots are " +
  "exact and skew-free.";

export function RestoreDialog({
  vaultId,
  open,
  onOpenChange,
}: {
  vaultId: string;
  open: boolean;
  onOpenChange: (v: boolean) => void;
}) {
  const { restore } = useEngine();
  const [snaps, setSnaps] = useState<Snapshot[]>([]);
  const [picked, setPicked] = useState<string>("");
  const [when, setWhen] = useState<string>("");

  useEffect(() => {
    if (!open) return;
    api
      .listSnapshots(vaultId)
      .then((s) => {
        setSnaps(s);
        setPicked(s[0]?.name ?? "");
      })
      .catch(() => setSnaps([]));
  }, [open, vaultId]);

  async function doNamed() {
    if (!picked) return;
    await restore(vaultId, { kind: "named", name: picked });
    onOpenChange(false);
  }

  async function doTime() {
    if (!when) return;
    await restore(vaultId, {
      kind: "time",
      rfc3339: new Date(when).toISOString(),
    });
    onOpenChange(false);
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Restore</DialogTitle>
          <DialogDescription>
            Restore is the engine’s restore-as-edit — it converges like any other change. There is
            no rewind.
          </DialogDescription>
        </DialogHeader>

        <Tabs defaultValue="named">
          <TabsList className="grid w-full grid-cols-2">
            <TabsTrigger value="named">Named snapshot</TabsTrigger>
            <TabsTrigger value="time">Point in time</TabsTrigger>
          </TabsList>

          <TabsContent value="named" className="space-y-3 pt-2">
            {snaps.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                No restore points yet. Create one first.
              </p>
            ) : (
              <div className="space-y-2">
                {snaps.map((s) => (
                  <label
                    key={s.name}
                    className="flex cursor-pointer items-center gap-3 rounded-md border p-2.5 text-sm"
                  >
                    <input
                      type="radio"
                      name="snap"
                      checked={picked === s.name}
                      onChange={() => setPicked(s.name)}
                    />
                    <span className="flex-1">{s.name}</span>
                    <span className="text-xs text-muted-foreground">
                      {new Date(s.createdAt).toLocaleString()}
                    </span>
                  </label>
                ))}
              </div>
            )}
            <DialogFooter>
              <Button onClick={() => void doNamed()} disabled={!picked}>
                Restore snapshot
              </Button>
            </DialogFooter>
          </TabsContent>

          <TabsContent value="time" className="space-y-3 pt-2">
            <div className="space-y-2">
              <Label htmlFor="restore-when">Restore to</Label>
              <Input
                id="restore-when"
                type="datetime-local"
                value={when}
                onChange={(e) => setWhen(e.target.value)}
              />
            </div>
            <Alert>
              <AlertDescription>{SKEW_WARNING}</AlertDescription>
            </Alert>
            <DialogFooter>
              <Button onClick={() => void doTime()} disabled={!when}>
                Restore to time
              </Button>
            </DialogFooter>
          </TabsContent>
        </Tabs>
      </DialogContent>
    </Dialog>
  );
}
