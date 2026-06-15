import { useEffect } from "react";
import {
  useHotkeyStore, bindingFromEvent, bindingsEqual,
  getHoveredZone, setHoveredZone, dispatchToZone, isRecordingActive,
  type Binding, type HotkeyActionId,
} from "@/stores/hotkeyStore";
import { useLayoutStore } from "@/stores/layoutStore";
import { useUiStore } from "@/stores/uiStore";

// Global hotkey listener. Mounted once (App). Routes a matched chord to the zone
// under the mouse cursor (its registered ChartZone dispatcher), falling back to
// the active tab's first zone when the cursor isn't over a chart. Typing in an
// input/textarea is never hijacked.

/** Don't fire chart hotkeys while the user is typing in a field. */
function isTypingTarget(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el) return false;
  if (el.isContentEditable) return true;
  const tag = el.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

/** Don't fire chart hotkeys while a modal (Settings, Journal, …) is open — the
 *  chart it would target sits behind the dialog. Radix dialogs mark their content
 *  with role="dialog" + data-state="open". */
function aDialogIsOpen(): boolean {
  return !!document.querySelector('[role="dialog"][data-state="open"]');
}

function suppressed(target: EventTarget | null): boolean {
  return isRecordingActive() || aDialogIsOpen() || isTypingTarget(target);
}

export function useHotkeys(): void {
  useEffect(() => {
    function matchAction(b: Binding): HotkeyActionId | null {
      const { bindings } = useHotkeyStore.getState();
      for (const id of Object.keys(bindings) as HotkeyActionId[]) {
        const bb = bindings[id];
        if (bb && bindingsEqual(bb, b)) return id;
      }
      return null;
    }

    function targetZone(eventEl: EventTarget | null): string | null {
      // Prefer the zone under the event (most accurate for mouse buttons), then
      // the last-hovered zone (for keyboard), then the active tab's first zone.
      const fromEvent = (eventEl as HTMLElement | null)?.closest?.("[data-zone-id]");
      const hovered = fromEvent?.getAttribute("data-zone-id") ?? getHoveredZone();
      if (hovered) return hovered;
      const { activeSession } = useUiStore.getState();
      const { tabs, activeTabId } = useLayoutStore.getState();
      const sessionTabs = tabs[activeSession] ?? [];
      const tab = sessionTabs.find((t) => t.tab_id === activeTabId[activeSession]) ?? sessionTabs[0];
      return tab?.zones[0]?.zone_id ?? null;
    }

    function handle(e: KeyboardEvent | MouseEvent): void {
      if (suppressed(e.target)) return;
      const b = bindingFromEvent(e);
      if (!b) return;
      const id = matchAction(b);
      if (!id) return;
      const zone = targetZone(e.target);
      if (!zone) return;
      e.preventDefault();
      e.stopPropagation();
      dispatchToZone(zone, id);
    }

    // Track the hovered zone for keyboard hotkeys (mouse hotkeys resolve from the
    // event target directly). closest() returns null over the rail/sidebar, which
    // clears the hover so a keyboard hotkey then falls back to the active zone.
    function onMouseOver(e: MouseEvent): void {
      const el = (e.target as HTMLElement | null)?.closest?.("[data-zone-id]");
      setHoveredZone(el?.getAttribute("data-zone-id") ?? null);
    }

    // Suppress browser back/forward navigation on the extra mouse buttons we use.
    function onAuxClick(e: MouseEvent): void {
      if (suppressed(e.target)) return;
      const b = bindingFromEvent(e);
      if (b && matchAction(b)) e.preventDefault();
    }

    window.addEventListener("keydown", handle, true);
    window.addEventListener("mousedown", handle, true);
    window.addEventListener("auxclick", onAuxClick, true);
    window.addEventListener("mouseover", onMouseOver, true);
    return () => {
      window.removeEventListener("keydown", handle, true);
      window.removeEventListener("mousedown", handle, true);
      window.removeEventListener("auxclick", onAuxClick, true);
      window.removeEventListener("mouseover", onMouseOver, true);
    };
  }, []);
}
