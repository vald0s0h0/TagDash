import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { StartupPanel } from "@/components/StartupPanel";

interface Props {
  open: boolean;
  onClose: () => void;
}

export function StartupModal({ open, onClose }: Props) {
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-2xl max-h-[90vh] overflow-y-auto p-0">
        <DialogHeader className="sr-only">
          <DialogTitle>Startup Pipeline</DialogTitle>
        </DialogHeader>
        <StartupPanel />
      </DialogContent>
    </Dialog>
  );
}
