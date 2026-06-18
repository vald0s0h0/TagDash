import { CartesianGrid, Line, LineChart, ReferenceLine, YAxis } from "recharts";
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart";
import type { DashboardTrade } from "@/types";
import { closedTrades, rollingProfitFactor } from "./kpis";

const WINDOW = 20;

const config = {
  pf: { label: `Facteur de profit (${WINDOW})`, color: "hsl(var(--chart-1))" },
} satisfies ChartConfig;

/** Profit factor computed over a trailing window of trades — shows whether the
 *  edge is improving or decaying. The dashed line marks break-even (PF = 1). */
export function RollingProfitFactorCard({ trades }: { trades: DashboardTrade[] }) {
  const data = rollingProfitFactor(closedTrades(trades), WINDOW);

  if (data.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-xs text-foreground/50">
        Aucun trade clôturé
      </div>
    );
  }

  return (
    <ChartContainer config={config} className="aspect-auto h-full w-full">
      <LineChart data={data} margin={{ left: 4, right: 8, top: 8, bottom: 0 }}>
        <CartesianGrid vertical={false} />
        <YAxis
          width={32}
          tickLine={false}
          axisLine={false}
          domain={[0, "auto"]}
          tickFormatter={(v) => Number(v).toFixed(1)}
        />
        <ReferenceLine y={1} stroke="hsl(var(--muted-foreground))" strokeDasharray="3 3" />
        <ChartTooltip
          content={
            <ChartTooltipContent hideLabel formatter={(value) => Number(value).toFixed(2)} />
          }
        />
        <Line
          dataKey="pf"
          type="monotone"
          stroke="var(--color-pf)"
          strokeWidth={2}
          dot={false}
        />
      </LineChart>
    </ChartContainer>
  );
}
