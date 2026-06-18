import { cn } from "@/lib/utils";
import type { DashboardTrade } from "@/types";
import { closedTrades, summarize, formatMoney, formatPf, formatPct } from "./kpis";

function Stat({
  label,
  value,
  tone,
}: {
  label: string;
  value: string;
  tone?: "pos" | "neg" | "neutral";
}) {
  return (
    <div className="flex flex-col justify-center">
      <span className="text-[10px] uppercase tracking-wider text-foreground/50">{label}</span>
      <span
        className={cn(
          "tabular-nums text-lg font-semibold leading-tight",
          tone === "pos" && "text-emerald-400",
          tone === "neg" && "text-red-400",
          (!tone || tone === "neutral") && "text-foreground"
        )}
      >
        {value}
      </span>
    </div>
  );
}

/** Headline trading KPIs derived from the closed trades. */
export function KpiCard({ trades }: { trades: DashboardTrade[] }) {
  const k = summarize(closedTrades(trades));

  if (k.count === 0) {
    return (
      <div className="flex h-full items-center justify-center text-xs text-foreground/50">
        Aucun trade clôturé
      </div>
    );
  }

  return (
    <div className="grid h-full grid-cols-3 gap-x-4 gap-y-2 content-center">
      <Stat label="Facteur de profit" value={formatPf(k.profitFactor)} tone={(k.profitFactor ?? 0) >= 1 ? "pos" : "neg"} />
      <Stat label="Réussite" value={formatPct(k.winRate)} />
      <Stat label="PnL total" value={formatMoney(k.totalPnl)} tone={k.totalPnl >= 0 ? "pos" : "neg"} />
      <Stat label="Trades" value={String(k.count)} />
      <Stat label="Gain moyen" value={formatMoney(k.avgWin)} tone="pos" />
      <Stat label="Perte moyenne" value={formatMoney(-k.avgLoss)} tone="neg" />
    </div>
  );
}
