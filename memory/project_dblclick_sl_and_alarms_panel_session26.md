---
name: project_dblclick_sl_and_alarms_panel_session26
description: Raccourci SL double-clic sur le chart + section Alarmes condensée dans la sidebar
metadata:
  type: project
---

Session 26 (2026-06-03) — deux features trading UI.

**1. Raccourci SL double-clic** ([LightweightChart.tsx](src/components/LightweightChart.tsx) + [ChartZone.tsx](src/components/ChartZone.tsx))
Double-clic n'importe où sur le pane interactif = mémorise le prix comme SL provisoire (dessine la ligne SL immédiatement + persiste via `updateZoneSl`), sans armer l'outil SL au préalable. Débloque les boutons 25/50/100 (via `hasSl`). Re-double-clic déplace le SL. Implémenté avec `chart.subscribeDblClick` (lightweight-charts v4.2.3) + nouvelle prop `onSlDblClick` câblée sur `handleSlDragEnd` (identique au drag). Le reste du pipeline (sizing/sens long-short/ouverture) était déjà là via `createInternalMarketOrderPercent`.

**2. Section Alarmes** ([AlarmsPanel.tsx](src/components/AlarmsPanel.tsx), [Sidebar.tsx](src/components/Sidebar.tsx))
Liste condensée au-dessus de Positions/Ordres : 1 ligne/ticker (alarmes armées seulement, `triggered_at==null`, dédup par symbole en gardant la priorité max), badge PX + ticker. Clic → `openInActiveZone(alarmToAlert(a))` avec `session:"open"` → ouvre le chart dans l'onglet Open via la carte de stratégie.
Backend : `get_all_alarms` renvoie désormais `Vec<AlarmView>` (DTO enrichi avec `strategy_name`+`priority` dérivés du registry, défaut "Alarme"/5, comme le watcher d'alarmes [[project_preopen_screener_alarms_session21]]). Type front `AlarmView` ajouté. Compteur sidebar = nb de symboles distincts armés.

Voir [[project_charts_session8]] (SL/TP drag lines), [[project_internal_trading_session9]] (25/50/100), [[project_bugdb_alarms_session20]] (price_alarms en DB).
