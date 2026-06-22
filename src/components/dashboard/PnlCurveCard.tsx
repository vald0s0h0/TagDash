import type { DashboardTrade } from "@/types";
import { closedTrades, pnlCurve, formatMoney } from "./kpis";
import { ChartCard } from "./ChartCard";
import { EmptyCard } from "./frosted";

/** Cumulative realized P&L (equity curve) as a Frosted/Brutal card 01. Monochrome:
 *  gains/losses read from the shape of the curve, not colour. */
export function PnlCurveCard({ trades }: { trades: DashboardTrade[] }) {
  const data = pnlCurve(closedTrades(trades));

  if (data.length === 0) {
    return <EmptyCard label="PnL Cumulé" message="Aucun trade clôturé" />;
  }

  const series = data.map((d) => d.cumulative);
  const last = series[series.length - 1];

  return <ChartCard label="PnL Cumulé" value={formatMoney(last)} data={series} />;
}
