// Settings → Broker. Picks WHERE orders go and against WHICH account.
//
// Three things are shown apart because they are apart, and conflating them is how
// you send a real order believing you're in a simulator:
//
//   1. the broker + account — chosen here;
//   2. the trading environment (paper vs real money) — chosen here too, but given
//      its own row, its own colour, and a confirmation before it can be armed;
//   3. the market-data subscription (IEX vs SIP) — NOT chosen here. It is detected
//      from the key and lives in Feed Diagnostics. A free IEX plan can trade every
//      ticker on a real account; the two have nothing to do with each other.
//
// Note on (3): Alpaca issues one key pair per account, serving trading AND market
// data, so the account picked here is also the one the data feed authenticates as.
// That decides WHICH key is used — never what may be traded with it.

import { useEffect, useState } from "react";
import { AlertTriangle, Check, CircleDollarSign, FlaskConical, Radio, ServerCog } from "lucide-react";
import { Button } from "@/components/ui/button";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import type { BrokerMirror, BrokerSelection } from "@/types";

const BROKERS = [
  {
    id: "tagdash" as const,
    label: "TagDash",
    blurb:
      "Simulateur interne. Les fills sont synthétiques (calculés sur le prix du " +
      "marché local), aucun ordre ne quitte la machine. Toujours en démo.",
  },
  {
    id: "alpaca" as const,
    label: "Alpaca",
    blurb:
      "Ordres réellement envoyés à Alpaca. Les positions et les ordres affichés " +
      "sont ceux du serveur Alpaca, pas une copie locale : les exécutions arrivent " +
      "par le flux trade_updates et sont réconciliées toutes les 15 s.",
  },
];

export function BrokerTab() {
  const [sel, setSel] = useState<BrokerSelection | null>(null);
  const [mirror, setMirror] = useState<BrokerMirror | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [confirmLive, setConfirmLive] = useState(false);
  const [busy, setBusy] = useState(false);

  const refresh = () => {
    api.getBrokerSelection().then(setSel).catch((e) => setErr(String(e)));
    api.getBrokerMirror().then(setMirror).catch(() => {});
  };

  useEffect(() => {
    refresh();
    const id = window.setInterval(() => {
      api.getBrokerMirror().then(setMirror).catch(() => {});
    }, 2000);
    return () => window.clearInterval(id);
  }, []);

  const apply = async (broker: string, environment: string) => {
    setErr(null);
    setBusy(true);
    try {
      setSel(await api.setBrokerSelection(broker, environment));
      setConfirmLive(false);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  if (!sel) {
    return <div className="text-xs text-muted-foreground">Chargement…</div>;
  }

  const isAlpaca = sel.broker === "alpaca";
  const orders = Object.values(mirror?.orders ?? {});
  const positions = Object.values(mirror?.positions ?? {});

  return (
    <div className="space-y-4">
      {/* ── Live-money banner: the single most important thing on this screen ── */}
      {sel.is_live && (
        <div className="flex items-start gap-2 rounded-md border border-red-600/60 bg-red-950/40 p-3">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-red-400" />
          <div className="text-xs">
            <div className="font-semibold text-red-300">COMPTE RÉEL ARMÉ</div>
            <p className="mt-0.5 text-red-200/80">
              Chaque ordre envoyé engage de l'argent réel. Les fills proviennent du
              marché, pas du simulateur.
            </p>
          </div>
        </div>
      )}

      {/* ── 1. Broker ── */}
      <div className="rounded-md border border-border p-3">
        <div className="mb-2 flex items-center gap-1.5 text-sm font-medium">
          <ServerCog className="h-4 w-4" /> Broker
        </div>
        <div className="space-y-2">
          {BROKERS.map((b) => (
            <button
              key={b.id}
              disabled={busy}
              onClick={() => apply(b.id, b.id === "tagdash" ? "paper" : sel.environment)}
              className={cn(
                "flex w-full items-start gap-2 rounded border p-2.5 text-left transition-colors",
                sel.broker === b.id
                  ? "border-blue-500/60 bg-blue-950/30"
                  : "border-border/50 hover:bg-accent/40",
              )}
            >
              <div className="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center">
                {sel.broker === b.id && <Check className="h-3.5 w-3.5 text-blue-400" />}
              </div>
              <div className="min-w-0">
                <div className="text-xs font-medium">{b.label}</div>
                <p className="mt-0.5 text-[11px] leading-snug text-muted-foreground">{b.blurb}</p>
              </div>
            </button>
          ))}
        </div>
      </div>

      {/* ── 2. Environment ── */}
      <div className="rounded-md border border-border p-3">
        <div className="mb-1 flex items-center gap-1.5 text-sm font-medium">
          <CircleDollarSign className="h-4 w-4" /> Environnement de trading
        </div>
        <p className="mb-2.5 text-[11px] text-muted-foreground">
          Indépendant du broker et des données de marché. Le simulateur TagDash n'a
          pas de contrepartie réelle — il reste en démo quoi qu'il arrive.
        </p>

        <div className="flex gap-2">
          <button
            disabled={busy || !isAlpaca}
            onClick={() => apply(sel.broker, "paper")}
            className={cn(
              "flex flex-1 items-center justify-center gap-1.5 rounded border px-3 py-2 text-xs transition-colors",
              sel.environment === "paper" || !isAlpaca
                ? "border-emerald-600/60 bg-emerald-950/30 text-emerald-200"
                : "border-border/50 text-muted-foreground hover:bg-accent/40",
              !isAlpaca && "opacity-70",
            )}
          >
            <FlaskConical className="h-3.5 w-3.5" /> Démo (paper money)
          </button>
          <button
            disabled={busy || !isAlpaca}
            onClick={() => (sel.is_live ? undefined : setConfirmLive(true))}
            className={cn(
              "flex flex-1 items-center justify-center gap-1.5 rounded border px-3 py-2 text-xs transition-colors",
              sel.is_live
                ? "border-red-600/70 bg-red-950/40 font-semibold text-red-200"
                : "border-border/50 text-muted-foreground hover:bg-accent/40",
              !isAlpaca && "cursor-not-allowed opacity-40",
            )}
          >
            <AlertTriangle className="h-3.5 w-3.5" /> Réel (argent réel)
          </button>
        </div>

        {!isAlpaca && (
          <p className="mt-2 text-[11px] text-muted-foreground">
            Le broker TagDash ne peut pas passer en réel.
          </p>
        )}

        {/* Arming a real account is a deliberate, two-step act. */}
        {confirmLive && (
          <div className="mt-3 rounded border border-red-600/60 bg-red-950/30 p-2.5">
            <p className="text-xs text-red-200">
              Passer sur le compte <strong>RÉEL</strong> Alpaca ? Les ordres suivants
              engageront de l'argent réel.
            </p>
            <div className="mt-2 flex justify-end gap-2">
              <Button variant="ghost" size="sm" onClick={() => setConfirmLive(false)}>
                Annuler
              </Button>
              <Button
                size="sm"
                variant="destructive"
                disabled={busy}
                onClick={() => apply("alpaca", "live")}
              >
                Oui, activer le compte réel
              </Button>
            </div>
          </div>
        )}
      </div>

      {/* ── 3. What is NOT decided here ── */}
      <div className="rounded-md border border-border/50 p-3">
        <div className="mb-1 flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
          <Radio className="h-3.5 w-3.5" /> Données de marché — réglage distinct
        </div>
        <p className="text-[11px] leading-snug text-muted-foreground">
          L'abonnement IEX / SIP est détecté automatiquement depuis la clé et
          n'influence pas le trading : tu peux trader n'importe quel ticker avec des
          données IEX. Voir Réglages → Diagnostics du flux.
        </p>
        <p className="mt-1.5 text-[11px] leading-snug text-muted-foreground">
          La paire de clés d'un compte Alpaca sert aussi bien au trading qu'aux
          données, donc le compte choisi ci-dessus est celui dont le flux de marché
          s'authentifie. Changer de compte peut changer l'abonnement détecté —
          <strong> redémarre TagDash</strong> pour l'appliquer partout.
        </p>
      </div>

      {/* ── Server-side truth ── */}
      {isAlpaca && (
        <div className="rounded-md border border-border p-3">
          <div className="mb-2 flex items-center justify-between">
            <div className="text-sm font-medium">État du compte Alpaca</div>
            <span
              className={cn(
                "rounded px-1.5 py-0.5 text-[10px]",
                mirror?.stream_connected
                  ? "bg-emerald-950/50 text-emerald-300"
                  : "bg-amber-950/50 text-amber-300",
              )}
            >
              {mirror?.stream_connected ? "flux exécutions connecté" : "flux hors ligne"}
            </span>
          </div>

          {!sel.credentials_ready && (
            <p className="mb-2 text-[11px] text-amber-400">
              Clés de trading {sel.environment === "live" ? "réelles" : "paper"} absentes
              — Réglages → Clés API.
            </p>
          )}

          <dl className="space-y-1 text-[11px]">
            <Row label="Compte" value={sel.account_id} />
            <Row label="Statut" value={mirror?.account?.status ?? "—"} />
            <Row
              label="Equity"
              value={
                mirror?.account?.equity != null
                  ? `${mirror.account.equity.toLocaleString()} ${mirror.account.currency}`
                  : "—"
              }
            />
            <Row
              label="Buying power"
              value={mirror?.account?.buying_power?.toLocaleString() ?? "—"}
            />
            <Row label="Ordres actifs (serveur)" value={String(orders.length)} />
            <Row label="Positions (serveur)" value={String(positions.length)} />
            <Row
              label="Dernière réconciliation"
              value={
                mirror?.last_reconcile
                  ? new Date(mirror.last_reconcile).toLocaleTimeString()
                  : "jamais"
              }
            />
          </dl>

          {mirror?.account?.trading_blocked && (
            <p className="mt-2 text-[11px] text-red-400">
              Alpaca a bloqué le trading sur ce compte — les ordres seront refusés.
            </p>
          )}
          {mirror?.account?.trade_suspended_by_user && (
            <p className="mt-2 text-[11px] text-amber-400">
              Trading suspendu depuis ton tableau de bord Alpaca — tout nouvel ordre
              est refusé (code 40310000). Réactive-le chez Alpaca pour trader.
            </p>
          )}
          {mirror?.last_error && (
            <p className="mt-2 break-words text-[11px] text-amber-400">{mirror.last_error}</p>
          )}
        </div>
      )}

      {err && <p className="text-xs text-red-400">{err}</p>}
    </div>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between gap-3">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="truncate font-mono text-foreground">{value}</dd>
    </div>
  );
}
