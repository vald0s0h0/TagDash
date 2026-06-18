---
name: project_micropullback_overlay_session59
description: Micro Pullback chart relayout (daily+5m left column, 10s right) + redesigned info overlay with value bars, DB risk scores and news freshness badges
metadata:
  type: project
---

Session 59 (2026-06-18) — Micro Pullback layout + rich info overlay.

**Pane columns** : ajout `PaneSpec.column: Option<u8>` (Rust + TS, `#[serde(default)]`/optionnel). Les panes partageant un `column` sont empilés verticalement (ordre de déclaration) ; sans `column` → chaque pane sa propre colonne = ancien côte-à-côte (rétro-compatible, toutes les autres stratégies passées à `column: None`). ChartZone groupe en `columns: number[][]` puis rend des colonnes flex (gap, border-l) contenant des panes flex-col empilés (border-t). Micro = daily(col0)+5m(col0) gauche empilés, 10s(col1) droite.

**Overlay** déplacé du pane gauche vers le pane **droit (interactif/sub-minute)** : `overlayPaneIdx = isMicro ? interactiveIdx : 0`. Nouveau composant `MicroInfoOverlay` (chartZoneParts.tsx) à côté du `StrategyInfoOverlay` générique (conservé pour les autres). Garde le look (bg-black/40 backdrop-blur, labels 9px uppercase muted, valeurs semibold tabular). Lignes haut→bas : Pays·Industrie · Float · Volume · Short int · Capa.dil · Besoin dil · Dil.hist · Pump&Dump · News score (placeholder vide grisé, pas encore câblé) · 4 dernières news.

**Barres** (grid aligné `[62px_1fr_40px]`) : bleu = liquidité/intérêt (float, volume, short int), rouge = risque dilution/manip. Décisions de normalisation : `floatFill` **inversé** (float bas = barre pleine, log 500K→50M, le chiffre exact reste affiché) ; `volumeFill` log 10K→5M ; scores 0..100 → fill = score/100 **sans afficher le nombre** (abstrait) ; float & volume **affichent** le nombre (fmtCompact). Absence de donnée → fill null → ligne grisée opacity-40.

**Données** : `CardInfo` (commands/mod.rs) étendu avec les 5 scores (`pump_dump_score`, `dilution_score`, `dilution_capacity_score`, `dilution_need_score`, `short_interest_score`, lus via nouveau `cache_repository::get_risk_scores` depuis `fundamentals_cache`) + `recent_news: Vec<CardNews{headline,created_at,source}>` (via nouveau `MarketState::recent_headlines(sym,4)`). Volume = `premarket_volume` déjà calculé en RAM (somme M1 04:00–09:30 incluant la barre en cours = tick-ajusté), pas en DB.

**News freshness** : badge calculé côté client depuis `created_at` (RFC3339), re-render chaque seconde (`useNowTick(1000)`) — "20sec"/"1min"/"2h", vert <20min / orange <60min / rouge sinon.

Screenshot capture (ChartZone) lit désormais `[data-cap-label]`/`[data-cap-value]` (fallback children[0]/[1] pour l'overlay générique). Voir [[project_info_bar_split_session48]] (ChartInfoBar commun conservé au-dessus) et [[project_startup_scores_dilution_session58]] (origine des scores).
