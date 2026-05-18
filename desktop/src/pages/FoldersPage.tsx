import { FolderPlus, Link2, Plus } from "lucide-react";
import { useEffect, useState } from "react";
import { AddLocalFolderDialog } from "@/components/dialogs/AddLocalFolderDialog";
import { CloneRemoteDialog } from "@/components/dialogs/CloneRemoteDialog";
import { FolderRow } from "@/components/folders/FolderRow";
import { PageHeader } from "@/components/layout/PageHeader";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import { useEngine } from "@/hooks/useEngine";

export function FoldersPage() {
  const { vaults, loading } = useEngine();
  const [addLocal, setAddLocal] = useState(false);
  const [clone, setClone] = useState(false);

  // Tray "Add folder…" / "Connect to remote folder…" bridge (spec §6.1).
  useEffect(() => {
    const a = () => setAddLocal(true);
    const c = () => setClone(true);
    window.addEventListener("ctx:add-local", a);
    window.addEventListener("ctx:connect-remote", c);
    return () => {
      window.removeEventListener("ctx:add-local", a);
      window.removeEventListener("ctx:connect-remote", c);
    };
  }, []);

  return (
    <div className="mx-auto max-w-3xl px-8 py-10">
      <PageHeader
        title="Folders"
        subtitle="Each folder is one CSP vault, kept byte-identical across your devices."
        actions={
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button className="shadow-lg shadow-primary/20">
                <Plus className="h-4 w-4" />
                Add folder
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-60">
              <DropdownMenuItem onClick={() => setAddLocal(true)}>
                <FolderPlus className="h-4 w-4" />
                Add a local folder…
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => setClone(true)}>
                <Link2 className="h-4 w-4" />
                Connect to a remote folder…
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        }
      />

      <div className="space-y-3">
        {loading ? (
          [0, 1, 2].map((i) => <Skeleton key={i} className="h-[74px] w-full rounded-xl" />)
        ) : vaults.length === 0 ? (
          <div className="surface flex flex-col items-center justify-center gap-2 px-6 py-16 text-center">
            <div className="flex h-12 w-12 items-center justify-center rounded-2xl bg-gradient-to-br from-violet-500/20 to-indigo-600/20 ring-1 ring-primary/20">
              <FolderPlus className="h-6 w-6 text-primary" />
            </div>
            <p className="mt-1 font-medium">No folders yet</p>
            <p className="max-w-xs text-sm text-muted-foreground">
              Add a local folder or clone a remote one to start syncing across your devices.
            </p>
            <Button variant="outline" className="mt-3" onClick={() => setAddLocal(true)}>
              <Plus className="h-4 w-4" />
              Add your first folder
            </Button>
          </div>
        ) : (
          vaults.map((v) => <FolderRow key={v.id} vault={v} />)
        )}
      </div>

      <AddLocalFolderDialog open={addLocal} onOpenChange={setAddLocal} />
      <CloneRemoteDialog open={clone} onOpenChange={setClone} />
    </div>
  );
}
