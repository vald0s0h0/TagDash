---
name: project_premarket_daily_bar_session56
description: Barre daily premarket — le chart daily affiche une bougie premarket 04:00–09:30 puis reset à l'ouverture 9:30
metadata:
  type: project
---

Session 56 — barre courante premarket sur les charts **daily uniquement**.

Pendant le premarket (04:00–09:30 ET) le chart daily montre une bougie premarket provisoire ; à 9:30 elle est **larguée** et la bougie de session régulière démarre fraîche depuis l'open (« se retire pour devenir la barre courante normale »).

**Implémentation (100% backend, le front rend ce que `get_bars` renvoie via `update()` sur la dernière barre) :**
- `time::is_premarket(instant)` = 240..570 ET (mirroir de `is_regular_session`).
- `MarketState.premarket_daily: HashMap<String, Bar>` — bougie premarket par symbole. Construite dans `ingest_trade` à partir des prints premarket SEULEMENT (l'agrégateur daily régulier continue d'ignorer le premarket via `!is_regular_session`). Larguée (`remove`) au 1er print non-premarket (= l'open). Reset si jour différent.
- `get_bars` (branche Daily) : `daily_forming = is_premarket(now) ? premarket_daily : agg.current_bar()` ; le reste (dedup par jour NY, fold dans la barre du jour) inchangé.
- `seed_premarket_daily(sym, bar)` : merge un agrégat Alpaca (open Alpaca + high/low/volume max, close live gardé). Évite que la barre soit incomplète si le chart s'ouvre en milieu de premarket.
- `alpaca::bars::fetch_premarket_daily_bar` : agrège les bougies 1-min Alpaca 04:00→cap(9:30) en 1 bougie daily stampée à minuit NY (« ajuster les appels api alpaca »).
- `load_chart_bars` : si Daily && premarket → après le merge history, fetch + `seed_premarket_daily` (lock relâché pendant l'await). Le refetch daily 30s rafraîchit la barre.
- `reset_data` clear `premarket_daily` (replay).

Pas de modif du CandleAggregator daily régulier (risque minimal). Test : `market_state::tests::daily_premarket_bar_then_resets_at_open` (pilote `replay::clock` avec Drop guard anti-fuite). Voir [[project_daily_session_and_chart_wheel_session51]] (daily = session régulière seule) et [[project_chart_bar_loading_session37]] (load_chart_bars / get_bars forming).
