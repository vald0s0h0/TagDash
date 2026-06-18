---
name: project-dashboard-moodboard-session60
description: KPI dashboard "moodboard" tab — glass cards on 10×6 grid, TradeTally trades DB, diary card, daily background
metadata:
  type: project
---

Session 60 — nouvel onglet **Dashboard** (moodboard KPI), premier du `LeftRail`, icône `Moon`.

**Vue** : `uiStore.activeView: "trading" | "dashboard"` (défaut trading). `App.tsx` : si `dashboard` → `<Dashboard/>` plein cadre **sans** `Sidebar`/`ReplayToolbar`/`MainWindow`. Bouton Moon en 1ʳᵉ position du rail (les session-tabs remettent `activeView="trading"`).

**Backend** :
- Tables `tt_trades` (miroir trades TradeTally, `raw_json` = objet complet pour compat future) + `diary_entries_local` (schema.rs).
- `local_db/dashboard_repository.rs` : upsert/bulk + `get_all_trades` (ordre exit/entry asc) + `insert_diary_local`.
- `dashboard/mod.rs` : `DashboardTrade`/`DailyBackground`, `sync_trades(client)` pagine `GET /api/v1/trades?page&limit=100`. **Envelope réel** = `{ data:[...], pagination:{ limit, offset, total, hasMore } }` (PAS `trades`/`totalPages`) ; champs trade en **strings** (pnl/quantity/prix), dates = `entry_time`/`exit_time`/`trade_date` (PAS entry_date/exit_date), **pas de champ status** (dérivé : exit_time présent ⇒ Closed). Pagination via `pagination.hasMore`. `pick_daily_background(app_dir)` = image déterministe/jour ET depuis `<app_dir>/backgrounds/` encodée **base64 data-URL** (encodeur maison, pas de crate), `open_folder` (explorer/open/xdg-open).
- Diary = endpoint réel `POST /api/diary` (mount `/api/diary`, **pas** v1 ; body camelCase `entryDate/title/content/entryType`, `createOrUpdateEntry`). Derrière middleware `authenticate` → `TtClient::create_diary_entry` essaie le token puis **fallback login JWT** (email/pwd) sur 401/403. Routé par la **file résiliente** : `tradetally::enqueue_diary_entry` → worker `dispatch` bras `"diary_entry"`.
- `TtClient` : ajout `get_json`, refactor `login_jwt` partagé (diary + upload screenshots).
- 5 commandes : `sync_tradetally_trades` (sync au tab open, source de vérité), `get_dashboard_trades`, `save_diary_entry` (entry_date = `time::et_date(now)`, enqueue + mirror local), `get_daily_background`, `open_backgrounds_folder`.

**Frontend** :
- KPI/séries calculés **côté front** (`components/dashboard/kpis.ts`) : `profitFactor`, `pnlCurve` (équity cumulée), `rollingProfitFactor` (fenêtre 20, no-loss clampé à cap), `summarize` (winRate/avgWin/avgLoss/expectancy…).
- Charts = **shadcn charts** (`components/ui/chart.tsx`) + **recharts pinné 2.15.4** (v3 cassait le tsc strict). Tokens `--chart-1..5` ajoutés dans `index.css`. Cartes : `PnlCurveCard` (AreaChart vert/rouge selon signe final), `RollingProfitFactorCard` (LineChart + ReferenceLine y=1), `KpiCard` (tuiles), `JournalCard` (diary).
- **Grille maison 10×6** invisible : `stores/dashboardStore.ts` (zustand `persist` localStorage `tagdash-dashboard`, `layout`+`editing`, merge defaults pour cartes futures), `GridCard.tsx` (drag header + grip resize en cellules via pointer events, clamp), registre `cards.tsx` (`CARD_DEFS` extensible). `Dashboard.tsx` : fond `object-cover object-center` + voile noir/40, contrôles discrets (dropdown : show/hide cartes, éditer, réinitialiser, rafraîchir, dossier des fonds + chemin).
- Classe CSS **`.glass-card`** (index.css) = effet verre, **uniquement** pour ce dashboard.

**Dossier fonds** : `%APPDATA%/tagdash/backgrounds/` (créé auto, chemin affiché + bouton "Dossier des fonds").

**À vérifier en run live** : que le token API passe sur `/api/v1/trades` (sinon ajuster), et que `POST /api/diary` accepte le token (sinon le fallback JWT exige `tradetally_email`/`tradetally_password` dans les secrets). Voir [[project-tradetally-v1-auth-update-session29]].

Cf. plan `.claude/plans/snoopy-whistling-hopcroft.md`. Lié à [[project-tradetally-v1-and-bracket-orders-session12]], [[project-titlebar-and-singlechart-session55]].
