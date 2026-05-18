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
import { runningUnderTauri } from "@/lib/api";

export function AddLocalFolderDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
}) {
  const { addLocalFolder } = useEngine();
  const [path, setPath] = useState("");
  const [busy, setBusy] = useState(false);

  async function browse() {
    if (!runningUnderTauri) {
      toast.info("Native folder picker is available in the desktop build.");
      return;
    }
    const { open: pick } = await import("@tauri-apps/plugin-dialog");
    const dir = await pick({ directory: true, multiple: false });
    if (typeof dir === "string") setPath(dir);
  }

  async function submit() {
    if (!path.trim()) return;
    setBusy(true);
    try {
      const v = await addLocalFolder(path.trim());
      toast.success(
        v.isCspVault
          ? `Attached existing vault “${v.displayName}”`
          : `Created new vault “${v.displayName}”`,
      );
      setPath("");
      onOpenChange(false);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add a local folder</DialogTitle>
          <DialogDescription>
            If the folder already contains a CSP vault it is attached; otherwise a new scoped vault
            is created there. Nothing under <code>.context/</code> is ever modified by the app.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-2">
          <Label htmlFor="local-path">Folder path</Label>
          <div className="flex gap-2">
            <Input
              id="local-path"
              value={path}
              placeholder="/Users/you/Documents/Vault"
              onChange={(e) => setPath(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void submit()}
            />
            <Button type="button" variant="outline" onClick={() => void browse()}>
              <FolderOpen className="h-4 w-4" />
              Browse
            </Button>
          </div>
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button onClick={() => void submit()} disabled={busy || !path.trim()}>
            Add folder
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
