import { useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import type { Bar } from "@/types";
import { api } from "@/lib/tauri";
import { nyDayStamp, nySession } from "@/lib/nyTime";

/** Premarket history pulled on the first load, in calendar days. Sized to cover a
 *  daily pane's DEFAULT view (~100 bars ≈ 5 months at the default bar spacing), so
 *  switching the ghost on shows something immediately instead of after a fetch.
 *  Matches the backend's alert pre-warm, which caches exactly this span. */
const DEFAULT_WINDOW_DAYS = 180;

/** The window grows in whole backend chunks (`premarket::CHUNK_DAYS`), so one
 *  widening = one provider request, and a slow scroll left cannot fire a refetch
 *  per bar crossed. */
const CHUNK_DAYS = 60;

/** Days of already-loaded ghost kept beyond the left edge of the view. Widening
 *  starts before the edge is reached, so the ghost is there when you arrive. */
const MARGIN_DAYS = 20;

/** How often the FORMING premarket candle is re-pulled while the session runs.
 *  Cheap: the backend answers a fully-covered window from SQLite and overlays the
 *  live RAM bar, so this is a local read, not a provider call. */
const LIVE_POLL_MS = 5_000;

/** `YYYY-MM-DD` (NY) `days` before now. */
function nyDateDaysAgo(days: number): string {
  return nyDayStamp(new Date(Date.now() - days * 86_400_000)).day;
}

/** Premarket (04:00–09:30 ET) daily candles for the daily pane's ghost series.
 *
 *  The window follows the VISIBLE range, not the loaded history. That distinction
 *  is the whole point: a daily pane loads 600 bars (~2.5 years) up front while
 *  showing ~100 of them, so sizing the window off `bars[0]` asked the backend for
 *  two and a half years on the very first render — a dozen sequential provider
 *  chunks before a single ghost could be drawn. Following the view asks for what
 *  is on screen, then widens one chunk at a time as you scroll back.
 *
 *  The window only ever grows: scrolling forward again must not throw away history
 *  already fetched, and a symbol switch resets it (see the effect below).
 *
 *  Disabled panes fetch nothing at all (`enabled` is false on every intraday pane
 *  and whenever the mode is "off"), so this costs zero until it is switched on. */
export function usePremarketBars(
  symbol: string,
  enabled: boolean,
  /** Unix seconds of the leftmost visible bar, or null before the first layout. */
  visibleFromSec: number | null,
): Bar[] | undefined {
  const [windowDays, setWindowDays] = useState(DEFAULT_WINDOW_DAYS);

  // A different ticker starts from the default window again — its own history has
  // nothing to do with the one we had just widened for.
  useEffect(() => { setWindowDays(DEFAULT_WINDOW_DAYS); }, [symbol]);

  useEffect(() => {
    if (!enabled || visibleFromSec == null) return;
    const daysBack = (Date.now() / 1000 - visibleFromSec) / 86_400;
    setWindowDays((cur) => {
      if (daysBack <= cur - MARGIN_DAYS) return cur; // still comfortably covered
      const wanted = Math.ceil((daysBack + MARGIN_DAYS) / CHUNK_DAYS) * CHUNK_DAYS;
      return Math.max(cur, wanted);
    });
  }, [enabled, visibleFromSec]);

  const fromDate = useMemo(() => nyDateDaysAgo(windowDays), [windowDays]);

  // While premarket is running the forming candle moves, so poll; the rest of the
  // day the series is final and a refetch would be pure waste.
  const live = nySession(new Date()) === "pre";

  const { data } = useQuery({
    queryKey: ["premarket_daily", symbol, fromDate],
    queryFn:  () => api.loadPremarketDaily(symbol, fromDate),
    enabled:  enabled && !!symbol,
    // Widening the window changes the key. Without this the ghost would blank out
    // for the length of the fetch every time a scroll-back extends it — keep the
    // narrower series on screen until the wider one lands.
    placeholderData: (prev) => prev,
    staleTime: live ? 0 : 5 * 60 * 1000,
    refetchInterval: live ? LIVE_POLL_MS : false,
    retry: false,
  });

  return data;
}
