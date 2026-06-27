import { useEffect, useRef } from "react";
import { useQuery } from "@tanstack/react-query";
import { useActiveAlerts, useScreenerMatches } from "@/queries/useScanner";
import { useUiStore } from "@/stores/uiStore";
import { useLayoutStore } from "@/stores/layoutStore";
import { useAlertStatusStore } from "@/stores/alertStatusStore";
import { useChartStore } from "@/stores/chartStore";
import { useGamepadStore, GAMEPAD_ACTIONS, type GamepadActionId } from "@/stores/gamepadStore";
import { useGamepadUiStore } from "@/stores/gamepadUiStore";
import { getZoneGamepad, getChartControl, isGamepadCapturing } from "@/lib/gamepadBus";
import { axisSpeed, buttonHeld, resolveAction, isBoundToButton, pickGamepad } from "@/lib/gamepadActions";
import { matchToAlert } from "@/components/ScreenerPanel";
import { api } from "@/lib/tauri";
import type { AlertSignal, ScreenerMatch, Session } from "@/types";

// Global Xbox-controller loop. Mounted once (App). A requestAnimationFrame poll
// reads navigator.getGamepads() every frame: button edges fire the digital
// commands (through the R2 layer) and the sticks drive the focused chart's
// zoom/cursor continuously. Everything acts on the ACTIVE session's chart zone —
// the same target the alert sidebar drives. Settings/Journal dialogs and the
// binding recorder suppress the loop so it never double-fires.

// Trading sessions the L1/L2 tab navigation cycles (wrap-around).
const TRADING_SESSIONS: Session[] = ["premarket", "pre_open", "open"];
// Window within which a second "remove orders" press means "+ alarms too".
const DOUBLE_TAP_MS = 350;

/** Short rumble; no-op when the pad / browser doesn't support haptics. */
function vibrate(pad: Gamepad | null, ms: number, strong: number, weak: number): void {
  const act = (pad as unknown as { vibrationActuator?: { playEffect?: (t: string, p: object) => Promise<unknown> } })
    ?.vibrationActuator;
  act?.playEffect?.("dual-rumble", {
    duration: ms, startDelay: 0, strongMagnitude: strong, weakMagnitude: weak,
  }).catch(() => {});
}

/** A Radix dialog (Settings, Journal, …) sits over the chart → suppress the loop.
 *  Our own controller overlays (tag picker, flash, armed) aren't role=dialog, so
 *  they don't trip this and the pad keeps driving them. */
function aRadixDialogOpen(): boolean {
  return !!document.querySelector('[role="dialog"][data-state="open"]');
}

export function useGamepad(): void {
  const padRef = useRef<Gamepad | null>(null);

  // Latest navigation data, mirrored into a ref the (non-React-timed) loop reads.
  const alertsQuery = useActiveAlerts();
  const screener    = useScreenerMatches().data ?? [];
  const alerts      = alertsQuery.data ?? [];
  const navRef = useRef<{ alerts: AlertSignal[]; screener: ScreenerMatch[] }>({ alerts, screener });
  navRef.current = { alerts, screener };

  // ── Poll loop + dispatch ───────────────────────────────────────────────────
  useEffect(() => {
    let raf = 0;
    let prev: boolean[] = [];
    let prevR2 = false;
    let lastRemove = 0;

    // The active session's chart zone id (only when it shows a ticker).
    const activeZoneId = (): string | null => {
      const session = useUiStore.getState().activeSession;
      const z = useLayoutStore.getState().tabs[session]?.[0]?.zones[0];
      return z?.symbol ? z.zone_id : null;
    };

    // List index step (clamped, no wrap); jumps to an end when off-list.
    const stepIndex = (idx: number, dir: 1 | -1, len: number): number =>
      idx < 0 ? (dir === 1 ? 0 : len - 1) : Math.min(len - 1, Math.max(0, idx + dir));

    const navTicker = (dir: 1 | -1): void => {
      const session = useUiStore.getState().activeSession;
      const layout = useLayoutStore.getState();
      const current = layout.tabs[session]?.[0]?.zones[0]?.symbol ?? null;
      if (session === "pre_open") {
        const dismissed = useUiStore.getState().dismissedScreener;
        const list = navRef.current.screener.filter((m) => !dismissed.includes(m.symbol));
        if (list.length === 0) return;
        const m = list[stepIndex(list.findIndex((x) => x.symbol === current), dir, list.length)];
        useUiStore.getState().setSelectedTicker(m.symbol);
        layout.openInActiveZone(matchToAlert(m));
      } else {
        const released = useAlertStatusStore.getState().released;
        const list = navRef.current.alerts.filter((a) => a.session === session && !released.has(a.symbol));
        if (list.length === 0) return;
        const a = list[stepIndex(list.findIndex((x) => x.symbol === current), dir, list.length)];
        useUiStore.getState().setSelectedTicker(a.symbol);
        layout.placeAlert(a);
      }
    };

    const tabNav = (dir: 1 | -1): void => {
      const ui = useUiStore.getState();
      const cur = TRADING_SESSIONS.indexOf(ui.activeSession);
      const next = ((cur < 0 ? 0 : cur) + dir + TRADING_SESSIONS.length) % TRADING_SESSIONS.length;
      ui.setActiveSession(TRADING_SESSIONS[next]);
      ui.setActiveView("trading");
    };

    const releaseActive = (): void => {
      const session = useUiStore.getState().activeSession;
      const layout = useLayoutStore.getState();
      const zone = layout.tabs[session]?.[0]?.zones[0];
      if (!zone?.symbol) return;
      if (session === "pre_open") {
        const sym = zone.symbol;
        useUiStore.getState().dismissScreener(sym);
        const dismissed = [...useUiStore.getState().dismissedScreener, sym];
        const next = navRef.current.screener.find((m) => !dismissed.includes(m.symbol));
        if (next) {
          useUiStore.getState().setSelectedTicker(next.symbol);
          layout.openInActiveZone(matchToAlert(next));
        }
      } else {
        useChartStore.getState().clearZone(zone.zone_id);
        layout.releaseZone(zone.zone_id); // Sidebar effect auto-opens the next pending alert
        api.clearZoneContext(zone.zone_id).catch(() => {});
      }
    };

    const shareTag = (): void => {
      const zone = getZoneGamepad(activeZoneId());
      const tradeId = zone?.tradeId() ?? null;
      const symbol  = zone?.symbol() ?? null;
      if (!tradeId || !symbol) {
        useGamepadUiStore.getState().setFlashError("Aucun trade actif pour ajouter un tag");
        return;
      }
      useGamepadUiStore.getState().openTagPicker(tradeId, symbol);
    };

    const fire = (id: GamepadActionId): void => {
      const zone = getZoneGamepad(activeZoneId());
      switch (id) {
        case "ticker_prev":    navTicker(-1); break;
        case "ticker_next":    navTicker(1);  break;
        case "ticker_release": releaseActive(); break;
        case "tab_prev":       tabNav(-1); break;
        case "tab_next":       tabNav(1);  break;
        case "focus_cycle":    zone?.cycleFocus(); break;
        case "cursor_sl":      zone?.placeSl(); break;
        case "cursor_alarm":   zone?.placeAlarm(); break;
        case "cursor_tp":      zone?.placeTp(); break;
        case "remove_orders": {
          const now = performance.now();
          if (now - lastRemove < DOUBLE_TAP_MS) { zone?.removeOrdersAndAlarms(); lastRemove = 0; }
          else { zone?.removeOrders(); lastRemove = now; }
          break;
        }
        case "order_25":  zone?.order(25);  break;
        case "order_50":  zone?.order(50);  break;
        case "order_100": zone?.order(100); break;
        case "close_position": zone?.close(); break;
        case "confirm_hod": zone?.confirmHod?.(); break;
        case "capture": if (zone?.hasTradeId()) zone.capture(); break;
        case "share_tag": shareTag(); break;
        case "journal_audio": if (zone?.hasTradeId()) zone.journalAudio(); break;
        case "r2_modifier":   break; // handled as a held modifier, not an edge
      }
    };

    const handleAxes = (pad: Gamepad): void => {
      const zone = getZoneGamepad(activeZoneId());
      if (!zone) return;
      const ctl = getChartControl(zone.getFocusedPaneId());
      if (!ctl) return;
      const st = useGamepadStore.getState();

      // W3C Gamepad API axis convention: right = +1, **up = −1** (down is positive).
      const zt = st.bindings.zoom_time;
      if (zt?.kind === "axis") {
        let v = axisSpeed(pad.axes[zt.index] ?? 0, st.leftSensitivity);
        if (st.invertZoomTime) v = -v;
        if (v !== 0) ctl.zoomTime(1 + 0.022 * v); // stick right (+) → zoom in
      }
      const zp = st.bindings.zoom_price;
      if (zp?.kind === "axis") {
        let v = axisSpeed(pad.axes[zp.index] ?? 0, st.leftSensitivity);
        if (st.invertZoomPrice) v = -v;
        if (v !== 0) ctl.zoomPrice(0.01 * v); // stick up (−) → tighter margins → zoom in
      }
      const cm = st.bindings.cursor_move;
      if (cm?.kind === "axis") {
        let v = axisSpeed(pad.axes[cm.index] ?? 0, st.rightSensitivity);
        if (st.invertCursor) v = -v;
        if (v !== 0) ctl.nudgeCursor(-0.018 * v); // stick up (−) → cursor up (higher price)
      }
    };

    const poll = (): void => {
      // The whole body is guarded: a single thrown action must NOT kill the rAF
      // chain (that would silently stop ALL controller input while the pad still
      // reads as connected). We always reschedule below, no matter what.
      try {
        const pads = navigator.getGamepads ? navigator.getGamepads() : [];
        const pad = pickGamepad(pads); // the actual gamepad, not a SpaceMouse / other HID
        padRef.current = pad;

        const st = useGamepadStore.getState();
        const blocked = !pad || !st.enabled || isGamepadCapturing()
          || useUiStore.getState().activeView !== "trading" || aRadixDialogOpen();

        if (pad && !blocked) {
          const r2Held = buttonHeld(pad, st.bindings.r2_modifier);
          if (r2Held !== prevR2) { useGamepadUiStore.getState().setArmed(r2Held); prevR2 = r2Held; }

          const picker = useGamepadUiStore.getState().tag;
          for (let i = 0; i < pad.buttons.length; i++) {
            if (!(pad.buttons[i].pressed && !prev[i])) continue; // edge only
            if (picker) {
              // While the auto-tag picker is open, only Share matters (advance it).
              if (isBoundToButton(st.bindings, "share_tag", i)) useGamepadUiStore.getState().bumpTagAdvance();
              continue;
            }
            const action = resolveAction(st.bindings, i, r2Held);
            if (action) { try { fire(action); } catch (e) { console.error("gamepad action error:", action, e); } }
          }
          if (!picker) handleAxes(pad);
        } else if (prevR2) {
          useGamepadUiStore.getState().setArmed(false);
          prevR2 = false;
        }

        prev = pad ? pad.buttons.map((b) => b.pressed) : [];
      } catch (e) {
        console.error("gamepad poll error:", e);
      }
      raf = requestAnimationFrame(poll); // always reschedule — loop must never die
    };

    raf = requestAnimationFrame(poll);
    return () => cancelAnimationFrame(raf);
  }, []);

  // ── Haptics: new ticker · trade open · trade close (even out of view) ───────
  // Diffs the already-polled alert/position lists (global, so a close fires even
  // when the ticker isn't on screen). A null baseline on first load avoids buzzing
  // for everything already present.
  const seenAlerts = useRef<Set<string> | null>(null);
  useEffect(() => {
    const data = alertsQuery.data;
    if (!data) return; // not loaded yet → no baseline, no buzz
    const ids = new Set(data.map((a) => a.alert_id));
    if (seenAlerts.current === null) { seenAlerts.current = ids; return; } // baseline = first load
    const isNew = [...ids].some((id) => !seenAlerts.current!.has(id));
    seenAlerts.current = ids;
    if (isNew) vibrate(padRef.current, 35, 0.5, 0.3); // shortest — new ticker
  }, [alertsQuery.data]);

  const posQuery = useQuery({
    queryKey: ["internal_positions"],
    queryFn:  () => api.getInternalPositions(),
    refetchInterval: 1000,
  });
  const prevOpen = useRef<Set<string> | null>(null);
  useEffect(() => {
    const data = posQuery.data;
    if (!data) return;
    const open = new Set(data.map((p) => p.symbol));
    if (prevOpen.current === null) { prevOpen.current = open; return; } // baseline = first load
    const opened = [...open].some((s) => !prevOpen.current!.has(s));
    const closed = [...prevOpen.current].some((s) => !open.has(s));
    prevOpen.current = open;
    if (opened) vibrate(padRef.current, 80, 0.85, 0.5);            // trade opened
    if (closed) {                                                  // trade closed — double pulse
      vibrate(padRef.current, 60, 0.7, 0.5);
      setTimeout(() => vibrate(padRef.current, 60, 0.7, 0.5), 110);
    }
  }, [posQuery.data]);
}
