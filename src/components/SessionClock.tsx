import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import { nyTime, nyDayMonth } from "@/lib/nyTime";

/** Delay applied to prices by Massive's Starter+ plan (see `massive::plan::Tier`).
 *  Fixed by the plan, not measured: it has to tick smoothly once a second, and the
 *  observed lag of the last aggregate jitters with network and with how quietly a
 *  ticker trades. */
const DELAY_MS = 15 * 60_000;

/** Current SESSION time in New York wall-clock, shown top-right in the tab bar.
 *  Follows the Market-Replay simulated clock when a replay is active (so the
 *  displayed time is the replayed instant, DST-aware), otherwise ticks real time
 *  once a second. Underlying instants stay UTC; only the display is NY-localised.
 *
 *  On a 15-minute delayed plan it shows the time the DATA is at — real time minus
 *  the delay — because that is the instant the prices, the candles and the
 *  execution markers on screen belong to. Reading the real clock against a chart
 *  that is a quarter of an hour behind is what makes a delayed session dangerous,
 *  so the amber badge (and the amber figures, as in replay) mark every clock that
 *  is not wall-clock truth. The real time stays one hover away. */
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

  // Massive Starter+ tier: 15-minute delayed data, so the on-screen prices lag
  // the clock. No need to poll fast — the tier rarely changes mid-session.
  const { data: massiveTier } = useQuery({
    queryKey: ["massive_tier_status"],
    queryFn:  () => api.getMassiveTierStatus(),
    refetchInterval: 30_000,
  });
  const isReplay = !!replay?.active && !!replay.sim_time;
  // A replay feeds its own recorded day at its own clock — nothing about it is
  // delayed, so the badge and the shift belong to live sessions only.
  const isDelayed = !isReplay && massiveTier?.effective === "unlimited";

  const now = new Date();
  const instant: Date = isReplay
    ? new Date(replay!.sim_time!)
    : isDelayed
      ? new Date(now.getTime() - DELAY_MS)
      : now;

  const title = isReplay
    ? "Heure simulée (Market Replay) — New York"
    : isDelayed
      ? `Heure des DONNÉES (différées de 15 min) — New York. `
        + `Il est réellement ${nyTime(now, true)} ET. `
        + `Les news restent en temps réel (Alpaca).`
      : "Heure de session — New York";

  return (
    <div
      className="ml-auto flex items-center gap-1.5 whitespace-nowrap pl-2 text-[11px] normal-case tracking-normal tabular-nums"
      title={title}
    >
      {isReplay && (
        <span className="rounded bg-amber-900/40 px-1 py-px text-[8px] font-semibold uppercase tracking-wider text-amber-400">
          Replay
        </span>
      )}
      {isDelayed && (
        <span
          className="rounded bg-amber-900/40 px-1 py-px text-[8px] font-semibold uppercase tracking-wider text-amber-400"
          title="Prix différés de 15 min (Massive Starter+) — les news restent en temps réel (Alpaca)"
        >
          −15 min
        </span>
      )}
      <span className="text-muted-foreground/50">{nyDayMonth(instant)}</span>
      <span
        className={
          isReplay || isDelayed ? "font-medium text-amber-300" : "font-medium text-foreground/80"
        }
      >
        {nyTime(instant, true)}
      </span>
      <span className="text-muted-foreground/40">ET</span>
    </div>
  );
}
