import { api } from "@/lib/tauri";
import { useUiStore } from "@/stores/uiStore";
import type { UsageEvent, UsageSource } from "@/types";

// Usage tracker: records every user action — which control or command, from
// where, and when — into the local `usage_events` table, to see what is used the
// most (and at what time of day) and what is never used.
//
// Two feeds:
//   • DOM controls — one capture-phase listener on the window, so every button,
//     tab, menu item and switch is covered without touching the components, new
//     ones included. A control is named by its label and located by its context
//     (region > dialog > tab > menu).
//   • Commands that have no DOM control — hotkeys, the gamepad, a few raw keys —
//     call `trackUsage` from where they are dispatched, with their command id.
//
// What is typed into a field is never read. Events are buffered and written in
// batches; a tracking failure must never reach the UI, so everything here is
// swallowed.

const MAX_TEXT = 80;
/** Free text on a clickable that is not a real control (a card, a row) is mostly
 *  data — a ticker, a price — so it only names the control when it is short and
 *  there is no tooltip. */
const MAX_LOOSE_TEXT = 32;
const FLUSH_MS = 500;
const RETRY_MS = 5_000;
const MAX_PENDING = 5_000;

/** Real controls. `label` is left out on purpose: clicking one re-dispatches the
 *  click on its field, which is the one counted. */
const CONTROLS = [
  "[data-track]", "button", "a[href]", "summary", "select",
  "input[type=checkbox]", "input[type=radio]", "input[type=button]", "input[type=submit]",
  "[role=button]", "[role=tab]", "[role=switch]", "[role=checkbox]", "[role=radio]",
  "[role=menuitem]", "[role=menuitemcheckbox]", "[role=menuitemradio]", "[role=option]",
].join(",");

let windowLabel: string | null = null; // null → not installed, everything is dropped
let pending: UsageEvent[] = [];
let timer: number | null = null;
let sending = false;

// ─── Buffer ───────────────────────────────────────────────────────────────────

function schedule(delay: number): void {
  if (timer === null) timer = window.setTimeout(flush, delay);
}

function flush(): void {
  if (timer !== null) { window.clearTimeout(timer); timer = null; }
  if (sending || pending.length === 0) return;
  const batch = pending;
  pending = [];
  sending = true;
  api.recordUsageEvents(batch)
    .then(
      () => { sending = false; if (pending.length > 0) schedule(FLUSH_MS); },
      () => {
        // Keep the batch for the next attempt (bounded, oldest dropped first).
        sending = false;
        pending = batch.concat(pending).slice(-MAX_PENDING);
        schedule(RETRY_MS);
      },
    );
}

function currentView(): string {
  if (windowLabel !== "main") return windowLabel ?? "";
  const { activeView, activeSession } = useUiStore.getState();
  return activeView === "trading" ? `trading/${activeSession}` : activeView;
}

/** Record one action. `action` is a stable command id (hotkey / gamepad) or the
 *  control's label (click). Safe to call from anywhere, at any rate. */
export function trackUsage(
  source: UsageSource,
  action: string,
  opts: { context?: string; input?: string | null; hint?: string | null } = {},
): void {
  if (windowLabel === null) return;
  pending.push({
    ts_ms: Date.now(),
    tz_offset_min: -new Date().getTimezoneOffset(),
    source,
    action,
    context: opts.context ?? "global",
    input: opts.input ?? null,
    hint: opts.hint ?? null,
    view: currentView(),
    window: windowLabel,
  });
  if (pending.length > MAX_PENDING) pending.shift();
  schedule(FLUSH_MS);
}

// ─── Naming a DOM control ─────────────────────────────────────────────────────

function clean(s: string | null | undefined): string {
  return (s ?? "").replace(/\s+/g, " ").trim().slice(0, MAX_TEXT);
}

/** Icon-only button with no label at all: fall back to its lucide icon name. */
function iconOf(el: Element): string {
  const svg = el.querySelector("svg[class*='lucide-']");
  const cls = svg ? Array.from(svg.classList).find((c) => c.startsWith("lucide-")) : undefined;
  return cls ? `icon:${cls.slice("lucide-".length)}` : "";
}

function isField(el: Element): el is HTMLInputElement | HTMLSelectElement {
  return el instanceof HTMLInputElement || el instanceof HTMLSelectElement;
}

/** Text of a field's <label>, without the field itself when the label wraps it
 *  (a wrapped <select> would otherwise add all its options). */
function labelText(el: HTMLInputElement | HTMLSelectElement): string {
  const label = el.labels?.[0];
  if (!label) return "";
  if (!label.contains(el)) return clean(label.textContent);
  const copy = label.cloneNode(true) as HTMLElement;
  copy.querySelectorAll("input,select,textarea").forEach((f) => f.remove());
  return clean(copy.textContent);
}

/** The control's name, plus its tooltip when that says something else. `loose`
 *  = a clickable that is not a real control (a card, a value to edit): its text
 *  is data more often than a label, so the tooltip names it first. */
function nameOf(el: HTMLElement, loose = false): { action: string; hint: string | null } {
  const title = clean(el.getAttribute("title"));
  let text: string;
  if (isField(el)) {
    // A field's own text is its options or its value — use its <label> instead.
    text = labelText(el) || clean(el.name) || clean(el.id);
  } else {
    text = clean(el.textContent);
    if (loose && text.length > MAX_LOOSE_TEXT) text = "";
  }
  // "×", "+", an emoji: a glyph is a worse name than the tooltip beside it.
  const worded = /[\p{L}\p{N}]/u.test(text);
  const action = clean(el.dataset.track)
    || clean(el.getAttribute("aria-label"))
    || (loose ? title : "")
    || (worded ? text : "")
    || title
    || text
    || iconOf(el)
    || el.tagName.toLowerCase();
  return { action, hint: title && title !== action ? title : null };
}

function kindOf(el: HTMLElement): string {
  const role = el.getAttribute("role");
  if (role) return role;
  const tag = el.tagName.toLowerCase();
  return el instanceof HTMLInputElement ? `${tag}:${el.type}` : tag;
}

/** The element an `aria-labelledby` points to (Radix: a dialog's title, or the
 *  trigger of a menu / tab panel). */
function labelledBy(el: Element): HTMLElement | null {
  const id = el.getAttribute("aria-labelledby")?.split(/\s+/)[0];
  return id ? document.getElementById(id) : null;
}

/** Where the control sits, outermost first: `left-rail`, `chart > menu:…`,
 *  `dialog:Settings > tab:Broker`. Menus and dialogs are portalled to <body>, so
 *  a menu is followed back to its trigger to recover the region it belongs to. */
function contextOf(el: Element): string {
  const parts: string[] = [];
  let node: Element | null = el.parentElement;
  for (let hops = 0; node && hops < 200; hops++) {
    const html = node as HTMLElement;
    const role = node.getAttribute("role");
    let next: Element | null = node.parentElement;
    if (html.dataset?.usageArea) {
      parts.push(html.dataset.usageArea);
    } else if (node.hasAttribute("data-zone-id")) {
      parts.push("chart");
    } else if (role === "tabpanel") {
      const tab = labelledBy(node);
      if (tab) parts.push(`tab:${nameOf(tab).action}`);
    } else if (role === "dialog" || role === "alertdialog") {
      const title = clean(labelledBy(node)?.textContent) || clean(node.getAttribute("aria-label"));
      parts.push(title ? `dialog:${title}` : "dialog");
    } else if (role === "menu" || role === "listbox") {
      const trigger = labelledBy(node);
      if (trigger && !node.contains(trigger)) {
        parts.push(`menu:${nameOf(trigger).action}`);
        next = trigger.parentElement;
      } else {
        parts.push("menu");
      }
    }
    node = next;
  }
  return parts.length > 0 ? parts.reverse().join(" > ") : "app";
}

/** A clickable that is not a real control (a card, a row, a timeline): the
 *  outermost element of the `cursor: pointer` run under the click. */
function pointerRoot(start: HTMLElement): HTMLElement | null {
  if (start.closest("label")) return null; // its field gets the click that counts
  if (getComputedStyle(start).cursor !== "pointer") return null;
  let el = start;
  while (el.parentElement && getComputedStyle(el.parentElement).cursor === "pointer") {
    el = el.parentElement;
  }
  return el;
}

// ─── DOM listeners ────────────────────────────────────────────────────────────

function onClick(e: MouseEvent): void {
  try {
    const target = e.target instanceof HTMLElement ? e.target
      : e.target instanceof Element ? e.target.parentElement // an <svg> / <path>
      : null;
    if (!target) return;
    const control = target.closest<HTMLElement>(CONTROLS);
    const el = control ?? pointerRoot(target);
    if (!el || el.getAttribute("aria-disabled") === "true") return;
    const { action, hint } = nameOf(el, !control);
    trackUsage("click", action, { context: contextOf(el), input: kindOf(el), hint });
  } catch {
    /* never let tracking break a click */
  }
}

/** A <select> is clicked to open, then changed: the change is the action, and the
 *  option picked is part of it. A slider only reports that it was moved. */
function onChange(e: Event): void {
  try {
    const el = e.target;
    let input: string;
    if (el instanceof HTMLSelectElement) {
      input = `select:${clean(el.selectedOptions[0]?.textContent) || clean(el.value)}`;
    } else if (el instanceof HTMLInputElement && el.type === "range") {
      input = "input:range";
    } else {
      return;
    }
    const { action, hint } = nameOf(el);
    trackUsage("change", action, { context: contextOf(el), input, hint });
  } catch {
    /* ignore */
  }
}

function onHidden(): void {
  if (document.visibilityState === "hidden") flush();
}

/** Start recording in this window. Called once from the entry point, outside
 *  React; `label` is the Tauri window label. */
export function installUsageTracker(label: string): void {
  if (windowLabel !== null) return;
  windowLabel = label;
  window.addEventListener("click", onClick, true);
  window.addEventListener("change", onChange, true);
  // Don't sit on the last actions when the window goes away.
  window.addEventListener("pagehide", flush);
  document.addEventListener("visibilitychange", onHidden);
}
