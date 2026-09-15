import { useEffect, useMemo, useState } from "react";
import { CalendarDays, ChevronLeft, ChevronRight } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import type { FlatFileDay, ReplaySource } from "@/types";

interface Props {
  open: boolean;
  /** Tape the replay will read — lifted to the toolbar, which starts the session. */
  source: ReplaySource;
  onSourceChange: (source: ReplaySource) => void;
  onClose: () => void;
  onSelect: (day: string, source: ReplaySource) => void;
}

const WEEKDAY_LABELS = ["Lu", "Ma", "Me", "Je", "Ve", "Sa", "Di"];

/**
 * The three tapes a day can be replayed from. Ranked here by fidelity on the days
 * they cover, NOT by the app's automatic priority: the whole point of the toggle is
 * that a Starter subscription (1 s, every day) and a trade flat file (real prints +
 * quotes, but only over the ignition windows of the days downloaded) each win on
 * different ground, so the arbitration is the trader's.
 */
const SOURCES: {
  id: ReplaySource;
  label: string;
  hint: string;
  dot: string;
}[] = [
  {
    id: "api",
    label: "API",
    hint:
      "Massive / Alpaca — n'importe quelle séance passée, granularité seconde sur Starter+. Dégrade vers les flat files puis la barre daily si la source ne répond pas.",
    dot: "bg-violet-400",
  },
  {
    id: "flat_trades",
    label: "Flat Trades",
    hint:
      "trade-<jour>.db — vrais prints ET quotes sur les fenêtres d'ignition, tranches 10 s synthétisées ailleurs. Aucune requête réseau ; refuse un jour sans fichier trade complet.",
    dot: "bg-emerald-400",
  },
  {
    id: "flat_minutes",
    label: "Flat Minutes",
    hint:
      "minute-<jour>.db seul — tranches 10 s synthétisées depuis les barres 1 min, overlay trade ignoré même s'il existe. Aucune requête réseau.",
    dot: "bg-sky-400",
  },
];

function monthKey(year: number, month: number) {
  return `${year}-${String(month + 1).padStart(2, "0")}`;
}

function daysInMonth(year: number, month: number): number {
  return new Date(year, month + 1, 0).getDate();
}

function startDow(year: number, month: number): number {
  const d = new Date(year, month, 1).getDay();
  return d === 0 ? 6 : d - 1;
}

function isWeekend(day: string): boolean {
  const [y, m, d] = day.split("-").map(Number);
  const dow = new Date(y, m - 1, d).getDay();
  return dow === 0 || dow === 6;
}

export function DayPickerModal({
  open,
  source,
  onSourceChange,
  onClose,
  onSelect,
}: Props) {
  const [minuteDays, setMinuteDays] = useState<FlatFileDay[]>([]);
  const [tradeFileDays, setTradeFileDays] = useState<FlatFileDay[]>([]);
  const [tradedDays, setTradedDays] = useState<string[]>([]);

  const now = new Date();
  const [viewYear, setViewYear] = useState(now.getFullYear());
  const [viewMonth, setViewMonth] = useState(now.getMonth());

  useEffect(() => {
    if (!open) return;
    api.getFlatFilesCalendar("minute").then(setMinuteDays).catch(() => {});
    api.getFlatFilesCalendar("trade").then(setTradeFileDays).catch(() => {});
    api.getTradeDays().then(setTradedDays).catch(() => {});
  }, [open]);

  // Only COMPLETE files count: a partial download would replay a truncated session.
  const minuteSet = useMemo(
    () => new Set(minuteDays.filter((d) => d.complete).map((d) => d.day)),
    [minuteDays],
  );
  const tradeFileSet = useMemo(
    () => new Set(tradeFileDays.filter((d) => d.complete).map((d) => d.day)),
    [tradeFileDays],
  );
  const tradedSet = useMemo(() => new Set(tradedDays), [tradedDays]);

  const today = useMemo(() => {
    const d = new Date();
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(
      d.getDate(),
    ).padStart(2, "0")}`;
  }, []);

  /**
   * Whether `day` can be replayed under the CURRENT source. The API can serve any
   * past weekday, so it is gated on the calendar rather than on a file list; the
   * forced modes are gated on what is actually on disk, because they cannot fall
   * back to anything.
   */
  const isAvailable = useMemo(() => {
    if (source === "flat_trades") return (d: string) => tradeFileSet.has(d);
    if (source === "flat_minutes") return (d: string) => minuteSet.has(d);
    return (d: string) => d <= today && !isWeekend(d);
  }, [source, tradeFileSet, minuteSet, today]);

  const availableCount =
    source === "flat_trades"
      ? tradeFileSet.size
      : source === "flat_minutes"
        ? minuteSet.size
        : null;

  // Months worth jumping to: the days on disk under the current mode, and the union
  // in API mode (where every month is reachable anyway, so the chips are a shortcut
  // to the sessions the user has actually worked on).
  const monthsWithData = useMemo(() => {
    const s = new Set<string>();
    const add = (days: Iterable<string>) => {
      for (const d of days) s.add(d.slice(0, 7));
    };
    if (source === "flat_trades") add(tradeFileSet);
    else if (source === "flat_minutes") add(minuteSet);
    else {
      add(minuteSet);
      add(tradeFileSet);
      add(tradedSet);
    }
    return s;
  }, [source, minuteSet, tradeFileSet, tradedSet]);

  const prev = () => {
    if (viewMonth === 0) { setViewYear((y) => y - 1); setViewMonth(11); }
    else setViewMonth((m) => m - 1);
  };
  const next = () => {
    if (viewMonth === 11) { setViewYear((y) => y + 1); setViewMonth(0); }
    else setViewMonth((m) => m + 1);
  };

  const mk = monthKey(viewYear, viewMonth);
  const totalDays = daysInMonth(viewYear, viewMonth);
  const offset = startDow(viewYear, viewMonth);

  const cells: (string | null)[] = [];
  for (let i = 0; i < offset; i++) cells.push(null);
  for (let d = 1; d <= totalDays; d++) {
    cells.push(`${viewYear}-${String(viewMonth + 1).padStart(2, "0")}-${String(d).padStart(2, "0")}`);
  }

  const handleSelect = (day: string) => {
    onSelect(day, source);
    onClose();
  };

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-sm">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <CalendarDays className="h-4 w-4" />
            Choisir un jour
          </DialogTitle>
        </DialogHeader>

        <div className="space-y-3">
          {/* Source toggle — decided BEFORE the day, because it decides which days
              are selectable at all. */}
          <div className="space-y-1.5">
            <div className="grid grid-cols-3 gap-1 rounded border border-border bg-background p-0.5">
              {SOURCES.map((s) => (
                <button
                  key={s.id}
                  onClick={() => onSourceChange(s.id)}
                  title={s.hint}
                  className={cn(
                    "flex items-center justify-center gap-1.5 rounded px-1 py-1 text-[11px] transition-colors",
                    source === s.id
                      ? "bg-accent font-medium text-foreground"
                      : "text-muted-foreground hover:bg-accent/50",
                  )}
                >
                  <span className={cn("h-1.5 w-1.5 rounded-full", s.dot)} />
                  {s.label}
                </button>
              ))}
            </div>
            <p className="text-[10px] leading-snug text-muted-foreground">
              {SOURCES.find((s) => s.id === source)?.hint}
            </p>
          </div>

          {/* Month nav */}
          <div className="flex items-center justify-between">
            <button onClick={prev} className="rounded p-1 hover:bg-accent">
              <ChevronLeft className="h-4 w-4" />
            </button>
            <span className="text-sm font-medium capitalize">
              {new Date(viewYear, viewMonth).toLocaleDateString("fr-FR", {
                month: "long",
                year: "numeric",
              })}
            </span>
            <button onClick={next} className="rounded p-1 hover:bg-accent">
              <ChevronRight className="h-4 w-4" />
            </button>
          </div>

          {/* Weekday headers */}
          <div className="grid grid-cols-7 gap-px text-center text-[10px] text-muted-foreground">
            {WEEKDAY_LABELS.map((w) => (
              <div key={w} className="py-1">{w}</div>
            ))}
          </div>

          {/* Day cells. The dots always show every dataset the day has, whatever the
              selected source: that is how the user sees a day is worth switching
              mode for, rather than finding it greyed out with no reason given. */}
          <div className="grid grid-cols-7 gap-px">
            {cells.map((day, i) => {
              if (!day) return <div key={`e${i}`} />;
              const hasMinute = minuteSet.has(day);
              const hasTradeFile = tradeFileSet.has(day);
              const hasTraded = tradedSet.has(day);
              const selectable = isAvailable(day);
              const dayNum = parseInt(day.slice(8), 10);

              return (
                <button
                  key={day}
                  disabled={!selectable}
                  onClick={() => handleSelect(day)}
                  title={
                    selectable
                      ? [
                          hasTradeFile && "fichier trade",
                          hasMinute && "fichier minute",
                          hasTraded && "jour tradé",
                        ]
                          .filter(Boolean)
                          .join(" · ") || "via l'API"
                      : source === "api"
                        ? "séance à venir ou week-end"
                        : "aucun fichier local pour ce jour dans ce mode"
                  }
                  className={cn(
                    "relative flex h-9 flex-col items-center justify-center rounded text-xs transition-colors",
                    selectable
                      ? "cursor-pointer hover:bg-accent"
                      : "cursor-default text-muted-foreground/30",
                    selectable && hasTraded && "text-amber-300 font-semibold",
                    selectable && !hasTraded && "text-foreground",
                  )}
                >
                  <span>{dayNum}</span>
                  {(hasTradeFile || hasMinute || hasTraded) && (
                    <span className="absolute bottom-0.5 flex gap-0.5">
                      {hasTradeFile && (
                        <span className="h-1 w-1 rounded-full bg-emerald-400" />
                      )}
                      {hasMinute && (
                        <span className="h-1 w-1 rounded-full bg-sky-400" />
                      )}
                      {hasTraded && (
                        <span className="h-1 w-1 rounded-full bg-amber-400" />
                      )}
                    </span>
                  )}
                </button>
              );
            })}
          </div>

          {/* Legend */}
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-border pt-2 text-[10px] text-muted-foreground">
            <span className="flex items-center gap-1">
              <span className="h-2 w-2 rounded-full bg-emerald-400" />
              Fichier trade
            </span>
            <span className="flex items-center gap-1">
              <span className="h-2 w-2 rounded-full bg-sky-400" />
              Fichier minute
            </span>
            <span className="flex items-center gap-1">
              <span className="h-2 w-2 rounded-full bg-amber-400" />
              Jour tradé
            </span>
            {availableCount !== null && (
              <span className="ml-auto">
                {availableCount} jour{availableCount > 1 ? "s" : ""} sur disque
              </span>
            )}
          </div>

          {/* Quick jump to months with data */}
          {monthsWithData.size > 0 && (
            <div className="flex flex-wrap gap-1 border-t border-border pt-2">
              {[...monthsWithData]
                .sort()
                .reverse()
                .slice(0, 12)
                .map((m) => {
                  const [y, mo] = m.split("-").map(Number);
                  const label = new Date(y, mo - 1).toLocaleDateString("fr-FR", {
                    month: "short",
                    year: "2-digit",
                  });
                  const isCurrent = mk === m;
                  return (
                    <button
                      key={m}
                      onClick={() => { setViewYear(y); setViewMonth(mo - 1); }}
                      className={cn(
                        "rounded px-2 py-0.5 text-[10px] transition-colors",
                        isCurrent
                          ? "bg-accent text-foreground"
                          : "text-muted-foreground hover:bg-accent/50",
                      )}
                    >
                      {label}
                    </button>
                  );
                })}
            </div>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
