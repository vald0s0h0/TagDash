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
import type { FlatFileDay } from "@/types";

interface Props {
  open: boolean;
  onClose: () => void;
  onSelect: (day: string) => void;
}

const WEEKDAY_LABELS = ["Lu", "Ma", "Me", "Je", "Ve", "Sa", "Di"];

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

export function DayPickerModal({ open, onClose, onSelect }: Props) {
  const [flatDays, setFlatDays] = useState<FlatFileDay[]>([]);
  const [tradeDays, setTradeDays] = useState<string[]>([]);

  const now = new Date();
  const [viewYear, setViewYear] = useState(now.getFullYear());
  const [viewMonth, setViewMonth] = useState(now.getMonth());

  useEffect(() => {
    if (!open) return;
    api.getFlatFilesCalendar("minute").then(setFlatDays).catch(() => {});
    api.getTradeDays().then(setTradeDays).catch(() => {});
  }, [open]);

  const flatSet = useMemo(
    () => new Set(flatDays.filter((d) => d.complete).map((d) => d.day)),
    [flatDays],
  );
  const tradeSet = useMemo(() => new Set(tradeDays), [tradeDays]);

  const allDays = useMemo(() => {
    const s = new Set<string>();
    for (const d of flatSet) s.add(d);
    for (const d of tradeSet) s.add(d);
    return s;
  }, [flatSet, tradeSet]);

  const monthsWithData = useMemo(() => {
    const s = new Set<string>();
    for (const d of allDays) s.add(d.slice(0, 7));
    return s;
  }, [allDays]);

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
    onSelect(day);
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

          {/* Day cells */}
          <div className="grid grid-cols-7 gap-px">
            {cells.map((day, i) => {
              if (!day) return <div key={`e${i}`} />;
              const hasFlat = flatSet.has(day);
              const hasTrade = tradeSet.has(day);
              const hasAny = hasFlat || hasTrade;
              const dayNum = parseInt(day.slice(8), 10);

              return (
                <button
                  key={day}
                  disabled={!hasAny}
                  onClick={() => handleSelect(day)}
                  className={cn(
                    "relative flex h-9 flex-col items-center justify-center rounded text-xs transition-colors",
                    hasAny
                      ? "cursor-pointer hover:bg-accent"
                      : "cursor-default text-muted-foreground/30",
                    hasFlat && !hasTrade && "text-sky-400",
                    hasTrade && "text-amber-300 font-semibold",
                  )}
                >
                  <span>{dayNum}</span>
                  {hasAny && (
                    <span className="flex gap-0.5 absolute bottom-0.5">
                      {hasFlat && (
                        <span className="h-1 w-1 rounded-full bg-sky-400" />
                      )}
                      {hasTrade && (
                        <span className="h-1 w-1 rounded-full bg-amber-400" />
                      )}
                    </span>
                  )}
                </button>
              );
            })}
          </div>

          {/* Legend */}
          <div className="flex items-center gap-4 border-t border-border pt-2 text-[10px] text-muted-foreground">
            <span className="flex items-center gap-1">
              <span className="h-2 w-2 rounded-full bg-sky-400" />
              Data disponible
            </span>
            <span className="flex items-center gap-1">
              <span className="h-2 w-2 rounded-full bg-amber-400" />
              Jour tradé
            </span>
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
