// Apex's Settings block — rendered under Micro Pullback, the strategy it belongs to.
//
// Apex is armed from the keyboard only (no toolbar button yet). Armed, the backend
// signs every print of the ticker and fires a MARKET order the moment a packet of
// prints leans hard one way, with a bracket built on ATR(14, 10 s) — one multiplier
// per side. The exit can then be handed back to the tape (TP dynamique) and the stop
// can follow the move (SL suiveur); both are decisions TagDash makes live, which is
// why each says so in its own hint.
// Every change commits immediately and reaches the flow book on the next print.

import { Row, Num } from "@/components/settings/MicroPullbackPanel";
import { useApexOptions, useSetApexOptions, useStrategies } from "@/queries/useScanner";
import { useHotkeyStore, bindingLabel } from "@/stores/hotkeyStore";
import { useApexStore } from "@/stores/apexStore";
import type { ApexOptions, ApexPhase } from "@/types";

const PHASE_LABELS: Record<ApexPhase, string> = {
  disabled: "désactivé",
  idle: "au repos",
  armed: "ARMÉ",
  firing: "envoi de l'ordre…",
  in_trade: "trade en cours",
};

export function ApexPanel() {
  const { data: options } = useApexOptions();
  const setOptions = useSetApexOptions();
  const { data: strategies } = useStrategies();
  const binding = useHotkeyStore((s) => s.bindings.apex_arm);
  const status = useApexStore((s) => s.status);

  if (!options) return null;
  const busy = setOptions.isPending;
  const off = busy || !options.enabled;

  function patch(partial: Partial<ApexOptions>) {
    setOptions.mutate({ ...options!, ...partial });
  }
  const num = (key: keyof ApexOptions, transform = (n: number) => n) =>
    (raw: string) => {
      const n = parseFloat(raw);
      if (Number.isFinite(n)) patch({ [key]: transform(n) } as Partial<ApexOptions>);
    };

  const mpRisk = strategies?.find((s) => s.id === "micro_pullback")?.risk.max_risk_dollars;
  const apexRisk = mpRisk != null ? (mpRisk * options.risk_pct) / 100 : null;
  const split = Math.round(50 + options.imbalance_pct / 2);
  const exitSplit = Math.round(50 + options.exit_imbalance_pct / 2);
  // What the trailing start means in money, when an ATR is on screen.
  const trailStartCents =
    status?.sl_distance != null
      ? `${(status.sl_distance * options.sl_trail_start_r).toFixed(2)} $`
      : null;
  const hotkey = binding ? bindingLabel(binding) : null;

  return (
    <div className="mt-2 space-y-3 rounded border border-violet-500/30 bg-violet-500/5 p-2.5">
      <div className="space-y-1">
        <div className="flex items-center justify-between gap-2">
          <div className="text-[11px] font-medium">Apex</div>
          {status && (
            <span
              className={`rounded px-1.5 py-0.5 text-[9px] tabular-nums ${
                status.phase === "armed"
                  ? "bg-violet-500/25 text-violet-200"
                  : "bg-muted text-muted-foreground"
              }`}
            >
              {PHASE_LABELS[status.phase]}
              {status.symbol && status.phase !== "idle" && status.phase !== "disabled"
                ? ` · ${status.symbol}`
                : ""}
              {status.phase === "in_trade" && status.stop_price != null
                ? ` · stop ${status.stop_price.toFixed(2)}${status.trail_active ? " ↗" : ""}`
                : ""}
            </span>
          )}
        </div>
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          Trade éclair sur la balance des prints. Armé, un paquet de prints orienté envoie
          immédiatement un ordre <strong>market</strong> dans le même sens, avec son
          bracket ATR. Armer / désarmer :{" "}
          {hotkey ? (
            <kbd className="rounded bg-muted px-1 font-mono text-[10px] text-foreground/80">{hotkey}</kbd>
          ) : (
            <span className="text-amber-500/90">aucune touche assignée</span>
          )}{" "}
          (Réglages → Clavier) sur la zone survolée. Une seconde pression désarme tant
          qu'aucun trade Apex n'est ouvert. TradeTally reçoit le setup « Apex ».
        </div>
      </div>

      <div className="space-y-1.5 border-t border-violet-500/20 pt-2">
        <Row
          checked={options.enabled}
          disabled={busy}
          label="Activer Apex"
          hint="Décoché : aucune surveillance des prints n'est calculée et la touche n'arme plus. Coché, la balance des tickers affichés est tenue en continu pour qu'Apex parte à chaud."
          onChange={(v) => patch({ enabled: v })}
        />
        <Row
          checked={options.long_enabled}
          disabled={off}
          label="Long"
          hint="Paquet de prints acheteurs → achat market."
          onChange={(v) => patch({ long_enabled: v })}
        />
        <Row
          checked={options.short_enabled}
          disabled={off}
          label="Short"
          hint="Paquet de prints vendeurs → vente à découvert market."
          onChange={(v) => patch({ short_enabled: v })}
        />
        {options.enabled && !options.long_enabled && !options.short_enabled && (
          <div className="text-[10px] text-amber-500/90">
            Aucune direction active : Apex ne peut pas être armé.
          </div>
        )}
      </div>

      <div className="space-y-1.5 border-t border-violet-500/20 pt-2">
        <div className="text-[11px] font-medium">TP / SL et taille</div>
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          Les deux distances sortent du même ATR(14) en barres 10 s, chacune avec son
          propre multiplicateur : à 1,0 / 1,0 le bracket est symétrique.
        </div>
        <Num
          label="Distance TP"
          value={String(options.tp_atr_multiplier)}
          suffix={options.tp_dynamic ? "× ATR — inutilisé (TP dynamique)" : "× ATR(14) en barres 10 s"}
          step="0.1"
          min={0.1}
          max={10}
          disabled={off || options.tp_dynamic}
          onCommit={num("tp_atr_multiplier")}
        />
        <Num
          label="Distance SL"
          value={String(options.sl_atr_multiplier)}
          suffix="× ATR(14) — c'est aussi la base de la taille"
          step="0.1"
          min={0.1}
          max={10}
          disabled={off}
          onCommit={num("sl_atr_multiplier")}
        />
        <Num
          label="Distance minimum"
          value={String(options.min_distance_cents)}
          suffix="cents — en dessous, trade non pris"
          step="1"
          min={0}
          disabled={off}
          onCommit={num("min_distance_cents")}
        />
        <Num
          label="Risque"
          value={String(options.risk_pct)}
          suffix={
            apexRisk != null
              ? `% du risque Micro Pullback (${apexRisk.toFixed(0)} $ sur ${mpRisk!.toFixed(0)} $)`
              : "% du risque Micro Pullback"
          }
          step="5"
          min={1}
          max={100}
          disabled={off}
          onCommit={num("risk_pct")}
        />
      </div>

      <div className="space-y-1.5 border-t border-violet-500/20 pt-2">
        <div className="text-[11px] font-medium">TP dynamique</div>
        <Row
          checked={options.tp_dynamic}
          disabled={off}
          label="Sortie sur retournement de la tape"
          hint="Coché : aucun ordre de TP n'est posé. La position est fermée au marché (limite marketable en pré/post-marché) dès que la balance des prints passe à l'opposé du trade, ou qu'un gros participant inverse imprime. La sortie n'attend pas d'être en profit. Le SL, lui, reste posé."
          onChange={(v) => patch({ tp_dynamic: v })}
        />
        <Num
          label="Déséquilibre inverse"
          value={String(options.exit_imbalance_pct)}
          suffix={`% — soit au moins ${exitSplit}/${100 - exitSplit} contre le trade`}
          step="5"
          min={0}
          max={100}
          disabled={off || !options.tp_dynamic}
          onCommit={num("exit_imbalance_pct")}
        />
        <Num
          label="Prints minimum"
          value={String(options.exit_min_prints)}
          suffix="dans la fenêtre du déclencheur"
          min={1}
          disabled={off || !options.tp_dynamic}
          onCommit={num("exit_min_prints", (n) => Math.round(n))}
        />
        <Num
          label="Gros participant"
          value={String(options.exit_block_ratio)}
          suffix="× la taille habituelle d'un print du ticker"
          step="1"
          min={1}
          max={100}
          disabled={off || !options.tp_dynamic}
          onCommit={num("exit_block_ratio")}
        />
        <Num
          label="…et au moins"
          value={String(options.exit_block_min_shares)}
          suffix="actions sur ce print"
          step="100"
          min={0}
          disabled={off || !options.tp_dynamic}
          onCommit={num("exit_block_min_shares", (n) => Math.round(n))}
        />
        {options.tp_dynamic && (
          <div className="text-[10px] leading-snug text-amber-500/90">
            Rien ne remplace le TP chez le broker : si TagDash se ferme, la position
            n'a plus que son stop.
          </div>
        )}
      </div>

      <div className="space-y-1.5 border-t border-violet-500/20 pt-2">
        <div className="text-[11px] font-medium">SL suiveur</div>
        <Row
          checked={options.sl_trailing}
          disabled={off}
          label="Le stop suit le mouvement"
          hint="Coché : une fois le trade en profit du seuil ci-dessous, le stop suit le meilleur prix atteint, à la distance indiquée, et ne redescend jamais."
          onChange={(v) => patch({ sl_trailing: v })}
        />
        <Num
          label="Départ"
          value={String(options.sl_trail_start_r)}
          suffix={
            trailStartCents != null
              ? `R de profit (≈ ${trailStartCents} avec le SL actuel)`
              : "R de profit — 0 = dès le premier tick"
          }
          step="0.1"
          min={0}
          max={10}
          disabled={off || !options.sl_trailing}
          onCommit={num("sl_trail_start_r")}
        />
        <Num
          label="Distance"
          value={String(options.sl_trail_atr_multiplier)}
          suffix="× ATR derrière le meilleur prix"
          step="0.1"
          min={0.1}
          max={10}
          disabled={off || !options.sl_trailing}
          onCommit={num("sl_trail_atr_multiplier")}
        />
        <Num
          label="Pas minimum"
          value={String(options.sl_trail_step_cents)}
          suffix="cents — en dessous le stop ne bouge pas"
          step="0.5"
          min={0.1}
          max={100}
          disabled={off || !options.sl_trailing}
          onCommit={num("sl_trail_step_cents")}
        />
        {options.sl_trailing && (
          <div className="text-[10px] leading-snug text-muted-foreground/70">
            Broker interne (et replay) : c'est l'ordre stop lui-même qui est déplacé.
            Avec Alpaca, le stop du broker reste au niveau de départ comme filet, et
            c'est TagDash qui sort au marché quand son niveau suiveur est traversé —
            donc l'app doit rester ouverte.
          </div>
        )}
      </div>

      <div className="space-y-1.5 border-t border-violet-500/20 pt-2">
        <div className="text-[11px] font-medium">Déclencheur</div>
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          Chaque print est signé acheteur ou vendeur (règle du quote sur SIP et flat files
          trades, règle du tick sur les agrégats seconde Massive). Réglé large par
          défaut : armer est déjà la décision, le tape n'a qu'à confirmer.
        </div>
        <Num
          label="Fenêtre"
          value={String(options.window_ms)}
          suffix="ms glissantes"
          step="100"
          min={200}
          max={10000}
          disabled={off}
          onCommit={num("window_ms", (n) => Math.round(n))}
        />
        <Num
          label="Prints minimum"
          value={String(options.min_prints)}
          suffix="dans la fenêtre"
          min={1}
          disabled={off}
          onCommit={num("min_prints", (n) => Math.round(n))}
        />
        <Num
          label="Déséquilibre"
          value={String(options.imbalance_pct)}
          suffix={`% — soit au moins ${split}/${100 - split}`}
          step="5"
          min={0}
          max={100}
          disabled={off}
          onCommit={num("imbalance_pct")}
        />
        <Num
          label="Rafale"
          value={String(options.burst_ratio)}
          suffix="× le rythme habituel du ticker"
          step="0.5"
          min={0}
          max={50}
          disabled={off}
          onCommit={num("burst_ratio")}
        />
      </div>
    </div>
  );
}
