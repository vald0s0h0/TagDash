import type { DashboardTrade } from "@/types";
import { closedTrades, rollingProfitFactor } from "./kpis";
import { ChartCard } from "./ChartCard";
import { EmptyCard } from "./frosted";

const WINDOW = 20;

/** Rolling profit factor over a trailing window, as a Frosted/Brutal card 01. */
export function RollingProfitFactorCard({ trades }: { trades: DashboardTrade[] }) {
  const data = rollingProfitFactor(closedTrades(trades), WINDOW);

  if (data.length === 0) {
    return <EmptyCard label={`Facteur de profit · ${WINDOW}`} message="Aucun trade clôturé" />;
  }

  const series = data.map((d) => d.pf);
  const last = series[series.length - 1];

  return (
    <ChartCard
      label={`Facteur de profit · ${WINDOW}`}
      value={last.toFixed(2)}
      data={series}
    />
  );
}
