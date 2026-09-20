import { cn } from "@/lib/utils";
import type { DashboardTrade } from "@/types";
import { closedTrades, riskSummary, formatMoney } from "./kpis";
import { EmptyCard } from "./frosted";

const WINDOW_DAYS = 20;

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

/** Risk sizing over the trailing 20 days, as a Frosted/Brutal card 07 (KPI×3):
 *  espérance par trade, cutoff journalier (3× le PnL moyen/jour) et risque par
 *  trade (= le PnL moyen/jour). */
export function RiskCard({ trades }: { trades: DashboardTrade[] }) {
  const r = riskSummary(closedTrades(trades), WINDOW_DAYS);

  if (r.tradeCount === 0) {
    return <EmptyCard label="Risque · 20j" message="Aucun trade clôturé récent" />;
  }

  return (
    <div className="flex h-full w-full items-center px-2 py-6">
      <Cell value={formatMoney(r.expectancy)} label="Espérance" />
      <div className="h-[54px] w-px bg-white/[0.14]" />
      <Cell value={formatMoney(r.dailyCutoff)} label="Daily Cut Off" italic />
      <div className="h-[54px] w-px bg-white/[0.14]" />
      <Cell value={formatMoney(r.riskPerTrade)} label="Risque / Trade" />
    </div>
  );
}
