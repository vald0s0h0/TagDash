import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { ATTENTION_PROPAGATE_EVENT } from "@/attention/api";
import { api } from "@/lib/tauri";
import { useLayoutStore } from "@/stores/layoutStore";
import { useUiStore } from "@/stores/uiStore";

/** A ticker sent from the detached « Attention du marché » screen (second click on
 *  it there) opens here through the paths the app already has: the alert card when
 *  a strategy is showing that ticker — so its strategy, its levels and its session
 *  tab come with it — otherwise the manual 5-minute chart of the ticker search. */
export function useAttentionBridge() {
  useEffect(() => {
    let off: (() => void) | undefined;
    listen<string>(ATTENTION_PROPAGATE_EVENT, async (e) => {
      const symbol = e.payload;
      if (!symbol) return;
      const ui = useUiStore.getState();
      const layout = useLayoutStore.getState();
      ui.setSelectedTicker(symbol);
      ui.setActiveView("trading");
      const alerts = await api.getActiveAlerts().catch(() => []);
      const session = useUiStore.getState().activeSession;
      const match =
        alerts.find((a) => a.symbol === symbol && a.session === session) ??
        alerts.find((a) => a.symbol === symbol);
      if (match) {
        layout.placeAlert(match);
        ui.setActiveSession(match.session);
      } else {
        layout.openTickerInZone(symbol, session);
      }
    })
      .then((u) => { off = u; })
      .catch(() => {});
    return () => off?.();
  }, []);
}
