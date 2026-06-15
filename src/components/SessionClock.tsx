import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import { nyTime, nyDayMonth } from "@/lib/nyTime";

/** Current SESSION time in New York wall-clock, shown top-right in the tab bar.
 *  Follows the Market-Replay simulated clock when a replay is active (so the
 *  displayed time is the replayed instant, DST-aware), otherwise ticks real time
 *  once a second. Underlying instants stay UTC; only the display is NY-localised. */
export function SessionClock() {
  // Re-render every second for the live tick.
  const [, force] = useState(0);
  useEffect(() => {
    const id = setInterval(() => force((n) => n + 1), 1000);
    return () => clearInterval(id);
  }, []);

  // Replay clock (polled ~1s); when active, its sim_time overrides real time.
  const { data: replay } = useQuery({
    queryKey: ["replay_status"],
    queryFn:  () => api.getReplayStatus(),
    refetchInterval: 1000,
  });

  const isReplay = !!replay?.active && !!replay.sim_time;
  const instant: Date = isReplay ? new Date(replay!.sim_time!) : new Date();

  return (
    <div
      className="ml-auto flex items-center gap-1.5 whitespace-nowrap pl-2 text-[11px] normal-case tracking-normal tabular-nums"
      title={isReplay ? "Heure simulée (Market Replay) — New York" : "Heure de session — New York"}
    >
      {isReplay && (
        <span className="rounded bg-amber-900/40 px-1 py-px text-[8px] font-semibold uppercase tracking-wider text-amber-400">
          Replay
        </span>
      )}
      <span className="text-muted-foreground/50">{nyDayMonth(instant)}</span>
      <span className={isReplay ? "font-medium text-amber-300" : "font-medium text-foreground/80"}>
        {nyTime(instant, true)}
      </span>
      <span className="text-muted-foreground/40">ET</span>
    </div>
  );
}
