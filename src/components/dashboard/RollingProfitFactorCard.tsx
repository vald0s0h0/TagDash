import type { DashboardTrade } from "@/types";
import { closedTrades, rollingProfitFactor } from "./kpis";
import { ChartCard } from "./ChartCard";
import { EmptyCard } from "./frosted";

/** Rolling profit factor over a trailing window, as a Frosted/Brutal card 01.
 *  Y scale always starts at 0; draws a thin unlabeled reference line at PF=1
 *  (breakeven). */
export function RollingProfitFactorCard({
  trades,
  window = 20,
}: {
  trades: DashboardTrade[];
  window?: number;
}) {
  const closed = closedTrades(trades);
  const data = rollingProfitFactor(closed, window);

  if (data.length === 0) {
    const message =
      closed.length === 0
        ? "Aucun trade clôturé"
        : `Pas assez de trades clôturés (> ${window} requis)`;
    return <EmptyCard label={`Facteur de profit · ${window}`} message={message} />;
  }

  const series = data.map((d) => d.pf);
  const last = series[series.length - 1];

  return (
    <ChartCard
      label={`Facteur de profit · ${window}`}
      value={last.toFixed(2)}
      data={series}
      refLines={[1]}
      yMin={0}
    />
  );
}
