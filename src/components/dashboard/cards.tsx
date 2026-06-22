import type * as React from "react";
import type { DashboardTrade } from "@/types";
import type { CardId } from "@/stores/dashboardStore";
import { KpiCard } from "./KpiCard";
import { PnlCurveCard } from "./PnlCurveCard";
import { RollingProfitFactorCard } from "./RollingProfitFactorCard";
import { JournalCard } from "./JournalCard";
import { InspirationCard, QuoteCard, HeadingCard } from "./MoodCards";

/** Registry of dashboard cards. To add a card: add its id to `CardId`, give it a
 *  default slot in `DEFAULT_LAYOUT`, and append an entry here. The grid + the
 *  show/hide dropdown pick it up automatically. */
export interface CardDef {
  id: CardId;
  title: string;
  render: (ctx: { trades: DashboardTrade[] }) => React.ReactNode;
}

export const CARD_DEFS: CardDef[] = [
  { id: "kpis", title: "KPI", render: ({ trades }) => <KpiCard trades={trades} /> },
  { id: "pnl-curve", title: "PnL cumulé", render: ({ trades }) => <PnlCurveCard trades={trades} /> },
  {
    id: "rolling-pf",
    title: "Facteur de profit · 20 trades",
    render: ({ trades }) => <RollingProfitFactorCard trades={trades} />,
  },
  { id: "journal", title: "Journal", render: () => <JournalCard /> },
  { id: "inspiration", title: "Inspiration", render: () => <InspirationCard /> },
  { id: "quote", title: "Citation", render: () => <QuoteCard /> },
  { id: "heading", title: "Titre (H1)", render: () => <HeadingCard /> },
];
