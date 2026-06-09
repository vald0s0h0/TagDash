import { Activity, Play, Square } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { ScrollArea } from "@/components/ui/scroll-area";
import { cn } from "@/lib/utils";
import { useMarketSnapshot, useMockFeed } from "@/queries/useMarket";
import type { LatencyLevel, TickerLiveState } from "@/types";

// ─── Helpers ─────────────────────────────────────────────────────────────────

function fmtPrice(n: number | null): string {
  return n == null ? "—" : n.toFixed(2);
}

function fmtVol(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000)     return `${(n / 1_000).toFixed(0)}K`;
  return String(n);
}

function fmtPct(n: number | null): string {
  return n == null ? "—" : `${n >= 0 ? "+" : ""}${n.toFixed(2)}%`;
}

function latencyColor(level: LatencyLevel): string {
  switch (level) {
    case "normal":   return "text-emerald-400";
    case "warning":  return "text-amber-400";
    case "slow":     return "text-orange-400";
    case "critical": return "text-red-500";
  }
}

// ─── Latency badge ────────────────────────────────────────────────────────────

function LatencyBadge({ ms, level }: { ms: number; level: LatencyLevel }) {
  return (
    <span className={cn("flex items-center gap-1 text-[11px] tabular-nums", latencyColor(level))}>
      <Activity className="h-3 w-3" />
      {ms} ms
    </span>
  );
}

// ─── One row ─────────────────────────────────────────────────────────────────

function TickerRow({ t }: { t: TickerLiveState }) {
  const chg    = t.change_day_pct;
  const chgCls = chg == null ? "" : chg >= 0 ? "text-emerald-400" : "text-red-400";

  return (
    <tr className="border-b border-border/30 hover:bg-accent/20">
      <td className="px-3 py-1.5 font-semibold">{t.symbol}</td>
      <td className="px-3 py-1.5 text-right tabular-nums font-mono">
        {fmtPrice(t.last_price)}
      </td>
      <td className="px-3 py-1.5 text-right tabular-nums text-muted-foreground">
        {fmtPrice(t.bid)}
      </td>
      <td className="px-3 py-1.5 text-right tabular-nums text-muted-foreground">
        {fmtPrice(t.ask)}
      </td>
      <td className="px-3 py-1.5 text-right tabular-nums text-muted-foreground">
        {fmtPrice(t.spread)}
      </td>
      <td className="px-3 py-1.5 text-right tabular-nums">
        {fmtVol(t.volume_day)}
      </td>
      <td className="px-3 py-1.5 text-right tabular-nums text-sky-400">
        {fmtPrice(t.high_day)}
      </td>
      <td className="px-3 py-1.5 text-right tabular-nums text-rose-400">
        {fmtPrice(t.low_day)}
      </td>
      <td className={cn("px-3 py-1.5 text-right tabular-nums", chgCls)}>
        {fmtPct(chg)}
      </td>
      <td className="px-3 py-1.5 text-right tabular-nums text-muted-foreground text-[10px]">
        {t.latency_ui_ms != null ? `${t.latency_ui_ms}ms` : "—"}
      </td>
    </tr>
  );
}

// ─── Main component ───────────────────────────────────────────────────────────

export function LiveFeedPanel() {
  const { data: snapshot } = useMarketSnapshot();
  const { start, stop }    = useMockFeed();

  const isRunning = snapshot?.mock_running ?? false;
  const tickers   = Object.values(snapshot?.tickers ?? {}).sort((a, b) =>
    a.symbol.localeCompare(b.symbol)
  );

  return (
    <div className="flex flex-col border-b border-border bg-card/60">
      {/* Header bar */}
      <div className="flex items-center justify-between px-4 py-2">
        <div className="flex items-center gap-3">
          <span className="text-xs font-semibold uppercase tracking-wide text-foreground">
            Live Market Feed
          </span>
          <Badge
            variant="outline"
            className={cn(
              "text-[10px] px-1.5 py-0",
              isRunning ? "border-emerald-700 text-emerald-400" : "border-border text-muted-foreground"
            )}
          >
            {isRunning ? "LIVE" : "STOPPED"}
          </Badge>
          {tickers.length > 0 && (
            <span className="text-[11px] text-muted-foreground">
              {tickers.length} ticker{tickers.length !== 1 ? "s" : ""}
            </span>
          )}
        </div>

        <div className="flex items-center gap-3">
          {snapshot && (
            <LatencyBadge
              ms={snapshot.latency.websocket_to_ui_ms}
              level={snapshot.latency.level}
            />
          )}
          {isRunning ? (
            <Button
              size="sm"
              variant="outline"
              className="h-7 px-2 text-[11px]"
              onClick={() => stop.mutate()}
              disabled={stop.isPending}
            >
              <Square className="mr-1 h-3 w-3 fill-current" />
              Stop
            </Button>
          ) : (
            <Button
              size="sm"
              className="h-7 px-2 text-[11px]"
              onClick={() => start.mutate()}
              disabled={start.isPending}
            >
              <Play className="mr-1 h-3 w-3 fill-current" />
              Start mock feed
            </Button>
          )}
        </div>
      </div>

      {/* Ticker table (only shown when tickers exist) */}
      {tickers.length > 0 && (
        <ScrollArea className="h-48 border-t border-border/50">
          <table className="w-full text-xs">
            <thead className="sticky top-0 bg-card z-10">
              <tr className="border-b border-border text-[10px] uppercase tracking-wider text-muted-foreground">
                <th className="px-3 py-1.5 text-left">Symbol</th>
                <th className="px-3 py-1.5 text-right">Price</th>
                <th className="px-3 py-1.5 text-right">Bid</th>
                <th className="px-3 py-1.5 text-right">Ask</th>
                <th className="px-3 py-1.5 text-right">Spread</th>
                <th className="px-3 py-1.5 text-right">Vol</th>
                <th className="px-3 py-1.5 text-right">High</th>
                <th className="px-3 py-1.5 text-right">Low</th>
                <th className="px-3 py-1.5 text-right">Chg%</th>
                <th className="px-3 py-1.5 text-right">Lat</th>
              </tr>
            </thead>
            <tbody>
              {tickers.map((t) => (
                <TickerRow key={t.symbol} t={t} />
              ))}
            </tbody>
          </table>
        </ScrollArea>
      )}

      {/* Empty-state placeholder */}
      {!isRunning && tickers.length === 0 && (
        <div className="px-4 pb-3 text-[11px] text-muted-foreground">
          Click "Start mock feed" to generate live data.
        </div>
      )}
    </div>
  );
}
