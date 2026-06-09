---
name: project_bugfixes_session27
description: Lot de correctifs session 27 — news Alpaca, clic scanner, suppression alarme, scrollbar, + diagnostic recovery Opening Interest
metadata:
  type: project
---

Session 27 (2026-06-03) — lot de bug reports traités.

**Clic scanner → zone libre** ([ScannerAlerts.tsx](src/components/ScannerAlerts.tsx)) : le clic appelait seulement setSelectedTicker. Désormais `open()` = setSelectedTicker + `placeAlert` (1ère zone vide / nouvel onglet, no-op si déjà affiché). Prop `onSelect`→`onOpen(alert)`.

**News micro_pullback = Alpaca, plus Massive** ([enrichment/mod.rs](src-tauri/src/enrichment/mod.rs)) : Step 4 supprimé (polling Massive 3×13s qui renvoyait "no news" car Massive n'a pas de news). Remplacé par lecture RAM `market.latest_news(symbol)` (nouvelle méthode MarketState sur news_by_symbol, source que micro_pullback corrèle déjà cf [[project_micropullback_news_correlation_session25]]). `enrichment::run` prend désormais `market: Arc<RwLock<MarketState>>` (câblé dans start_alert_enrichment). news_item: NewsItem→NewsHeadline (.title→.headline). Massive garde seulement les splits (Step 3).

**Suppression alarme** ([AlarmsPanel.tsx](src/components/AlarmsPanel.tsx)) : icône poubelle (group-hover) à droite ; supprime toutes les alarmes (api.deleteAlarm) du ticker (ligne condensée par symbole) puis invalide ["all_alarms"]. Row = div (plus button-in-button).

**Startup load_daily lent** ([startup/mod.rs](src-tauri/src/startup/mod.rs) étape 7) : chaque upsert + chaque `UPDATE universe_assets SET avg_volume` était en auto-commit (= 1 fsync en WAL), et la boucle UPDATE balaie TOUT l'univers (~milliers de symboles) à CHAQUE démarrage → des milliers de fsync même en run incrémental quasi sans nouvelles barres. Fix : tout le bloc d'écritures encadré par `execute_batch("BEGIN")` … `execute_batch("COMMIT")` (1 seul commit). Logique inchangée, les lectures avg_volumes/latest_closes voient les upserts en cours dans la transaction. (rusqlite 0.32, db sous Mutex donc transaction sûre.)

**Scrollbar dark** ([index.css](src/index.css)) : `*` scrollbar-width thin + scrollbar-color (Firefox) + ::-webkit-scrollbar 8px thumb hsl(--border)→hover --muted-foreground, track transparent, scrollbar-button display:none (pas de flèches).

**Opening Interest recovery (IMPLÉMENTÉ)** : problème = démarrage après 9h30 → gate1 exige la bougie 9h30 (`is_opening_bar`) absente de la RAM (closed_bars = post-démarrage only) → tous `rejected_late`, 0 alerte. Fix = backfill + replay : nouvelle `alpaca::bars::fetch_intraday_bars_today` (1Min du jour 13:30Z→now, renvoie `Vec<Bar>` avec vwap `vw`+trade_count `n`) ; dans `build_watchlist` étape e), si `!mock && et_minutes(now) > WINDOW_START_MIN`, fetch puis `replay_backfill` qui rejoue les bougies 1 par 1 (chaque close/vwap = prix courant) via `process_variant(..., replay=true)`. Nouveau param `replay: bool` sur process_variant : reconstruit l'état (gate1/run/pullback/cooldown) mais `if !replay { fire = ... }` → **n'alerte JAMAIS sur le passé** (choix user), seul un pullback live frais fire. Lock state scopé avant le fetch async. Voir [[project_opening_interest_session24]].
