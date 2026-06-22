import { useEffect, useRef } from "react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { StartupPanel } from "@/components/StartupPanel";
import { useStartupStatus } from "@/queries/useStartup";
import { isUpdateInProgress, useUpdaterStore } from "@/stores/updaterStore";

interface Props {
  open: boolean;
  onClose: () => void;
}

export function StartupModal({ open, onClose }: Props) {
  const { data: startup } = useStartupStatus();
  const updateStatus = useUpdaterStore((s) => s.status);

  // Auto-close once every pipeline step has finished — but only on the rising
  // edge of completion *observed while the modal is open and still running*. This
  // way the launch modal (which opens with the pipeline mid-run) closes itself
  // when done, while manually re-opening it after completion keeps it open (no
  // prior "running" state was seen, so there's no transition to fire on).
  // While an auto-update is downloading/installing the modal stays open so its
  // progress stays visible until the app relaunches.
  const sawRunning = useRef(false);
  useEffect(() => {
    if (!open) { sawRunning.current = false; return; }
    if (startup && !startup.completed) sawRunning.current = true;
    if (startup?.completed && sawRunning.current && !isUpdateInProgress(updateStatus)) {
      sawRunning.current = false;
      // Brief pause so the green check-marks are visible before it dismisses.
      const t = setTimeout(onClose, 1200);
      return () => clearTimeout(t);
    }
  }, [open, startup?.completed, updateStatus, onClose]); // eslint-disable-line react-hooks/exhaustive-deps

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
