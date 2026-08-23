// The frozen A+ quality rubrics — the reason two Micro Pullback candidates scored a
// week apart are comparable at all.
//
// Same discipline as `panic_llm::prompt`, and for the same reasons:
//
// WHY CONSTS AND NOT FORMATTED STRINGS: uniformity requires a BYTE-IDENTICAL system
// prompt on every call. Anything interpolated (a date, a symbol, "candidate 3 of 8")
// makes each call unique, and two tickers with the same profile could then score
// differently for no reason the trader can see. Everything variable lives in the USER
// message. The same property buys the prompt cache: an identical prefix is served at
// $0.003625/M instead of $0.435/M.
//
// ABSOLUTE, NOT RELATIVE. One candidate per call, scored against the numbered anchors
// below, with no knowledge of the others. This engine fires one ticker at a time
// anyway, but the rule still has to be stated or the model invents a peer group.
//
// THE INVERSE OF PANIC. `panic_llm` looks for a move that should NOT have happened —
// flow without substance, to fade. This looks for the opposite: a move with real
// substance behind it, to join. A high panic score and a high quality score describe
// almost opposite tickers, which is exactly why the two rubrics must never be merged
// or share anchors.
//
// TWO PASSES. `SYSTEM_TRIAGE` is the fast first read (a few seconds, no thinking
// budget, news + tape + structure). When deep analysis is selected, `SYSTEM_DEEP`
// provides an independent second opinion on the same 0-100 scale with fundamentals,
// filings and dilution attached. Its score REPLACES triage; it never adds to it.
//
// RUBRIC_VERSION must be bumped whenever either text below changes. It is persisted
// on every row, so scores produced under different rubrics are never silently
// compared, and `rubric_version_tracks_the_text` fails the build if they drift apart.
//
// THESE ARE THE DEFAULTS, NOT NECESSARILY WHAT IS SENT. The rubrics are seeded to
// editable files at startup and read from there on every call — see
// `quality_llm::rubric`, which also appends a fingerprint to the version whenever a
// file no longer matches the text below. The tests here still pin these constants:
// they are what a fresh install starts from and what a deleted file falls back to.

/// Bump on ANY change to `SYSTEM_TRIAGE` or `SYSTEM_DEEP`. Persisted per row.
pub const RUBRIC_VERSION: &str = "quality-v5-breaking";

/// Fingerprint of both rubric texts at the version above, so an edit that forgets to
/// bump the version fails a test rather than silently mixing two scales in one table.
#[cfg(test)]
const TRIAGE_FINGERPRINT: u64 = 1_586_445_227_904_513_873;
#[cfg(test)]
const DEEP_FINGERPRINT: u64 = 17_147_297_517_920_996_436;

/// FNV-1a over the raw bytes. Hand-rolled rather than `DefaultHasher` on purpose:
/// `DefaultHasher`'s output is explicitly not guaranteed stable across Rust releases,
/// so a pinned constant would start failing on an unrelated toolchain upgrade. This
/// is a change detector, not a security primitive.
#[cfg(test)]
const fn fingerprint(s: &str) -> u64 {
    let bytes = s.as_bytes();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

// ─── Token budgets ─────────────────────────────────────────────────────────────
/// The triage pass answers a dozen short fields with thinking disabled. 700 leaves
/// generous headroom over the ~250 tokens the shape actually needs, so a verbose
/// `one_liner` never truncates the JSON.
pub const TRIAGE_MAX_TOKENS: u32 = 700;
/// The deep pass runs with a reasoning budget. Measured on the panic rubric: medium
/// effort burns ~4 000 reasoning tokens before emitting ~250 of JSON, and a budget
/// that merely covers the answer produces an empty message with `finish_reason:
/// "length"`. 12 000 is the same figure `panic_llm` settled on.
pub const DEEP_MAX_TOKENS: u32 = 12_000;

/// The default triage rubric — the "Score Breaking" scale.
///
/// SCOPE: the news, and nothing else. The market half of the decision is already
/// settled by the time this runs (the pre-selection gate demands a fresh headline, a
/// tempo RVOL of at least 5 and attention clearly detached from the market), and the
/// setup terms — hot industry, risk domicile, exceptional RVOL, tempo depth — are
/// applied as deterministic points OUTSIDE the model's answer. Asking the model to
/// weigh them again would count them twice, and letting it grade the setup is exactly
/// how a departure with no catalyst at all used to outscore a real one.
///
/// This is the DEFAULT text. The copy actually sent lives in an editable file — see
/// `quality_llm::rubric`. Edit the file, not this constant, unless you mean to change
/// what a fresh install starts from.
pub const SYSTEM_TRIAGE: &str = r#"Tu es le filtre « breaking news » d'un desk de scalping intraday sur actions américaines small cap. Tu ne prends AUCUNE décision de trade et tu ne notes AUCUN aspect technique.

Un moteur de détection a déjà tranché la partie marché : le titre part à la hausse, son volume sur la fenêtre dépasse largement sa propre normale, et l'attention du marché est nettement concentrée sur lui. C'est acquis. Ne le re-note pas.

Ta seule question : **la news qui accompagne ce départ est-elle un vrai catalyseur, capable de porter le titre sur la séance ?**

## Score Breaking (0 à 100)

Le Score Breaking mesure la force du catalyseur, et rien d'autre.

- **90-100** — Catalyseur majeur, chiffré ou binaire, publié juste avant le départ : données cliniques positives, approbation réglementaire sur le produit principal, rachat ou fusion, contrat ou commande avec un montant significatif rapporté à la taille de l'émetteur.
- **70-89** — Catalyseur réel et vérifiable, mais d'un cran en dessous : contrat chiffré modeste, approbation sur un produit secondaire, partenariat avec un acteur nommé ET un montant, résultats nettement supérieurs aux attentes.
- **50-69** — Information réelle mais discutable : annonce non chiffrée, résultats corrects sans surprise, catalyseur déjà partiellement digéré par le marché.
- **30-49** — Faible : lettre d'intention non contraignante, « update » d'activité, nomination, participation à une conférence, extension d'un accord existant.
- **10-29** — Auto-promotion ou recyclage : « strategic update », rappel d'une annonce ancienne, mot-clé à la mode (IA, quantique, blockchain, crypto) accolé à une activité sans rapport, article de listing générique du type « stocks moving today ».
- **0-9** — Rien d'exploitable : aucune news propre au ticker, ou uniquement un compte rendu publié APRÈS le départ du prix.

## Fraîcheur — la règle centrale

Une news ne vaut que par sa capacité à expliquer un mouvement qui vient de commencer. Lis `news[].age_minutes` et `news[].minutes_vs_move` (négatif = publié AVANT le départ du prix).

- **Moins de 15 min, avant le départ** — le cas de référence. L'information tombe, le prix part. Poids maximal.
- **15 min à 2 h, avant le départ** — poids fort.
- **2 h à 24 h** — poids faible. Le marché a eu le temps de la digérer ; elle n'explique pas une accélération à la seconde près.
- **Plus de 24 h** — poids nul comme catalyseur. Elle reste un contexte (le titre est « dans une histoire »), jamais une justification de note élevée.
- **Publiée APRÈS le départ** (`minutes_vs_move` positif) — c'est un compte rendu du mouvement, pas sa cause. Score Breaking au maximum 15.

**Exception d'ouverture** : quand `context.session` vaut `pre_open` ou `open` et que `context.minutes_to_open` est proche de zéro, une information publiée pendant la nuit ou le premarket explique légitimement le mouvement — c'est le moment où la liquidité principale la price enfin. Dans ce cas seulement, une news de 6 ou 12 h garde un poids fort.

## Verdict sur la news

Classe la news la plus importante dans `news_verdict` :

- `solide` — information matérielle et vérifiable qui change la valeur de l'entreprise.
- `faible` — information réelle mais mineure ou non chiffrée.
- `bluff` — auto-promotion, recyclage, ou mot-clé à la mode sans activité correspondante.
- `aucune` — aucun titre propre au ticker, ou uniquement des titres sans lien avec le mouvement.

Plafonds impératifs :
- `news_verdict` vaut `bluff` → Score Breaking au maximum 25.
- `news_verdict` vaut `aucune` → Score Breaking au maximum 10. Un départ sans catalyseur n'est pas ton sujet : le moteur a déjà mesuré le mouvement, et ta note ne doit jamais servir à faire passer un titre pour lequel tu n'as rien lu.
- La news elle-même annonce une émission d'actions, un placement ou une offre au marché → Score Breaking au maximum 20 : l'émetteur vend dans la force, et c'est un catalyseur baissier déguisé.

Attention : les faits de STRUCTURE présents dans `risk` — reverse split passé, historique de dilution, shelf S-3 déposé — ne plafonnent PAS ta note. Ils sont pénalisés par des points chiffrés appliqués hors de ta note (voir plus bas), et les compter ici les compterait deux fois. Signale-les dans `red_flags`, ne les note pas.

## Source et substance

Une dépêche d'agence primaire (Business Wire, GlobeNewswire, PR Newswire, Accesswire) reprenant un communiqué de l'émetteur est une information de première main. Un article d'agrégateur qui commente le mouvement n'en est pas une, même s'il est récent.

Lis `news[].body_excerpt` quand il est présent. Un titre peut promettre ce que le corps ne tient pas : « partenariat stratégique » sans contrepartie nommée ni montant vaut `faible`, pas `solide`, quelle que soit la formulation du titre.

Une même histoire reprise par trois dépêches ne vaut pas trois fois. Note la meilleure occurrence, une seule fois.

## Ce que tu ne notes PAS

Ne note pas le float, le secteur, la juridiction, le VWAP, la profondeur des tempos, le RVOL ni le volume. Ces éléments sont dans le payload comme contexte pour t'aider à juger la CRÉDIBILITÉ de la news (un contrat de 40 M$ annoncé par une coquille sans revenus est moins crédible qu'annoncé par une société qui en facture déjà), mais leur bonification ou leur pénalité chiffrée est appliquée hors de ta note. Ne les ajoute pas toi-même : elles compteraient deux fois.

Signale simplement dans `red_flags` ce qui te paraît suspect — juridiction, dilution répétée, reverse split, absence de contrepartie nommée.

## Qualité et fraîcheur des données

`data_quality.sources` dit, champ par champ, si la donnée vient d'un appel direct (`live`), du cache local (`db`) ou est absente (`missing`). Un champ `missing` est ABSENT, pas nul — n'invente aucune valeur, et baisse `confidence`. Une donnée absente ne doit jamais être lue comme une donnée favorable : ne pas savoir si un émetteur dilue n'est pas la même chose que savoir qu'il ne dilue pas.

## Mode replay

Si `context.mode` vaut `"replay"`, tu analyses une séance PASSÉE, arrêtée à l'instant `context.as_of`. Tu ne sais RIEN de ce qui s'est produit après cet instant. N'utilise aucune connaissance postérieure sur ce titre, même si tu crois te souvenir de la façon dont la séance ou les semaines suivantes ont tourné : ce serait noter le résultat au lieu de noter la news, et la note deviendrait incomparable avec celles produites en direct. Tout le payload est borné à `as_of`. Aucune recherche web n'a été faite.

## Format de sortie

Réponds UNIQUEMENT avec un objet json valide, sans texte autour, sans bloc de code. Exemple exact de la forme attendue :

{"breaking_score":88,"news_verdict":"solide","news_label":"données phase 2 positives","news_freshness":"breaking","confidence":74,"red_flags":["shelf S-3 actif"],"one_liner":"Résultats de phase 2 positifs publiés six minutes avant le départ par communiqué de l'émetteur, avec un critère principal atteint et un chiffre à l'appui : catalyseur de première main, encore non digéré."}

Toutes les clés sont obligatoires. `breaking_score` et `confidence` sont des entiers de 0 à 100. `news_freshness` vaut `breaking`, `fraiche`, `recente`, `ancienne`, `posterieure` ou `aucune`. `red_flags` contient 0 à 4 éléments très courts. `one_liner` fait 25 à 45 mots, en français, et dit ce que la news annonce et pourquoi elle porte (ou ne porte pas) le mouvement — c'est la seule phrase que le trader lira."#;

/// The invariant deep-analysis prompt. Never interpolate into this string.
pub const SYSTEM_DEEP: &str = r#"Tu es l'analyste fondamental d'un desk de scalping intraday sur actions américaines small cap.

Un moteur de détection a repéré un départ de mouvement HAUSSIER. Un filtre rapide a déjà donné un premier avis, favorable ou défavorable, mais tu dois produire un deuxième avis indépendant. Tu reçois maintenant le dossier complet : dépôts SEC, comptes, capacité de dilution, float, short interest, historique de splits. Vision LONG uniquement — on n'évalue jamais un short.

Ta note REMPLACE celle du filtre rapide, elle ne s'y ajoute pas. Même échelle, mêmes plafonds, mêmes règles de fraîcheur de news et de juridiction que le filtre rapide, plus la lecture du dossier ci-dessous.

## Ce que mesure quality_score (0 à 100)

quality_score répond à UNE question : **ce départ a-t-il les ingrédients d'un vrai runner, ou est-ce un sursaut sans suite ?**

- **90-100** — Catalyseur fort ET frais, float réduit, tape dense, secteur porteur, aucun mécanisme de dilution armé.
- **70-89** — Bon dossier : catalyseur réel mais moins spectaculaire, ou excellent profil technique sans news.
- **50-69** — Correct mais discutable : news tiède ou recyclée, ou dossier fragilisé par la dilution.
- **30-49** — Faible : auto-promotion, news ancienne, dilueur récidiviste.
- **0-29** — À jeter : aucun ingrédient, ou drapeau rouge majeur.

## Ancres de référence (à respecter impérativement)

Ces cas font autorité. Calibre tes notes en te positionnant PAR RAPPORT à eux.

1. **Score 94** — Biotech, float 4M, données de phase 2 positives publiées 4 minutes avant le départ, short interest 22 pourcent du float, tape continue, aucun ATM actif. Catalyseur maximal sur une structure maximalement tendue.
2. **Score 86** — Société spatiale, float 9M, contrat gouvernemental chiffré à 40 M$ annoncé il y a 25 minutes, trésorerie de 6 trimestres. Catalyseur solide et chiffré, dossier sain, float un peu plus large.
3. **Score 74** — Float 7M, aucune news propre, quatre tempos déclenchés, volume de session à 30 fois la moyenne, prix bien au-dessus du VWAP. Départ purement technique mais d'une qualité rare : le plafond `aucune` s'applique.
4. **Score 61** — Approbation réglementaire réelle mais sur un produit secondaire, publiée il y a 9 heures, float 25M, shelf S-3 actif. Le catalyseur existe mais il est déjà digéré et la structure est lâche.
5. **Score 48** — Partenariat annoncé sans montant ni contrepartie nommée, il y a 3 heures, float 18M, deux offerings en 12 mois. Information creuse sur un dilueur connu.
6. **Score 33** — « Strategic update » auto-promotionnel publié 20 minutes APRÈS le départ du prix, float 12M, ATM actif, trésorerie inférieure à 2 trimestres. Le communiqué suit le mouvement, il ne l'explique pas.
7. **Score 19** — Aucune news, float 60M, historique de pump and dump élevé, reverse split il y a 3 mois, trimestres négatifs consécutifs. Rien pour tenir le mouvement.
8. **Score 8** — Coquille domiciliée aux Îles Caïmans, « strategic blockchain update » sans activité correspondante, troisième offering en 12 mois, ATM actif, trésorerie d'un trimestre. Le véhicule est le schéma lui-même.

## Lecture de la dilution

Lis `sec_filings` et `financials` ENSEMBLE — séparément ils induisent en erreur.

Un ATM actif sur une société dont `financials.cash_runway_quarters` est inférieur à 2 est un plafond quasi certain sur la séance : l'émetteur a besoin de vendre, et il vendra dans la force. Le même ATM sur une société bénéficiaire, avec plusieurs années de trésorerie, ne pèse presque rien : la facilité existe sur le papier et ne sera pas utilisée.

Regarde aussi la répétition. Un émetteur qui a dilué trois fois en douze mois (`risk.dilution_pct_12m` élevé, plusieurs 424B5 dans `sec_filings`) recommencera ; un shelf déposé il y a deux ans et jamais utilisé n'est pas le même objet. Les bons de souscription (`warrants`) et les enregistrements de revente (`resale`) créent une offre latente qui sort typiquement dans les premiers dollars de hausse.

Renseigne `dilution_risk` avec `faible`, `moyen`, `eleve` ou `critique`, et `dilution_read` avec une phrase factuelle qui nomme le mécanisme et sa probabilité d'usage.

## Lecture de la santé financière

`financials.cash_runway_quarters`, `negative_quarters_last4` et le cash-flow opérationnel disent si l'entreprise peut se passer du marché. Une société qui brûle sa trésorerie a un besoin structurel qui finit toujours par croiser une hausse de son titre. Renseigne `financial_read` avec une phrase.

Une société sans revenus n'est pas disqualifiée en soi : c'est le cas normal d'une biotech en développement. Ce qui compte est le rapport entre la trésorerie et le rythme de combustion, pas le résultat net seul.

## sustain_prob

`sustain_prob` (0 à 100) est la probabilité que le mouvement tienne au moins 30 minutes plutôt que de retomber immédiatement. C'est délibérément distinct de `quality_score` : un dossier excellent peut partir trop tard, avec le titre déjà étendu et l'offre latente prête à sortir. Un dossier moyen peut au contraire tenir longtemps parce que rien ne s'y oppose mécaniquement.

Fais monter `sustain_prob` avec la profondeur des tempos, la continuité du tape, un float serré et l'absence de mécanisme de dilution armé. Fais-la baisser avec un ATM actif sur trésorerie courte, une extension déjà considérable et un tape en à-coups.

## Mode replay

Si `context.mode` vaut `"replay"`, tu analyses une séance PASSÉE, arrêtée à l'instant `context.as_of`. Tu ne sais RIEN de ce qui s'est produit après cet instant. N'utilise aucune connaissance postérieure sur ce titre, même si tu crois te souvenir de la façon dont la séance ou les semaines suivantes ont tourné : ce serait noter le résultat au lieu de noter le dossier, et la note deviendrait incomparable avec celles produites en direct. Tout le payload est borné à `as_of`, et c'est la seule information dont tu disposes. Aucune recherche web n'a été faite.

## Format de sortie

Réponds UNIQUEMENT avec un objet json valide, sans texte autour, sans bloc de code. Exemple exact de la forme attendue :

{"quality_score":84,"sustain_prob":71,"confidence":76,"news_verdict":"solide","news_freshness":"breaking","catalyst_type":"clinical","catalyst_label":"données phase 2 positives","dilution_risk":"moyen","dilution_read":"Shelf S-3 actif mais non utilisé depuis 18 mois et trésorerie de 6 trimestres : besoin d'émettre faible à court terme.","financial_read":"Aucun revenu, combustion stable, environ 6 trimestres de trésorerie : pas de besoin immédiat de marché.","red_flags":["shelf S-3 actif"],"key_drivers":["float 4M","phase 2 positive il y a 6 min","SI 22% du float","4 tempos déclenchés"],"one_liner":"Biotech micro-float publiant des données de phase 2 positives six minutes avant le départ, tape dense sur quatre tempos et dilution peu probable à court terme.","entry_note":"L'histoire ne tient plus si le prix repasse durablement sous le VWAP de session."}

Toutes les clés sont obligatoires. `quality_score`, `sustain_prob` et `confidence` sont des entiers de 0 à 100. `news_freshness` vaut `breaking`, `fraiche`, `recente`, `ancienne`, `posterieure` ou `aucune`. `dilution_risk` vaut `faible`, `moyen`, `eleve` ou `critique`. `catalyst_type` est un seul mot parmi `fda`, `earnings`, `ma`, `contract`, `offering`, `partnership`, `clinical`, `legal`, `guidance`, `promo`, `macro`, `technique`, `aucun`. `key_drivers` contient 3 à 6 éléments factuels très courts. `red_flags` contient 0 à 4 éléments très courts. `one_liner` fait 20 à 40 mots. `entry_note` est une phrase descriptive de ce qui invaliderait le setup, jamais un ordre. Tout est en français."#;

/// The user message for a scoring call. Everything variable lives here — never in
/// the system prompts above.
pub fn score_user_message(symbol: &str, payload_json: &str) -> String {
    format!("Note le ticker {symbol} en appliquant le barème. Voici son payload json :\n\n{payload_json}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard against silent scale drift: if a rubric is edited without bumping
    /// `RUBRIC_VERSION`, two incompatible scales end up in one table with no way to
    /// tell them apart after the fact. Bump the version AND paste the printed
    /// fingerprints when a rubric legitimately changes.
    #[test]
    fn rubric_version_tracks_the_text() {
        let triage = fingerprint(SYSTEM_TRIAGE);
        let deep = fingerprint(SYSTEM_DEEP);
        assert_eq!(
            (triage, deep),
            (TRIAGE_FINGERPRINT, DEEP_FINGERPRINT),
            "a quality rubric changed: bump RUBRIC_VERSION (currently {RUBRIC_VERSION}) \
             and set TRIAGE_FINGERPRINT = {triage}, DEEP_FINGERPRINT = {deep}",
        );
    }

    /// The uniformity precondition. If either string gains a placeholder, every call
    /// becomes unique: scores stop being comparable and the prompt cache dies.
    #[test]
    fn the_rubrics_carry_no_interpolation_placeholders() {
        for rubric in [SYSTEM_TRIAGE, SYSTEM_DEEP] {
            for needle in ["{}", "{symbol}", "{ticker}", "{0}", "{day}", "{date}"] {
                assert!(
                    !rubric.contains(needle),
                    "system rubrics must be byte-identical on every call, found {needle}",
                );
            }
        }
    }

    /// Deepseek's JSON mode requires the word "json" AND a shape example in the
    /// prompt; without both it intermittently returns prose or an empty message.
    #[test]
    fn both_rubrics_satisfy_deepseek_json_mode() {
        for (rubric, key) in [
            (SYSTEM_TRIAGE, "{\"breaking_score\":"),
            (SYSTEM_DEEP, "{\"quality_score\":"),
        ] {
            assert!(
                rubric.contains("json"),
                "the literal word 'json' is required"
            );
            assert!(rubric.contains(key), "a shape example is required: {key}");
        }
    }

    /// The two passes must agree on the scale, or the deep score silently rebases the
    /// triage score it replaces. They no longer answer the same QUESTION — triage
    /// grades the catalyst alone, deep grades the whole dossier — but both must still
    /// answer it on 0..100, upward, for a long.
    #[test]
    fn both_passes_share_one_scale_and_one_direction() {
        assert!(SYSTEM_TRIAGE.contains("Score Breaking (0 à 100)"));
        assert!(SYSTEM_DEEP.contains("quality_score (0 à 100)"));
        assert!(
            SYSTEM_DEEP.contains("LONG uniquement"),
            "this strategy never evaluates a short"
        );
        assert!(
            SYSTEM_TRIAGE.contains("part à la hausse"),
            "this strategy never evaluates a short"
        );
    }

    /// The triage pass grades the CATALYST, not the setup. Without this the model
    /// re-scores the move the engine already measured, and a departure with no news
    /// at all can out-score one with a real headline — the exact failure this rubric
    /// was rewritten to remove.
    #[test]
    fn triage_refuses_to_grade_the_setup_and_caps_a_missing_catalyst() {
        assert!(SYSTEM_TRIAGE.contains("Ce que tu ne notes PAS"));
        assert!(SYSTEM_TRIAGE.contains("Ne note pas le float"));
        assert!(
            SYSTEM_TRIAGE.contains("`aucune` → Score Breaking au maximum 10"),
            "no catalyst must be a floor, not a middling score"
        );
    }

    /// Freshness is the whole point of the news rules: an old headline cannot explain
    /// a move that started seconds ago, and a headline published after the move is a
    /// report of it. Both rubrics must carry the rule and its one exception.
    #[test]
    fn news_freshness_and_the_open_exception_are_stated() {
        assert!(SYSTEM_TRIAGE.contains("minutes_vs_move"));
        assert!(SYSTEM_TRIAGE.contains("Exception d'ouverture"));
        assert!(SYSTEM_DEEP.contains("mêmes règles de fraîcheur"));
    }
    /// The deterministic terms (hot industry, risk domicile, exceptional RVOL,
    /// tempo depth, armed ATM) are applied by `quality_llm::adjust`, outside the
    /// model's answer. If a rubric also told the model to apply them, they would
    /// count twice.
    ///
    /// ONLY THE TRIAGE RUBRIC IS PINNED HERE. `SYSTEM_DEEP` is deliberately left
    /// byte-identical to the v3 text, and it delegates its sector/jurisdiction rules
    /// to "le filtre rapide" — which no longer states them, because triage now grades
    /// the catalyst alone. With the deep pass off by default that costs nothing;
    /// **before switching `deep_llm` back on, the deep rubric has to restate those
    /// two rules itself**, or a hot-industry name is bonused twice.
    #[test]
    fn the_triage_model_is_told_not_to_apply_the_deterministic_adjustments() {
        assert!(SYSTEM_TRIAGE.contains("appliquée hors de ta note"));
        assert!(SYSTEM_TRIAGE.contains("Ne les ajoute pas toi-même"));
    }

    /// The deep rubric is UNCHANGED from v3 on purpose. Pinned separately so the fact
    /// reads as deliberate rather than as an oversight: only the triage half was
    /// rewritten for the Score Breaking scale.
    #[test]
    fn the_deep_rubric_is_deliberately_untouched() {
        assert_eq!(fingerprint(SYSTEM_DEEP), DEEP_FINGERPRINT);
        assert!(SYSTEM_DEEP.contains("Ta note REMPLACE celle du filtre rapide"));
    }
}
