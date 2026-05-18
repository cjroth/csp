import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { useEngine } from "@/hooks/useEngine";
import type { Vault } from "@/lib/api.types";

// Honest confirm copy (spec §6.2): stops syncing + removes app tracking;
// does NOT delete the folder, the working files, or the vault's history.
export function ConfirmRemoveDialog({
  vault,
  open,
  onOpenChange,
}: {
  vault: Vault;
  open: boolean;
  onOpenChange: (v: boolean) => void;
}) {
  const { removeVault } = useEngine();
  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Remove “{vault.displayName}”?</AlertDialogTitle>
          <AlertDialogDescription>
            This stops syncing and removes the folder from Context Desktop. It does
            <strong> not</strong> delete the folder, your files, or the vault’s{" "}
            <code>.context/</code> history — it stays a valid CSP vault and can be re-added later.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Keep folder</AlertDialogCancel>
          <AlertDialogAction
            onClick={() => void removeVault(vault.id)}
            className="bg-destructive text-white hover:bg-destructive/90"
          >
            Stop &amp; remove
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
