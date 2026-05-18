import { FolderOpen, TriangleAlert } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
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
import { useEngine } from "@/hooks/useEngine";
import { runningUnderTauri } from "@/lib/api";

// Clone + watch a remote folder (spec §6.2). Clone runs, then the normal
// watch loop starts immediately — one continuous flow. The CSP §5.1
// fresh-NodeId caveat is shown verbatim, never smoothed.
export function CloneRemoteDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
}) {
  const { cloneRemote } = useEngine();
  const [dest, setDest] = useState("");
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [warning, setWarning] = useState<string | null>(null);

  function reset() {
    setDest("");
    setUrl("");
    setWarning(null);
  }

  async function browse() {
    if (!runningUnderTauri) {
      toast.info("Native folder picker is available in the desktop build.");
      return;
    }
    const { open: pick } = await import("@tauri-apps/plugin-dialog");
    const dir = await pick({ directory: true, multiple: false });
    if (typeof dir === "string") setDest(dir);
  }

  async function submit() {
    if (!dest.trim() || !url.trim()) return;
    setBusy(true);
    try {
      const out = await cloneRemote(dest.trim(), url.trim());
      toast.success(`Cloned “${out.vault.displayName}” — now watching`);
      if (out.nodeIdWarning) setWarning(out.nodeIdWarning);
      else {
        reset();
        onOpenChange(false);
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(v) => {
        if (!v) reset();
        onOpenChange(v);
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Connect to a remote folder</DialogTitle>
          <DialogDescription>
            Clone a peer’s folder to a local destination and keep it synced. The clone catches up,
            materialises the working tree, then watches like any other folder.
          </DialogDescription>
        </DialogHeader>

        {warning ? (
          <Alert variant="destructive">
            <TriangleAlert className="h-4 w-4" />
            <AlertTitle>Fresh NodeId (CSP §5.1)</AlertTitle>
            <AlertDescription>{warning}</AlertDescription>
          </Alert>
        ) : (
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="clone-url">Peer connect address</Label>
              <Input
                id="clone-url"
                value={url}
                placeholder="wss://192.168.1.42:51820"
                onChange={(e) => setUrl(e.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="clone-dest">Local destination folder</Label>
              <div className="flex gap-2">
                <Input
                  id="clone-dest"
                  value={dest}
                  placeholder="/Users/you/Documents/Synced"
                  onChange={(e) => setDest(e.target.value)}
                />
                <Button type="button" variant="outline" onClick={() => void browse()}>
                  <FolderOpen className="h-4 w-4" />
                  Browse
                </Button>
              </div>
            </div>
          </div>
        )}

        <DialogFooter>
          {warning ? (
            <Button
              onClick={() => {
                reset();
                onOpenChange(false);
              }}
            >
              Got it
            </Button>
          ) : (
            <>
              <Button variant="ghost" onClick={() => onOpenChange(false)}>
                Cancel
              </Button>
              <Button onClick={() => void submit()} disabled={busy || !dest.trim() || !url.trim()}>
                Clone &amp; watch
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
