import {
  AlarmClock, AlarmClockPlus, ArrowDownUp, Bell, Bird, Camera, CheckCircle,
  ChevronDown, ChevronLeft, ChevronRight, ChevronUp, ChevronsRight, CircleDot,
  CircleX, Clock, Columns2, Crosshair, DollarSign, FastForward, History, Mic,
  NotebookPen, Pause, Play, Radar, SkipForward, Slash, Smile, Sun, Sunrise, Tag,
  Trash2, Type, X,
  type LucideIcon,
} from "lucide-react";
import {
  commandDef, type GamepadCommandId, type GamepadDisplay,
} from "@/stores/gamepadStore";

// How a controller command is drawn on its key cap — shared by the on-screen
// overlay and the Settings editor so both stay identical.
//
// The icons are the interface's own: a command reuses the glyph of the toolbar /
// rail button that does the same thing (Bird = Libérer, AlarmClockPlus = alarme,
// Camera = capture, …). Where the toolbar shows text instead of an icon (SL, TP,
// 25 %…) the command has no default icon and its text carries it.

/** Every icon a key cap can show (the picker in Settings lists these). */
export const PAD_ICONS: Record<string, LucideIcon> = {
  // Chart toolbar
  Bird, Slash, Type, Smile, Clock, DollarSign, AlarmClockPlus, CircleX, Camera,
  CheckCircle, NotebookPen, Mic, Tag, Trash2, X,
  // Rail / sidebar
  Sunrise, CircleDot, Sun, Radar, Bell, AlarmClock, History,
  // Replay bar
  Play, Pause, SkipForward, FastForward, ChevronsRight,
  // Directions / pad-only
  ChevronUp, ChevronDown, ChevronLeft, ChevronRight, Columns2, Crosshair, ArrowDownUp,
};

const DEFAULT_ICON: Partial<Record<GamepadCommandId, string>> = {
  ticker_prev:    "ChevronUp",
  ticker_next:    "ChevronDown",
  ticker_release: "Bird",
  tab_prev:       "ChevronLeft",
  tab_next:       "ChevronRight",
  focus_cycle:    "Columns2",
  cursor_alarm:   "AlarmClockPlus",
  remove_orders:  "Trash2",
  close_position: "CircleX",
  confirm_hod:    "DollarSign",
  invert_hod:     "ArrowDownUp",
  capture:        "Camera",
  share_tag:      "Tag",
  journal_audio:  "Mic",
  replay_next_alert: "SkipForward",
  replay_play_pause: "Play",
};

function padIcon(name: string | undefined): LucideIcon | null {
  return name !== undefined && Object.prototype.hasOwnProperty.call(PAD_ICONS, name) ? PAD_ICONS[name] : null;
}

export function defaultIconName(id: GamepadCommandId): string | undefined {
  return DEFAULT_ICON[id];
}

export interface ResolvedDisplay {
  /** Null when the cap shows text only. */
  Icon: LucideIcon | null;
  /** Null when the cap shows the icon only. */
  text: string | null;
  /** Full command name (tooltip). */
  title: string;
}

/** Merge a command's defaults with the user's override into what to draw. A cap
 *  is never blank: "icon" without an icon falls back to the text. */
export function resolveDisplay(id: GamepadCommandId, override: GamepadDisplay | undefined): ResolvedDisplay {
  const def = commandDef(id);
  const mode = override?.mode ?? "both";
  const label = override?.text?.trim() || def?.short || id;
  const icon = padIcon(override?.icon ?? DEFAULT_ICON[id]);
  return {
    Icon: mode === "text" ? null : icon,
    text: mode === "icon" && icon ? null : label,
    title: def?.label ?? id,
  };
}
