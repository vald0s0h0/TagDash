// Settings → Comptes & Système → Pipeline de démarrage. Read-only review of the launch
// pipeline at any time — the auto-opening launch screen (StartupModal) is separate and
// unchanged, this just reuses the same panel for on-demand inspection.

import { StartupPanel } from "@/components/StartupPanel";

export function StartupTab() {
  return <StartupPanel />;
}
