// Shown after the offline switch is toggled. Chart and history reads pick the new
// policy up on their next call, but the live feed and the startup pipeline both ran
// at launch — so applying it fully (reconnecting the feed, re-running the network
// steps) needs a restart. Offers an immediate relaunch or "later".

import { RefreshCw } from "lucide-react";
import { relaunch } from "@tauri-apps/plugin-process";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";

interface Props {
  open: boolean;
  /** Dismiss without restarting ("Plus tard"). */
  onClose: () => void;
}

export function RestartRequiredDialog({ open, onClose }: Props) {
  const restartNow = async () => {
    try {
      await relaunch();
    } catch {
      // relaunch is unavailable in some dev contexts — close so the user can
      // restart manually rather than leaving a dead button.
      onClose();
    }
  };

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <RefreshCw className="h-4 w-4" />
            Redémarrage requis
          </DialogTitle>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">
          Les graphiques appliquent la nouvelle politique immédiatement, mais le flux
          temps réel et le pipeline de démarrage ont déjà tourné au lancement.
          Redémarre TagDash pour reconnecter le flux (ou le couper) et relancer les
          étapes réseau.
        </p>
        <div className="mt-2 flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onClose}>
            Plus tard
          </Button>
          <Button size="sm" onClick={restartNow}>
            <RefreshCw className="mr-1.5 h-3.5 w-3.5" /> Redémarrer maintenant
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
