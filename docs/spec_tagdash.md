# Scanner multi-jours Trade Ideas

# Spécification technique — Scanner live Alpaca + Tauri + TradeTally

## 1. Objectif du projet

Créer une application desktop de scanner trading temps réel, compatible Windows et macOS, basée sur Tauri, permettant de recevoir le flux Alpaca, détecter des setups via plusieurs stratégies de scan codées en Rust, afficher les alertes dans une interface très rapide avec graphiques, gérer des trades internes simulés, capturer des screenshots, journaliser les trades, et envoyer automatiquement les données disponibles vers l’API TradeTally.

La V1 doit être volontairement simple, rapide et fiable :

- pas de Docker ;
- pas de NAS DB propre à l’application ;
- pas d’API NAS ;
- pas de bridge TWS/DAS ;
- pas d’envoi d’ordre réel au broker ;
- backend local 100 % Rust ;
- stratégies Rust compilées ;
- cache RAM pour le live ;
- base locale simple pour charger les données nécessaires et réduire le travail du WebSocket ;
- TradeTally comme destination externe pour les trades, notes, captures et logs associés.

Le NAS peut héberger TradeTally, mais l’application ne dépend pas d’une base NAS dédiée. Si TradeTally est lent, coupé ou indisponible, le scanner doit continuer à fonctionner localement.

## 2. Architecture cible V1

```
Alpaca WebSocket
        ↓
Tauri App locale PC/Mac
        ↓
Backend Rust local
        ↓
Cache RAM temps réel
        ↓
Strategy Engine Rust compilé
        ↓
Alert Engine
        ↓
UI React + charts + alertes
        ↓
Trade interne + journal + capture
        ↓
Queue locale → API TradeTally
```

### Principe fondamental

Le chemin critique du live doit rester local :

```
Alpaca → RAM locale → scanner Rust → UI
```

La base locale et TradeTally ne doivent jamais bloquer l’affichage d’une alerte.

## 3. Deux couches de données

## 3.1. Cache RAM temps réel

Utilisé pour tous les calculs rapides :

- état live de chaque ticker ;
- prix courant ;
- bid/ask/spread ;
- volume du jour ;
- RVOL ;
- VWAP ;
- high/low du jour ;
- bougies 5s, 10s, 1m, 2m, 5m, 15m, daily ;
- signaux actifs ;
- alertes récentes ;
- zones UI occupées ;
- tickers figés dans les zones ;
- SL/TP dessinés ;
- tradeID courant par zone ;
- positions internes ;
- ordres internes en attente ;
- statut LLM ;
- latence affichée dans l’UI.

Technologies recommandées :

```
Rust
DashMap / HashMap + Arc<RwLock<...>>
Ring buffers par ticker
Tokio channels pour pousser les updates vers l’UI
```

La RAM est la source de vérité du live.

## 3.2. Base locale PC/Mac

Une base locale simple est utilisée pour alléger le travail du WebSocket et persister les éléments utiles entre deux sessions.

Technologie recommandée :

```
SQLite local
```

Options complémentaires plus tard :

```
DuckDB / Parquet pour analyse historique volumineuse
```

La base locale stocke :

- univers small caps préfiltré ;
- derniers daily bars nécessaires au calcul des filtres ;
- previous close ;
- average volume ;
- ATR si nécessaire ;
- change 3D / 5D pré-calculé ;
- données fondamentales importées : float, shortable si disponible ;
- tags récupérés depuis TradeTally ;
- file d’attente locale des envois TradeTally non synchronisés ;
- chemins locaux des captures ;
- logs locaux ;
- configuration générale ;
- historique minimal des alertes récentes si utile.

La base locale ne sert pas à calculer chaque tick. Elle sert à préparer l’univers, hydrater les stratégies et éviter de demander trop de symboles au WebSocket.

## 4. Stack technique

### 4.1. Application desktop

```
Tauri
React
TypeScript
Vite
Tailwind CSS
shadcn/ui
lucide-react
TanStack Table
TanStack Virtual
TradingView Lightweight Charts
Zustand
TanStack Query
```

### 4.2. Backend local

```
Rust 100 %
Tokio
Alpaca WebSocket client
SQLite local
Strategy engine compilé
Alert engine
Internal broker engine
Screenshot engine
TradeTally API client
Claude API client optionnel
```

### 4.3. Pas inclus dans la V1 mais déployé bientôt

```
Bridge TWS/DAS
Exécution broker réelle
Replay historique
```

## 5. Organisation du projet

## 6. Gestion des stratégies

## 6.1. Choix retenu

La V1 utilise l’option A : **stratégies Rust compilées**.

Chaque stratégie est un fichier Rust

Exemples :

```
premarket_frd_runner.rs
open_hod_breakout.rs
former_runner_news.rs
```

Ajouter ou modifier une stratégie nécessite de recompiler l’application. C’est accepté, car cette approche est plus simple, plus stable, plus typée et plus rapide.

## 6.2. Ne pas hardcoder manuellement la liste des stratégies

Même si les stratégies sont compilées, il faut éviter une liste manuelle fragile.

Solution recommandée :

```
build.rs ou macro Rust
→ détecte les fichiers de stratégie au moment de la compilation
→ génère un registre compilé
→ l’application charge ce registre au démarrage
```

À l’exécution, l’application ne lit pas des fichiers de stratégie externes. Elle lit le registre compilé.

Résultat :

- pas de chargement dynamique instable ;
- pas de DSL à maintenir ;
- pas de TOML stratégie ;
- recompilation nécessaire ;
- registre robuste ;
- pas besoin de maintenir une liste manuelle de stratégies.

## 6.3. Contrat d’une stratégie Rust

Chaque stratégie doit implémenter un trait commun.

Exemple conceptuel :

```rust
pub trait ScanStrategy: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn enabled(&self) -> bool;
    fn session(&self) -> Session;
    fn priority(&self) -> u8; // 1 à 5
    fn risk_config(&self) -> StrategyRiskConfig;
    fn card(&self) -> StrategyCard; // carte d'identité: univers, panes, indicateurs, bande d'infos, LLM, enrichissements
    fn should_alert(&self, ctx: &StrategyContext) -> Option<AlertSignal>;
    fn sort_key(&self, state: &TickerState) -> SortValue;
}
```

## 6.4. Éléments définis dans chaque fichier stratégie

Chaque fichier stratégie doit définir :

- nom ;
- ID ;
- toggle `ENABLED` en haut du fichier ;
- session : premarket, open, pre-open, (plusieurs choix possibles) ;
- priorité de l’alerte de 1 à 5 ;
- règles de filtrage ;
- règles de tri ;
- conditions d’alerte ;
- cooldown ;
- risque par trade propre à la stratégie ;
- carte d'identité `card()` (`StrategyCard`), qui regroupe :
  - `universe` : univers de streaming observé (low-float premarket vs us-stocks) ;
  - `panes` : 1 à 3 panes par zone (timeframe, instrument optionnel, indicateurs) ;
  - `info_fields` : champs spécifiques de la bande d'infos (le nom + le badge priorité sont communs, ajoutés par l'UI), chacun avec sa source (`alert` = dispo immédiatement, `enrichment`/`llm` = affichés en loading jusqu'à l'arrivée de la donnée) ;
  - `llm` : prompt template + champs produits, si la stratégie nécessite un appel LLM ;
  - `enrichments` : providers (Massive, sec-api…) + champs produits, pour les infos complémentaires.

> Indicateurs disponibles par pane : VWAP, EMA(période), SMA(période), Volume, PreviousClose (mappés vers lightweight-charts côté frontend).

## 7. Démarrage de l’application

Au démarrage, l’application doit réduire l’univers avant d’ouvrir les souscriptions WebSocket.

pour charger les float de tout l’univers us stock, utiliser l’API  Financial Modeling Prep (FMP) et l’endpoint paginé qui charge tous les floats d’un coup.

Étapes :

```
Charger la config locale.

Charger les stratégies Rust compilées activées.

Charger depuis SQLite le dernier univers connu :
   - tickers déjà filtrés ;
   - derniers floats connus ;
   - market cap si disponible ;
   - average volume ;
   - previous close ;
   - change 3D / 5D ;
   - date de dernière mise à jour des données.

Mettre à jour les floats via FMP de tout l'univers us stock:
   - appeler l’endpoint bulk paginé FMP `shares-float-all` ;
   - récupérer `float_shares`, `outstanding_shares`, `free_float` si disponible ;
   - sauvegarder/mettre à jour SQLite ;
   - conserver la date de mise à jour FMP ;
   - si FMP est indisponible, utiliser les derniers floats en cache SQLite et afficher alerte popup modal.

Mettre à jour l’univers avec api Alpaca :
   - tickers actifs ;
   - exchange ;
   - tradable ;
   - shortable ;
   - easy_to_borrow si disponible ;
   - status actif/inactif.

Charger les données daily/historiques nécessaires :
   - last close ;
   - previous close ;
   - volume moyen ;
   - ATR si nécessaire ;
   - change 1D / 3D / 5D ;
   - high/low récents ;
   - données nécessaires aux stratégies activées uniquement.

Calculer ou mettre à jour les univers :
   - chaque stratégie à son propre univers (all stock us, small caps, small caps low float)
   - un ficher rust pour filtrer les tickers par univers.

8. Construire l’univers final à streamer :
   - intersection Alpaca assets actifs + données FMP/cache + critères small caps ;
   - exclusion des tickers non tradables ;
   - exclusion des tickers sans données minimales requises ;
   - possibilité de garder certains tickers même sans float si une stratégie le permet ;
   - sauvegarde de l’univers final dans SQLite.

9. Récupérer depuis TradeTally la liste des tags disponibles :
   - stocker les tags en cache SQLite ;
   - si TradeTally est indisponible, utiliser les derniers tags en cache.

10. Démarrer Alpaca WebSocket uniquement sur les symboles retenus.

11. Démarrer les agrégateurs de bougies :
   - 5s ;
   - 10s ;
   - 1m ;
   - 2m ;
   - 5m ;
   - 15m ;
   - D.

12. Démarrer le scanner engine :
   - uniquement avec les stratégies activées ;
   - uniquement sur l’univers final ;
   - avec les données FMP/cache déjà disponibles dans le contexte ticker.

13. Démarrer l’UI :
   - sidebar ;
   - zones Premarket/Open ;
   - logs TradeTally ;
   - positions internes ;
   - ordres internes ;
   - latence Alpaca → UI.
```

## 7.1. Filtre small caps au démarrage / Univers

Objectif : demander moins de travail au WebSocket.

un fichier par univers, dans un dossier propre. le fichier rust contient le script pour filtrer les tickers. Ensuite, selon l’onglet actif, l’univers en question sera streamé.

## 8. Interface utilisateur

## 8.1. Zones principales

L’interface comporte :

### 1. Barre latérale fine

Boutons onglets :

- Premarket ;
- Pre-open ;
- Open ;
- Latency status à droite ;
- tout à droite, trois points avec dropdown menu :
    - Settings ;
    - Logs ;
    - Sync TradeTally status.
    - Signaler un bug (formulaire/ fenetre modale avec zone texte, puis bouton envoyer, puis stocke en bas dans un tableau délifant, avec bouton copier coller pour tout envoyer plus tard dans vscode + claude), puis bouton tout effacer.

### 2. Sidebar fonctionnelle

Contient :

- Scanner alerts ;
- Positions ouvertes ;
- Ordres en attente.

L’affichage du scanner dépend de l’onglet sélectionné : Premarket, Pre-open ou Open.

Les positions ouvertes et ordres en attente sont globaux, peu importe l’onglet actif.

### 3. Main window

Affiche les tickers issus des alertes sous forme de zones.

Premarket et Pre-open :

```
1 zone par onglet premarket
```

Open :

```
4 zones par onglet open
2 x 2
```

Les zones sont alimentées automatiquement de gauche à droite et de haut en bas.

## 8.2. Onglets dynamiques si manque de place

Si toutes les zones de l’onglet courant sont occupées et qu’une nouvelle alerte arrive, l’application crée automatiquement un nouvel onglet.

Exemples :

```
Premarket
Premarket 2
Premarket 3

Open
Open 2
Open 3
```

La nouvelle alerte est placée dans la première zone disponible du nouvel onglet.

## 8.3. déplacer les tickers

### Déplacer les tickers

objectif : par un click déposer, on peut déplacer les tickers d’une zone à une autre, les faire voyager d’un onglet à un autre. on peut attraper un ticker depuis un scanner et le glisser dans une zone choisie. 

## 8.4. Affichage des positions ouvertes sur les charts

Les positions internes ouvertes doivent apparaître sur les graphiques en très léger.

Affichage :

- ligne d’entrée avec forte transparence ;
- ligne SL avec forte transparence ;
- ligne TP avec forte transparence ;
- ne doit pas gêner la lecture du chart.
- labels à mettre sur les lignes concernées à gauche : sl, tp, nbR (potentiel, ajusté avec TP)

Style :

```
opacity faible
ligne fine
label discret
```

## 8.5. Indicateur LLM loading

Si une stratégie déclenche un prompt Claude, la zone doit afficher un symbole de chargement.

Comportement :

```
Alerte affichée immédiatement
→ badge/spinner “LLM…”
→ réponse reçue
→ remplacement par résumé LLM
→ en cas d’erreur : badge “LLM error”
```

Icône lucide suggérée :

```
LoaderCircle
```

## 9. Toolbar de chaque zone

La toolbar utilise des icônes lucide.

Boutons :

```
Libérer | Ligne | Texte | clock | SL | TP | 25 | 50 | 100 | Lmt/Mkt | X | Capture | Journal
```

Mapping lucide recommandé :

```
Libérer    → bird
Ligne      → Slash 
Texte      → Type
timeframe  → clock
SL         → SL
TP         → TP
25/50/100  → boutons texte
X          → circle-x
Capture    → Camera
Journal    → NotebookPen
Loading    → LoaderCircle
Logs       → ScrollText
Alertes    → Bell
Positions  → BriefcaseBusiness
Ordres     → ListOrdered
```

## 9.1.a Bouton Libérer

Objectif : retirer le ticker de la zone et libérer la place.

Comportement :

- supprime le ticker de la zone ;
- conserve les données de trade si un tradeID existe ;
- demande confirmation si une position interne est ouverte ;
- rend la zone disponible pour les prochaines alertes.

## 9.1.b Bouton Ligne

Active le mode dessin de ligne.

Fonction :

- l’utilisateur clique deux points sur le graphique ;
- une ligne est dessinée ;
- elle est stockée en RAM ;
- si un tradeID existe, la ligne est associée à ce tradeID (si jamais l’utilisateur ouvre à nouveau le ticker, il retrouve les éléments de dessin).

## 9.2. Bouton Texte

Active le mode annotation texte.

Fonction :

- clic sur le graphique ;
- saisie d’un texte court ;
- affichage du texte sur le chart ;
- stockage en RAM ;
- association au tradeID.

## 9.3. menu déroulant timeframes

dropdown menu :

```
5s
10s
1m
2m
5m
15m
D
```

Fonction :

- change le timeframe du chart actif ;
- si la zone contient plusieurs charts, le timeframe s’applique au chart sélectionné ;
- le choix est stocké localement dans l’état UI.
- icone clock

## 9.4. Bouton SL

Fonction :

- active le mode placement du stop loss ;
- l’utilisateur clique un prix sur le graphique ;
- crée une ligne horizontale avec mention `SL` à gauche ;
- mémorise le prix SL en RAM ;
- crée automatiquement un tradeID si aucun tradeID n’existe encore pour cette zone/ticker.
- je peux modifier le stop loss en cliquant déposant la ligne sur le chart.
- Associer TP OCO si TP existe déjà

Format tradeID :

```
YYMMJJHHMMSS-TICKER-STRATEGY
```

Exemple :

```
260527095512-ABCD-PREMARKET_FRD
```

Important : le tradeID peut exister même si aucun trade n’est finalement ouvert. Il sert à rattacher SL, TP, notes, captures et contexte.

## 9.5. Bouton TP

Fonction :

- active le mode placement take profit ;
- l’utilisateur clique un prix sur le graphique ;
- crée une ligne horizontale avec mention `TP` à gauche ;
- mémorise le prix TP en RAM ;
- crée automatiquement un tradeID si aucun tradeID n’existe encore.
- je peux modifier le tp en cliquant déposant sur le chart
- Associer sl OCO si SL existe déjà

Règle :

```
Le premier bouton SL ou TP utilisé crée le tradeID.
```

## 9.6. Boutons 25 / 50 / 100

Ces boutons créent des ordres limites internes. Ils ne sont pas envoyés à un broker.

### État grisé

Les boutons 25 / 50 / 100 doivent être grisés si aucun SL actif n’existe pour la zone.

Raison : la taille de position dépend de la distance avec le SL.

### Fonction

Quand un SL existe :

- lire le prix d’entrée souhaité ;
- lire le SL ;
- calculer le risque par action ;
- lire le risque autorisé de la stratégie ;
- calculer la taille full position ;
- appliquer 25 %, 50 % ou 100 % ;
- créer un ordre limite interne ;
- ajouter l’ordre aux ordres en attente ;
- simuler l’exécution selon la règle de fill interne.
- la direction Long/Short est calculée automatiquement selon où se situe le SL par rapport à l’entrée.
- ils peuvent être modifiés en clic déposer (comme tp et sl)

Calcul :

```
risk_per_share = abs(entry_price - stop_loss)
full_position_size = strategy_max_risk_dollars / risk_per_share
button_25_size = full_position_size * 0.25
button_50_size = full_position_size * 0.50
button_100_size = full_position_size
```

Le risque est variable par stratégie.

## 9.6.2 Toggle Lmt/Mkt

Toggle Lmt ↔ Mkt, sur Mkt par défaut. permet d’envoyer directement des ordres au marché quand Mkt (market) est activé. Sinon si Lmt activé, l’utilisateur renseignera la ligne limite sur le chart.

## 9.7. Bouton X

Fonction :

- ordre market interne ;
- fermeture de la position interne du symbole/zone ;
- annulation des ordres internes en attente liés ;
- mise à jour de TradeTally selon le tradeID.
- un trade est fermé si le retour à la position est flat pour le même ticker et même account. il peut y avoir plusieurs exécutions mais tant que ce n’est pas flat, on garde le même tradeID.

V1 : aucun ordre réel n’est envoyé à un broker.

## 9.8. Bouton Capture

Fonction :

- prend une capture de la zone graphique uniquement ;
- exclut la toolbar ;
- sauvegarde une copie locale ;
- si un tradeID existe, envoie la capture à TradeTally en asynchrone ;
- si TradeTally est indisponible, met l’envoi en queue locale.

Si aucun tradeID n’est associé :

```
ouvrir une fenêtre “Enregistrer sous”
```

## 9.9. Bouton Journal

Le bouton Journal doit être grisé si aucun tradeID n’existe pour la zone.

Quand un tradeID existe, il ouvre un formulaire simple :

- zone texte pour notes ;
- slider confidence level 1 à 10 ;
- tags ;
- bouton sauvegarder.

Les tags proposés doivent être récupérés en cache depuis l’API TradeTally au démarrage de l’application.

je peux cliquer à nouveau sur le bouton journal pour modifier les entrées et ça sera à nouveau mis à jour à TradeTally quand j’enregistre.

Données envoyées à TradeTally :

```json
{
  "trade_id": "260527095512-ABCD-PREMARKET_FRD",
  "notes": "...",
  "confidence": 7,
  "tags": ["FRD", "news", "low_float"],
  "created_at": "..."
}
```

## 10. Modèle de trade interne

L’application gère des trades internes. Elle ne transmet pas d’ordres réels au broker.

## 10.1. Création du tradeID

Le tradeID est créé dès le premier placement de SL ou TP.

Format :

```
YYMMJJHHMMSS-TICKER-STRATEGY
```

Exemple :

```
260527095512-ABCD-PREMARKET_FRD
```

Ce tradeID est utilisé pour rattacher :

- SL ;
- TP ;
- ordres internes ;
- position interne ;
- captures ;
- journal ;
- logs TradeTally ;
- résumé LLM ;
- contexte scanner.

## 10.2. Création du trade ouvert

Un trade devient réellement “ouvert” dans l’application quand un ordre interne est exécuté.

Données envoyées à TradeTally lors de l’ouverture :

- tradeID ;
- symbole ;
- heure d’ouverture ;
- prix d’entrée ;
- nombre d’actions ;
- sens : long ou short ;
- setup : nom de la stratégie ;
- stop loss ;
- take profit si disponible ;
- commissions par défaut ;
- fees par défaut ;
- broker par défaut ;
- account par défaut ;
- strategy_id ;
- zone_id ;
- contexte scanner ;
- priorité de l’alerte ;
- confidence/tags si déjà saisis.

## 10.3. Simulation des fills

Les fills internes doivent être simulés avec une règle défavorable bid/ask, comme si l’exécution était au marché.

Règle V1 :

```
Pour achat long ou couverture short : fill au ask
Pour vente short ou vente de sortie long : fill au bid
```

Cette règle est volontairement conservatrice.

## 11. Scanner alerts

## 11.1. Principe

Chaque stratégie active analyse le flux temps réel. Quand les conditions sont remplies, elle génère une alerte.

Une alerte contient :

- alertID ;
- timestamp ;
- symbole ;
- stratégie ;
- priorité 1 à 5 ;
- prix courant ;
- bid/ask ;
- spread ;
- volume ;
- RVOL ;
- change day ;
- change 3D ;
- float ;
- news_today ;
- halt status si disponible ;
- latency_ui_ms ;
- raison de l’alerte ;
- charts à afficher ;
- overlays à afficher ;
- demande LLM éventuelle.

## 11.2. Priorité des alertes

La priorité est définie dans le code de la stratégie avec une note de 1 à 5.

```
1 = faible
2 = normal
3 = intéressant
4 = important
5 = critique / très prioritaire
```

Cette priorité peut influencer :

- le son d’alerte ;
- la couleur du badge ;

## 11.3. Anti-spam

si un ticker est alerté depuis les scanner une deuxième fois alors qu’il est déjà affiché sur les charts, ne rien faire. si ensuite le chart est fermé par l’utilisateur, laisser une fenêtre antispam de 2 min avant d’autoriser à nouveau les alertes.

## 12. LLM / Claude API

Certaines stratégies peuvent lancer un prompt Claude.

Le LLM doit être strictement asynchrone.

Flux :

```
Alerte détectée
→ affichage immédiat de la zone
→ affichage LoaderCircle / “LLM…”
→ prompt envoyé à Claude
→ réponse reçue
→ résumé affiché dans la zone
```

Le LLM ne doit jamais bloquer :

- le scanner ;
- les alertes ;
- l’UI ;
- le placement SL/TP ;
- les boutons 25/50/100/X.

## 13. Données Alpaca

## 13.1. Flux live

Sources Alpaca utilisées si disponibles :

- trades ;
- quotes ;
- bars ;
- news ;
- assets ;
- halt status si disponible.

Si Alpaca ne fournit pas directement une information utile, l’application doit prévoir un champ optionnel et afficher `N/A`.

## 13.2. Agrégation locale

L’application construit localement les bougies :

```
5 secondes
10 secondes
1 minute
2 minutes
5 minutes
15 minutes
Daily
```

L’agrégation est faite en Rust, pas en React.

## 13.3. Latence simplifiée

La V1 affiche une seule latence principale :

```
latence entre le timestamp Alpaca et l’affichage dans l’UI
```

Nom :

```
websocket_to_ui_latency_ms
```

Affichage recommandé :

```
184 ms
```

Codes visuels :

```
< 300 ms : normal
300–1000 ms : attention
> 1000 ms : lent
> 2000 ms : critique
```

## 14. Positions ouvertes et ordres en attente

## 14.1. Positions ouvertes internes

Afficher :

- symbole ;
- quantité (avec + ou - selon long ou short);
- bouton info complémentaires (Vertical Ellipsis)
    - stratégie ;
    - sens ;
    - heure d’ouverture ;
    - prix d’entrée ;
    - SL ;
    - TP ;
    - R multiple latent ;
    - PnL latent ;
- boutons Close

## 14.2. Ordres internes en attente

Afficher :

- symbole ;
- quantité ;
- Logo OCO activé (point vert si OCO présent, et associer les deux points vert ensemble par une ligne par exemple, ou alors encadrer les deux lignes OCO ensemble)
- Bouton infos complémentaire (comme pour position, Vertical Ellipsis)
    - stratégie ;
    - limit price ;
    - sens ;
    - heure de création ;
- bouton annuler.

## 15. TradeTally API

## 15.1. Événements envoyés à TradeTally

L’application doit envoyer à TradeTally :

- tradeID  ;
- ouverture trade ;
- modification SL ;
- modification TP ;
- ajout/modification note ;
- ajout capture ;
- fermeture trade (ajout exécution)

Les endpoints exacts seront fournis plus tard. un trade contient plusieurs exécutions. un trade est fermé si retour à flat pour le même symbole et même account.

## 15.2. Queue locale résiliente

Si TradeTally est lent, en panne ou coupé :

- l’événement est écrit dans une queue locale SQLite ;
- l’UI affiche un statut `sync pending` ;
- l’application réessaie en arrière-plan ;
- le scanner live continue normalement.

## 15.3. Page TradeTally API

Ajouter une page modale dans dropdown menu de la barre latérale.

Il affiche ce qui est envoyé à TradeTally :

- timestamp ;
- type d’événement ;
- tradeID ;
- symbole ;
- endpoint appelé ;
- payload résumé ;
- statut : pending, success, failed ;
- message d’erreur éventuel ;
- nombre de tentatives.

Objectif : pouvoir auditer facilement les échanges avec TradeTally.

## 15.4. Tags TradeTally

Au démarrage :

```
Appeler l’API TradeTally pour récupérer la liste des tags disponibles.
Stocker les tags en cache local SQLite.
Proposer ces tags dans le formulaire Journal.
```

Si l’API TradeTally est indisponible :

```
utiliser les derniers tags en cache local
```

## 16. Configuration globale

Exemple :

```toml
[trading]
default_broker = "IBKR"
default_account = "SIM_INTERNAL"
default_commission = 1.00
default_fees = 0.35
min_position_size = 1
max_position_size = 10000

[alpaca]
feed = "sip"
use_news = true

[universe]
price_min = 0.50
price_max = 30.0
market_cap_max = 500000000
float_max = 50000000
average_volume_min = 100000

[ui]
default_theme = "dark"
premarket_zones_per_tab = 1
pre_open_zones_per_tab = 1
open_zones_per_tab = 4
auto_create_tabs = true

[latency]
warn_ms = 1000
critical_ms = 2000

[tradetally]
api_base_url = "http://nas.local:8000"
```

Les secrets Alpaca, Claude et TradeTally doivent être stockés côté backend Tauri, idéalement dans le stockage sécurisé du système, pas dans le frontend React.

## 17. Sécurité et limites V1

### 17.1. Clés API

Les clés ne doivent pas être exposées dans React.

Elles sont utilisées uniquement côté Rust.

### 17.2. Pas d’ordre broker réel

Les boutons 25/50/100/X ne doivent envoyer aucun ordre réel.

Ils ne créent que des ordres et positions internes.

### 17.3. Mode live conservateur

La simulation des fills utilise bid/ask défavorable.

### 17.4. Données TradeTally

TradeTally est hébergé sur le NAS.

L’application communique uniquement via API.

## 19. Décisions techniques actées

1. Backend local : **100 % Rust** pour rapidité et stabilité.
2. Stratégies : **Rust compilées**, recompilation acceptée.
3. TradeTally : endpoints exacts fournis plus tard.
4. Screenshots : sauvegarde locale + envoi TradeTally si tradeID ; sinon fenêtre “Enregistrer sous”.
5. Fills internes : bid/ask défavorable, comme si market.
6. Risque par trade : variable par stratégie.
7. Long/short : calculés en fonction de là où est placé le SL par rapport à l’entrée.
8. Halts : afficher si Alpaca fournit l’information ; sinon `N/A`.
9. Replay historique : non en V1 mais oui en V2, prévoir la possibilité d’évolution
10. Multi-utilisateur / multi-compte : non en V1.