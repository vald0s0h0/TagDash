# TagDash

Scanner trading live (Alpaca → RAM → Rust → UI) construit sur Tauri + React + Rust.

## Stack

- **Backend** : Rust 100 % (Tauri 2), Tokio, SQLite (à venir)
- **Frontend** : React 18 + TypeScript + Vite + Tailwind + shadcn/ui
- **State / data** : Zustand, TanStack Query, TanStack Table, TanStack Virtual
- **Charts** : TradingView Lightweight Charts
- **Icons** : lucide-react

## Prérequis (Windows)

1. **Node.js** ≥ 20
2. **Rust** (rustup) : `winget install Rustlang.Rustup`
3. **Visual Studio Build Tools 2022** avec workload `Desktop development with C++` + Windows 11 SDK

   ```powershell
   winget install Microsoft.VisualStudio.2022.BuildTools `
     --override "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --add Microsoft.VisualStudio.Component.Windows11SDK.22621"
   ```

## Prérequis (macOS)

1. **Node.js** ≥ 20 : `brew install node`
2. **Rust** (rustup) : `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
3. **Xcode Command Line Tools** : `xcode-select --install`

## Lancer en dev

```bash
npm install              # une fois
npm run tauri:dev        # lance Vite + Tauri (Rust compile la 1re fois)
```

Vite seul (sans la fenêtre Tauri) : `npm run dev` puis ouvre http://localhost:1420.

## Build de production

```powershell
npm run tauri:build
```

## Arborescence

```
src/                       Frontend React
  components/              UI shell (LeftRail, Sidebar, MainWindow, LogsPanel)
  stores/                  Zustand stores
  queries/                 Hooks TanStack Query
  charts/                  Wrappers TradingView Lightweight Charts (à venir)
  types/                   Types partagés (miroir des types Rust)
  lib/                     utils + bridge Tauri

src-tauri/                 Backend Tauri (Rust)
  src/
    commands/              Commandes Tauri exposées au front
    config/                Configuration runtime
    types/                 Types domaine (TickerState, AlertSignal, ...)
    local_db/              SQLite (univers, queue, cache, company_meta)  — non bloquant
    alpaca/                Client WebSocket + REST (assets, bars daily incrémental)
    massive/               Float en bulk (api.massive.com) — provider actif
    sec_api/               Pays d'origine + industrie SIC (sec-api.io) + table SIC
    fmp/                   FMP float (legacy / fallback, conservé)
    universe/              Filtres d'univers (small_caps, low_float, ...)
    market_state/          Cache RAM, ring buffers, source de vérité live
    scanner/               Engine stratégies → AlertSignal
    strategies/            Stratégies Rust compilées (trait ScanStrategy)
    internal_trading/      Ordres internes, positions, fills simulés
    tradetally/            Client API TradeTally (queue résiliente)
    screenshot/            Capture zone graphique
    llm/                   Client Claude (strictement async)
    chart_payloads/        Préparation payloads chart pour le front
```

## Chemin critique live

```
Alpaca → RAM (market_state) → scanner Rust → UI
```

SQLite et TradeTally ne doivent jamais bloquer cette chaîne.

## Commandes Tauri disponibles (test)

- `get_app_status()` — version, backend, latence
- `get_config()` — configuration runtime
- `get_mock_alerts()` — alertes mockées pour valider le bridge

## Pas en V1

- Bridge TWS/DAS
- Ordres broker réels
- Replay historique
- Docker / NAS DB / API NAS
