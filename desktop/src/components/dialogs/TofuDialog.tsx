import { ShieldQuestion } from "lucide-react";
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { useEngine } from "@/hooks/useEngine";

// Trust-on-first-use, surfaced natively (spec §8.3). CSP's TOFU window must
// never be silent in a GUI — empty authorized set + first connector.
export function TofuDialog() {
  const { pendingTofu, respondTofu, vaults } = useEngine();
  const open = pendingTofu !== null;
  const vaultName =
    vaults.find((v) => v.id === pendingTofu?.vaultId)?.displayName ?? pendingTofu?.vaultId ?? "";

  return (
    <AlertDialog open={open}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle className="flex items-center gap-2">
            <ShieldQuestion className="h-5 w-5 text-amber-500" />A new peer wants to connect
          </AlertDialogTitle>
          <AlertDialogDescription asChild>
            <div className="space-y-3 pt-1 text-sm">
              <p>
                This folder’s authorized set is empty. The first peer you allow is trusted from now
                on (CSP trust-on-first-use). Only allow it if you recognise this device.
              </p>
              <dl className="rounded-md border bg-muted/40 p-3 font-mono text-xs">
                <div className="flex justify-between gap-4">
                  <dt className="text-muted-foreground">Folder</dt>
                  <dd className="truncate">{vaultName}</dd>
                </div>
                <div className="mt-1 flex justify-between gap-4">
                  <dt className="text-muted-foreground">Address</dt>
                  <dd className="truncate">{pendingTofu?.address}</dd>
                </div>
                <div className="mt-1 flex justify-between gap-4">
                  <dt className="text-muted-foreground">Key</dt>
                  <dd className="truncate" title={pendingTofu?.peerFingerprint}>
                    {pendingTofu?.peerFingerprint}
                  </dd>
                </div>
              </dl>
            </div>
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <Button
            variant="outline"
            onClick={() => pendingTofu && void respondTofu(pendingTofu.requestId, false)}
          >
            Deny
          </Button>
          <Button onClick={() => pendingTofu && void respondTofu(pendingTofu.requestId, true)}>
            Allow this peer
          </Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
