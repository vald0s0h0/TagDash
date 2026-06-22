import { cn } from "@/lib/utils";
import type { DashboardTrade } from "@/types";
import { closedTrades, summarize, formatMoney, formatPf, formatPct } from "./kpis";
import { EmptyCard } from "./frosted";

function Cell({
  value,
  label,
  italic,
}: {
  value: string;
  label: string;
  italic?: boolean;
}) {
  return (
    <div className="flex flex-1 flex-col items-center justify-center text-center">
      <span
        className={cn(
          "font-display text-[38px] leading-[0.9] tracking-[-0.02em] tabular-nums text-white",
          italic && "italic"
        )}
      >
        {value}
      </span>
      <span className="mt-2 font-spacemono text-[10px] uppercase tracking-[0.10em] text-white/55">
        {label}
      </span>
    </div>
  );
}

/** Headline trading KPIs as a Frosted/Brutal card 07 (KPI×3): three cells split by
 *  hairline dividers. Monochrome — no gain/loss colour. */
export function KpiCard({ trades }: { trades: DashboardTrade[] }) {
  const k = summarize(closedTrades(trades));

  if (k.count === 0) {
    return <EmptyCard label="Performance" message="Aucun trade clôturé" />;
  }

  return (
    <div className="flex h-full w-full items-center px-2 py-6">
      <Cell value={formatMoney(k.totalPnl)} label="PnL Total" />
      <div className="h-[54px] w-px bg-white/[0.14]" />
      <Cell value={formatPf(k.profitFactor)} label="Facteur Profit" />
      <div className="h-[54px] w-px bg-white/[0.14]" />
      <Cell value={formatPct(k.winRate)} label="Réussite" italic />
    </div>
  );
}
