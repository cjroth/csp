import { FolderOpen } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
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

// Clone + watch a remote folder (spec §6.2). One continuous flow: probe →
// create → authorize the bootstrap source → watch. csp-core uses one
// device key + an opaque per-vault id as the equality guard, so there is
// no fresh-NodeId fork to warn about.
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

  function reset() {
    setDest("");
    setUrl("");
  }

  async function browse() {
    const { open: pick } = await import("@tauri-apps/plugin-dialog");
    const dir = await pick({ directory: true, multiple: false });
    if (typeof dir === "string") setDest(dir);
  }

  async function submit() {
    if (!dest.trim() || !url.trim()) return;
    setBusy(true);
    try {
      const v = await cloneRemote(dest.trim(), url.trim());
      toast.success(`Cloned “${v.displayName}” — now watching`);
      reset();
      onOpenChange(false);
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

        <div className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="clone-url">Peer connect address</Label>
            <Input
              id="clone-url"
              value={url}
              placeholder="your-server.example.com"
              onChange={(e) => setUrl(e.target.value)}
            />
            <p className="text-xs text-muted-foreground">
              Scheme and port are optional — a bare domain assumes a secure connection (
              <code>wss://</code> on port 443). Add an explicit
              <code> ws://</code> or <code>:port</code> for a plain or non-standard-port peer (e.g.{" "}
              <code>wss://192.168.1.42:51820</code>).
            </p>
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

        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button onClick={() => void submit()} disabled={busy || !dest.trim() || !url.trim()}>
            Clone &amp; watch
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
