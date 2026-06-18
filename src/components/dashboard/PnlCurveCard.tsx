import { Area, AreaChart, CartesianGrid, YAxis } from "recharts";
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart";
import type { DashboardTrade } from "@/types";
import { closedTrades, pnlCurve, formatMoney } from "./kpis";

/** Cumulative realized P&L (equity curve). Green when ending positive, red when
 *  negative — matching the app's gain/loss semantics. */
export function PnlCurveCard({ trades }: { trades: DashboardTrade[] }) {
  const data = pnlCurve(closedTrades(trades));

  if (data.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-xs text-foreground/50">
        Aucun trade clôturé
      </div>
    );
  }

  const positive = data[data.length - 1].cumulative >= 0;
  const color = positive ? "hsl(142 71% 45%)" : "hsl(0 72% 51%)";
  const config = {
    cumulative: { label: "PnL cumulé", color },
  } satisfies ChartConfig;

  return (
    <ChartContainer config={config} className="aspect-auto h-full w-full">
      <AreaChart data={data} margin={{ left: 4, right: 8, top: 8, bottom: 0 }}>
        <defs>
          <linearGradient id="fillPnl" x1="0" y1="0" x2="0" y2="1">
            <stop offset="5%" stopColor="var(--color-cumulative)" stopOpacity={0.35} />
            <stop offset="95%" stopColor="var(--color-cumulative)" stopOpacity={0.03} />
          </linearGradient>
        </defs>
        <CartesianGrid vertical={false} />
        <YAxis
          width={44}
          tickLine={false}
          axisLine={false}
          tickFormatter={(v) => formatMoney(Number(v))}
        />
        <ChartTooltip
          content={
            <ChartTooltipContent
              hideLabel
              formatter={(value) => formatMoney(Number(value))}
            />
          }
        />
        <Area
          dataKey="cumulative"
          type="monotone"
          stroke="var(--color-cumulative)"
          strokeWidth={2}
          fill="url(#fillPnl)"
        />
      </AreaChart>
    </ChartContainer>
  );
}
