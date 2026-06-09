import { create } from "zustand";
import type { Session, AlertSignal, ZoneAssignment, LayoutTab } from "@/types";

const ZONES_PER_TAB: Record<Session, number> = {
  premarket:  1,
  pre_open:   1,
  open:       4,
  afterhours: 1,
};

const SESSION_LABELS: Record<Session, string> = {
  premarket:  "Premarket",
  pre_open:   "Pre-open",
  open:       "Open",
  afterhours: "Afterhours",
};

const SESSIONS: Session[] = ["premarket", "pre_open", "open", "afterhours"];

function makeZone(tab_id: string, index: number): ZoneAssignment {
  return {
    zone_id:       `${tab_id}-zone-${index}`,
    symbol:        null,
    alert_id:      null,
    strategy_id:   null,
    strategy_name: null,
    priority:      null,
    reason:        null,
    price:         null,
    placed_at:     null,
    llm_status:    null,
    llm_summary:   null,
    display_timeframe: null,
    side:          null,
  };
}

function makeTab(session: Session, tabIndex: number): LayoutTab {
  const tab_id    = `${session}-${tabIndex}`;
  const zoneCount = ZONES_PER_TAB[session];
  const suffix    = tabIndex === 0 ? "" : ` ${tabIndex + 1}`;
  return {
    tab_id,
    session,
    label: `${SESSION_LABELS[session]}${suffix}`,
    zones: Array.from({ length: zoneCount }, (_, i) => makeZone(tab_id, i)),
  };
}

type ZoneContent = Omit<ZoneAssignment, "zone_id">;

function alertToContent(alert: AlertSignal): ZoneContent {
  return {
    symbol:        alert.symbol,
    alert_id:      alert.alert_id,
    strategy_id:   alert.strategy_id,
    strategy_name: alert.strategy_name,
    priority:      alert.priority,
    reason:        alert.reason,
    price:         alert.price,
    placed_at:     new Date().toISOString(),
    llm_status:    null,
    llm_summary:   null,
    display_timeframe: alert.display_timeframe,
    side:          alert.side,
  };
}

function applyContent(zone: ZoneAssignment, content: ZoneContent) {
  zone.symbol        = content.symbol;
  zone.alert_id      = content.alert_id;
  zone.strategy_id   = content.strategy_id;
  zone.strategy_name = content.strategy_name;
  zone.priority      = content.priority;
  zone.reason        = content.reason;
  zone.price         = content.price;
  zone.placed_at     = content.placed_at;
  zone.llm_status    = content.llm_status;
  zone.llm_summary   = content.llm_summary;
  zone.display_timeframe = content.display_timeframe;
  zone.side          = content.side;
}

function clearContent(zone: ZoneAssignment) {
  zone.symbol = zone.alert_id = zone.strategy_id = zone.strategy_name = null;
  zone.priority = zone.reason = zone.price = zone.placed_at = null;
  zone.llm_status = zone.llm_summary = null;
  zone.display_timeframe = zone.side = null;
}


function deepCloneTabs(tabs: Record<Session, LayoutTab[]>): Record<Session, LayoutTab[]> {
  return JSON.parse(JSON.stringify(tabs)) as Record<Session, LayoutTab[]>;
}

// ─── Store ────────────────────────────────────────────────────────────────────

interface LayoutState {
  tabs:        Record<Session, LayoutTab[]>;
  activeTabId: Record<Session, string>;

  // Auto-placement: finds first empty zone; creates a new tab if all full.
  placeAlert: (alert: AlertSignal) => void;

  // Explicit drag from scanner → specific zone.
  placeAlertInZone: (alert: AlertSignal, zone_id: string) => void;

  // Open a ticker in the active tab's first zone (replacing its content).
  // Used by the pre-open screener: clicking a ticker inspects it in the single
  // pre-open chart panel rather than spawning new tabs.
  openInActiveZone: (alert: AlertSignal) => void;

  // Release (Libérer button): clears the zone, auto-removes empty non-first tabs.
  releaseZone: (zone_id: string) => void;

  setActiveTab: (session: Session, tab_id: string) => void;
}

export const useLayoutStore = create<LayoutState>((set) => ({
  tabs: Object.fromEntries(
    SESSIONS.map((s) => [s, [makeTab(s, 0)]])
  ) as Record<Session, LayoutTab[]>,

  activeTabId: Object.fromEntries(
    SESSIONS.map((s) => [s, `${s}-0`])
  ) as Record<Session, string>,

  // ── placeAlert ──────────────────────────────────────────────────────────────
  placeAlert(alert) {
    set((state) => {
      const session     = alert.session;
      const tabs        = deepCloneTabs(state.tabs);
      const sessionTabs = tabs[session];

      // Anti-spam: symbol already displayed in this session → skip
      for (const tab of sessionTabs) {
        if (tab.zones.some((z) => z.symbol === alert.symbol)) return {};
      }

      // Find first empty zone across existing tabs
      for (const tab of sessionTabs) {
        const empty = tab.zones.find((z) => z.symbol === null);
        if (empty) {
          applyContent(empty, alertToContent(alert));
          return {
            tabs,
            activeTabId: { ...state.activeTabId, [session]: tab.tab_id },
          };
        }
      }

      // All zones full → create a new tab
      const newTab = makeTab(session, sessionTabs.length);
      applyContent(newTab.zones[0], alertToContent(alert));
      sessionTabs.push(newTab);
      return {
        tabs,
        activeTabId: { ...state.activeTabId, [session]: newTab.tab_id },
      };
    });
  },

  // ── placeAlertInZone ────────────────────────────────────────────────────────
  placeAlertInZone(alert, zone_id) {
    set((state) => {
      const tabs = deepCloneTabs(state.tabs);
      for (const session of SESSIONS) {
        for (const tab of tabs[session]) {
          const zone = tab.zones.find((z) => z.zone_id === zone_id);
          if (zone) {
            applyContent(zone, alertToContent(alert));
            return { tabs };
          }
        }
      }
      return {};
    });
  },

  // ── openInActiveZone ──────────────────────────────────────────────────────────
  openInActiveZone(alert) {
    set((state) => {
      const session = alert.session;
      const tabs    = deepCloneTabs(state.tabs);
      const activeId = state.activeTabId[session];
      const tab = tabs[session].find((t) => t.tab_id === activeId) ?? tabs[session][0];
      if (!tab || tab.zones.length === 0) return {};
      applyContent(tab.zones[0], alertToContent(alert));
      return {
        tabs,
        activeTabId: { ...state.activeTabId, [session]: tab.tab_id },
      };
    });
  },

  // ── releaseZone ─────────────────────────────────────────────────────────────
  releaseZone(zone_id) {
    set((state) => {
      const tabs = deepCloneTabs(state.tabs);
      for (const session of SESSIONS) {
        for (const tab of tabs[session]) {
          const zone = tab.zones.find((z) => z.zone_id === zone_id);
          if (zone) {
            clearContent(zone);
            // Remove empty non-first tabs
            tabs[session] = tabs[session].filter(
              (t, i) => i === 0 || t.zones.some((z) => z.symbol !== null)
            );
            const active      = state.activeTabId[session];
            const stillExists = tabs[session].some((t) => t.tab_id === active);
            const newActive   = stillExists ? active : tabs[session][0].tab_id;
            return {
              tabs,
              activeTabId: { ...state.activeTabId, [session]: newActive },
            };
          }
        }
      }
      return {};
    });
  },

  // ── setActiveTab ─────────────────────────────────────────────────────────────
  setActiveTab(session, tab_id) {
    set((state) => ({
      activeTabId: { ...state.activeTabId, [session]: tab_id },
    }));
  },
}));
