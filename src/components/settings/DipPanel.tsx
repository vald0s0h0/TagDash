// Dip's Settings block — rendered under Micro Pullback, beside Apex's, the strategy
// both engines belong to. Green where Apex is violet: same strategy, opposite idea.
//
// Apex reads the tape and asks which way it is going. Dip reads a PLACE the trader
// chose in advance and asks whether the strike landed there. The trader lays a thick
// band on a price with a hotkey, tunes it one notch at a time, and the band trades
// itself: a violent penetration followed by a turn sends a MARKET order with a
// bracket built on ATR(14, 10 s) — the very reading Apex uses, one multiplier per side.
//
// The two speed settings are the heart of it, and they are a BAND, not a floor. Under
// the floor nothing happened. Over the ceiling the move is out of reach and is let go
// on purpose: an order sent into it would be filled somewhere else entirely.
// Every change commits immediately and reaches the watch book on the next print.

import { Row, Num } from "@/components/settings/MicroPullbackPanel";
import { useDipOptions, useSetDipOptions, useStrategies } from "@/queries/useScanner";
import { useHotkeyStore, bindingLabel } from "@/stores/hotkeyStore";
import { useDipStore } from "@/stores/dipStore";
import type { DipOptions, DipPhase } from "@/types";

const PHASE_LABELS: Record<DipPhase, string> = {
  disabled: "désactivé",
  idle: "au repos",
  armed: "ZONE POSÉE",
  firing: "envoi de l'ordre…",
  in_trade: "trade en cours",
};

export function DipPanel() {
  const { data: options } = useDipOptions();
  const setOptions = useSetDipOptions();
  const { data: strategies } = useStrategies();
  const bindings = useHotkeyStore((s) => s.bindings);
  const status = useDipStore((s) => s.status);

  if (!options) return null;
  const busy = setOptions.isPending;
  const off = busy || !options.enabled;

  function patch(partial: Partial<DipOptions>) {
    setOptions.mutate({ ...options!, ...partial });
  }
  const num = (key: keyof DipOptions, transform = (n: number) => n) =>
    (raw: string) => {
      const n = parseFloat(raw);
      if (Number.isFinite(n)) patch({ [key]: transform(n) } as Partial<DipOptions>);
    };

  const mpRisk = strategies?.find((s) => s.id === "micro_pullback")?.risk.max_risk_dollars;
  const dipRisk = mpRisk != null ? (mpRisk * options.risk_pct) / 100 : null;
  const key = (id: "dip_place" | "dip_thinner" | "dip_thicker") => {
    const b = bindings[id];
    return b ? bindingLabel(b) : null;
  };
  const place = key("dip_place");
  const half = (options.step_cents / 2).toFixed(options.step_cents % 2 === 0 ? 0 : 1);
  // What the speed window means in money, when an ATR is on screen.
  const ordinary =
    status?.atr != null
      ? `${(status.atr * (options.speed_window_ms / 10000)).toFixed(3)} $`
      : null;

  const Key = ({ label }: { label: string | null }) =>
    label ? (
      <kbd className="rounded bg-muted px-1 font-mono text-[10px] text-foreground/80">{label}</kbd>
    ) : (
      <span className="text-amber-500/90">non assignée</span>
    );

  return (
    <div className="mt-2 space-y-3 rounded border border-emerald-500/30 bg-emerald-500/5 p-2.5">
      <div className="space-y-1">
        <div className="flex items-center justify-between gap-2">
          <div className="text-[11px] font-medium">Dip</div>
          {status && (
            <span
              className={`rounded px-1.5 py-0.5 text-[9px] tabular-nums ${
                status.attacking
                  ? "bg-emerald-400/35 text-emerald-100"
                  : status.phase === "armed"
                    ? "bg-emerald-500/25 text-emerald-200"
                    : "bg-muted text-muted-foreground"
              }`}
            >
              {status.attacking ? "ATTAQUE" : PHASE_LABELS[status.phase]}
              {status.symbol && status.phase !== "idle" && status.phase !== "disabled"
                ? ` · ${status.symbol}`
                : ""}
              {status.phase === "armed" && status.low != null && status.high != null
                ? ` · ${status.low.toFixed(2)}–${status.high.toFixed(2)}`
                : ""}
              {status.attacking ? ` · ${status.speed_ratio.toFixed(1)}×` : ""}
            </span>
          )}
        </div>
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          Le niveau anticipé. <Key label={place} /> pose une zone verte épaisse sur le prix
          sous le curseur ; elle devient un <strong>support</strong> si le prix est
          au-dessus, une <strong>résistance</strong> s'il est en dessous. <Key label={key("dip_thinner")} />{" "}
          et <Key label={key("dip_thicker")} /> l'affinent ou l'épaississent de {options.step_cents} ¢
          ({half} ¢ de chaque côté), sans jamais déplacer le niveau. <Key label={place} /> à
          nouveau <em>à l'intérieur</em> de la zone la retire ; ailleurs, elle la déplace.
          Puis le script est armé : il attend une pénétration violente suivie d'un demi-tour,
          et envoie l'ordre <strong>market</strong> dès que le demi-tour commence.
          TradeTally reçoit le setup « Dip ».
        </div>
      </div>

      <div className="space-y-1.5 border-t border-emerald-500/20 pt-2">
        <Row
          checked={options.enabled}
          disabled={busy}
          label="Activer Dip"
          hint="Décoché : aucun chemin de prix n'est suivi et la touche ne pose plus de zone. Coché, le chemin des tickers affichés est tenu en continu pour qu'une zone posée mesure une vitesse dès la première seconde."
          onChange={(v) => patch({ enabled: v })}
        />
        <Row
          checked={options.long_enabled}
          disabled={off}
          label="Long (support)"
          hint="Zone sous le prix : le flush y est acheté sur le demi-tour."
          onChange={(v) => patch({ long_enabled: v })}
        />
        <Row
          checked={options.short_enabled}
          disabled={off}
          label="Short (résistance)"
          hint="Zone au-dessus du prix : la poussée y est vendue à découvert sur le demi-tour."
          onChange={(v) => patch({ short_enabled: v })}
        />
        {options.enabled && !options.long_enabled && !options.short_enabled && (
          <div className="text-[10px] text-amber-500/90">
            Aucune direction active : aucune zone ne peut être posée.
          </div>
        )}
      </div>

      <div className="space-y-1.5 border-t border-emerald-500/20 pt-2">
        <div className="text-[11px] font-medium">La zone</div>
        <Num
          label="Épaisseur"
          value={String(options.thickness_cents)}
          suffix={`cents au total — ${(options.thickness_cents / 2).toFixed(1)} ¢ de chaque côté du niveau`}
          step="1"
          min={1}
          max={500}
          disabled={off}
          onCommit={num("thickness_cents")}
        />
        <Num
          label="Pas"
          value={String(options.step_cents)}
          suffix={`cents par pression de ${key("dip_thinner") ?? "—"} / ${key("dip_thicker") ?? "—"}`}
          step="1"
          min={0.2}
          max={50}
          disabled={off}
          onCommit={num("step_cents")}
        />
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          L'épaisseur réglée au clavier sur une zone devient l'épaisseur de la
          suivante : régler une zone, c'est régler l'outil.
        </div>
      </div>

      <div className="space-y-1.5 border-t border-emerald-500/20 pt-2">
        <div className="text-[11px] font-medium">TP / SL et taille</div>
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          Les deux distances sortent du même ATR(14) en barres 10 s qu'Apex, chacune
          avec son propre multiplicateur : à 1,0 / 1,0 le bracket est symétrique. Les
          deux ordres restent posés chez le broker — un trade Dip n'a pas besoin que
          TagDash reste ouvert.
        </div>
        <Num
          label="Distance TP"
          value={String(options.tp_atr_multiplier)}
          suffix="× ATR(14) en barres 10 s"
          step="0.1"
          min={0.1}
          max={10}
          disabled={off}
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
          suffix="cents — en dessous, la zone reste en veille"
          step="1"
          min={0}
          disabled={off}
          onCommit={num("min_distance_cents")}
        />
        <Num
          label="Risque"
          value={String(options.risk_pct)}
          suffix={
            dipRisk != null
              ? `% du risque Micro Pullback (${dipRisk.toFixed(0)} $ sur ${mpRisk!.toFixed(0)} $)`
              : "% du risque Micro Pullback"
          }
          step="5"
          min={1}
          max={100}
          disabled={off}
          onCommit={num("risk_pct")}
        />
      </div>

      <div className="space-y-1.5 border-t border-emerald-500/20 pt-2">
        <div className="text-[11px] font-medium">L'attaque</div>
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          La vitesse est <strong>relative au ticker lui-même</strong> : ce qu'il parcourt
          d'ordinaire dans la même durée, lu sur l'ATR(14) en barres 10 s. 1,0× = son
          rythme habituel. C'est une fourchette, pas un plancher — au-dessus du plafond
          le mouvement est hors de portée et il est laissé passer volontairement : la
          latence ne permet pas de l'attraper, l'ordre serait exécuté bien plus loin.
        </div>
        <Num
          label="Fenêtre"
          value={String(options.speed_window_ms)}
          suffix={
            ordinary != null
              ? `ms glissantes (≈ ${ordinary} de trajet ordinaire)`
              : "ms glissantes — micro-structure, une fraction de barre 10 s"
          }
          step="100"
          min={200}
          max={10000}
          disabled={off}
          onCommit={num("speed_window_ms", (n) => Math.round(n))}
        />
        <Num
          label="Vitesse minimum"
          value={String(options.min_speed_ratio)}
          suffix="× le trajet habituel du ticker sur la fenêtre"
          step="0.5"
          min={0}
          max={100}
          disabled={off}
          onCommit={num("min_speed_ratio")}
        />
        <Num
          label="Vitesse maximum"
          value={String(options.max_speed_ratio)}
          suffix="× — au-delà, hors de portée : attaque abandonnée"
          step="1"
          min={0.1}
          max={500}
          disabled={off}
          onCommit={num("max_speed_ratio")}
        />
        {options.max_speed_ratio <= options.min_speed_ratio && (
          <div className="text-[10px] text-amber-500/90">
            Plafond au niveau du plancher : aucune attaque ne peut passer.
          </div>
        )}
        <Num
          label="Pénétration min."
          value={String(options.min_penetration_cents)}
          suffix="cents dans la zone — un print qui effleure le bord ne compte pas"
          step="1"
          min={0}
          disabled={off}
          onCommit={num("min_penetration_cents")}
        />
        <Num
          label="Durée max."
          value={String(options.attack_max_ms)}
          suffix="ms — au-delà le prix stationne, ce n'est plus une attaque"
          step="1000"
          min={500}
          max={120000}
          disabled={off}
          onCommit={num("attack_max_ms", (n) => Math.round(n))}
        />
      </div>

      <div className="space-y-1.5 border-t border-emerald-500/20 pt-2">
        <div className="text-[11px] font-medium">Le demi-tour</div>
        <Num
          label="Retournement"
          value={String(options.reversal_cents)}
          suffix="cents repris sur l'extrême — l'ordre part dès que ça tourne"
          step="0.5"
          min={0.1}
          max={100}
          disabled={off}
          onCommit={num("reversal_cents")}
        />
        <Num
          label="Invalidation"
          value={String(options.break_cents)}
          suffix="cents au-delà du bord opposé : le niveau est cassé, la zone est retirée"
          step="1"
          min={0}
          disabled={off}
          onCommit={num("break_cents")}
        />
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          Une attaque qui n'aboutit pas (trop rapide, ou qui s'installe dans la zone)
          ne retire pas la zone : elle se réarme dès que le prix en ressort. Seul un
          niveau traversé la fait disparaître.
        </div>
      </div>
    </div>
  );
}
