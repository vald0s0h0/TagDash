// Shown after the data-source mode (API ↔ Flat files) is changed. The startup
// pipeline differs between the two modes (flat-files mode skips Alpaca, loads the
// daily history from disk and parks the latest day in Market Replay), so the app
// must restart to apply the new sequence. Offers an immediate relaunch or "later".

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
          Le changement de source de données modifie la séquence de démarrage
          (startup pipeline). Redémarre TagDash pour l'appliquer — en mode flat
          files les flux temps réel sont remplacés par le Market Replay hors-ligne.
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
