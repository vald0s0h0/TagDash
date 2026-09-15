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
// TWO PASSES. `SYSTEM_NEWS` is the fast read that owns one third of the score (a few
// seconds, no thinking budget, the headlines and nothing else). When "Qualité A+" is
// selected, `SYSTEM_DEEP` provides an independent second opinion with fundamentals,
// filings and dilution attached. Its score REPLACES the news pass's; it never adds.
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

/// Bump on ANY change to `SYSTEM_NEWS` or `SYSTEM_DEEP`. Persisted per row.
pub const RUBRIC_VERSION: &str = "score-v6-thirds";

/// Fingerprint of both rubric texts at the version above, so an edit that forgets to
/// bump the version fails a test rather than silently mixing two scales in one table.
#[cfg(test)]
const NEWS_FINGERPRINT: u64 = 651_325_393_890_734_417;
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
/// The news pass answers six short fields with thinking disabled. 700 leaves
/// generous headroom over the ~250 tokens the shape actually needs, so a verbose
/// `one_liner` never truncates the JSON.
pub const NEWS_MAX_TOKENS: u32 = 700;
/// The deep pass runs with a reasoning budget. Measured on the panic rubric: medium
/// effort burns ~4 000 reasoning tokens before emitting ~250 of JSON, and a budget
/// that merely covers the answer produces an empty message with `finish_reason:
/// "length"`. 12 000 is the same figure `panic_llm` settled on.
pub const DEEP_MAX_TOKENS: u32 = 12_000;

/// The default NEWS SCORING rubric.
///
/// SCOPE: the news, and nothing else — there is nothing else in the payload. The
/// tape half of the decision is two other thirds of the score, measured by the
/// engine; the freshness decay is applied deterministically outside this answer.
/// Everything this text asks for is a judgement on the substance of a headline.
///
/// This is the DEFAULT text. The copy actually sent lives in an editable file — see
/// `quality_llm::rubric`. Edit the file, not this constant, unless you mean to change
/// what a fresh install starts from.
pub const SYSTEM_NEWS: &str = r#"Tu es le lecteur de news d'un desk de scalping intraday sur actions américaines small cap. Tu ne prends AUCUNE décision de trade, et tu ne notes AUCUN aspect technique.

Tu reçois un ticker et les titres publiés à son sujet. Rien d'autre : pas de float, pas de volume, pas de VWAP, pas de dépôt SEC. C'est voulu. Ta seule question :

**cette news est-elle un vrai catalyseur, capable de porter le titre sur la séance ?**

Le moteur a déjà mesuré le mouvement et l'attention du marché, et il applique lui-même une décote sur l'âge du titre. Ne re-note ni l'un ni l'autre : ta note porte sur la SUBSTANCE de l'information.

## news_score (0 à 100)

- **90-100** — Catalyseur majeur, chiffré ou binaire : données cliniques positives, approbation réglementaire sur le produit principal, rachat ou fusion, contrat ou commande avec un montant significatif rapporté à la taille de l'émetteur.
- **70-89** — Catalyseur réel et vérifiable, d'un cran en dessous : contrat chiffré modeste, approbation sur un produit secondaire, accord définitif avec un partenaire nommé, résultats nettement supérieurs aux attentes.
- **50-69** — Information réelle mais discutable : annonce non chiffrée avec une contrepartie nommée, résultats corrects sans surprise.
- **30-49** — Faible : lettre d'intention non contraignante, « update » d'activité, nomination, participation à une conférence, extension d'un accord existant.
- **10-29** — Auto-promotion ou recyclage : « strategic update », rappel d'une annonce ancienne, mot-clé à la mode (IA, quantique, blockchain, crypto) accolé à une activité sans rapport, article de listing générique du type « stocks moving today ».
- **0-9** — Rien d'exploitable : aucun titre propre au ticker, ou uniquement un compte rendu publié APRÈS le départ du prix.

Un accord DÉFINITIF avec une contrepartie nommée n'est pas un « partenariat vague », même sans montant publié : il vaut la bande 70-89. Une intention, un protocole d'accord ou une exclusivité de négociation valent la bande 30-49.

## Causalité, pas fraîcheur

`news[].minutes_vs_move` est négatif quand le titre est paru AVANT le départ du prix, positif quand il est paru après.

- Positif → c'est un compte rendu du mouvement, pas sa cause : **news_score au maximum 15**.
- Négatif → le titre peut expliquer le départ. Note-le sur sa substance.

N'applique aucune autre décote liée au temps : `age_minutes` est traité hors de ta note.

## Verdict

Classe la news la plus importante dans `news_verdict` :

- `solide` — information matérielle et vérifiable qui change la valeur de l'entreprise.
- `faible` — information réelle mais mineure ou non chiffrée.
- `bluff` — auto-promotion, recyclage, ou mot-clé à la mode sans activité correspondante.
- `aucune` — aucun titre propre au ticker, ou uniquement des titres sans lien avec le mouvement.

Plafonds impératifs :
- `bluff` → news_score au maximum 25.
- `aucune` → news_score au maximum 10.
- La news annonce elle-même une émission d'actions, un placement ou une offre au marché → news_score au maximum 20 : l'émetteur vend dans la force, c'est un catalyseur baissier déguisé.

## Source et substance

Une dépêche d'agence primaire (Business Wire, GlobeNewswire, PR Newswire, Accesswire) reprenant un communiqué de l'émetteur est une information de première main. Un article d'agrégateur qui commente le mouvement n'en est pas une, même s'il est récent.

Lis `news[].body_excerpt` quand il est présent. Un titre peut promettre ce que le corps ne tient pas : « partenariat stratégique » sans contrepartie nommée vaut `faible`, pas `solide`, quelle que soit la formulation du titre.

Une même histoire reprise par trois dépêches ne vaut pas trois fois. Note la meilleure occurrence, une seule fois. S'il y a plusieurs histoires distinctes, note la plus forte et mentionne les autres dans `one_liner`.

## Mode replay

Si `mode` vaut `"replay"`, tu lis une séance PASSÉE, arrêtée à l'instant `as_of`. Tu ne sais RIEN de ce qui s'est produit après. N'utilise aucune connaissance postérieure sur ce titre, même si tu crois te souvenir de la façon dont la séance a tourné : ce serait noter le résultat au lieu de noter la news. Aucune recherche web n'a été faite.

## Format de sortie

Réponds UNIQUEMENT avec un objet json valide, sans texte autour, sans bloc de code. Exemple exact de la forme attendue :

{"news_score":88,"news_verdict":"solide","news_label":"données phase 2 positives","confidence":74,"red_flags":["accord non chiffré"],"one_liner":"Résultats de phase 2 positifs publiés six minutes avant le départ par communiqué de l'émetteur, critère principal atteint et chiffre à l'appui : catalyseur de première main, encore non digéré."}

Toutes les clés sont obligatoires. `news_score` et `confidence` sont des entiers de 0 à 100. `red_flags` contient 0 à 4 éléments très courts. `one_liner` fait 25 à 45 mots, en français, et dit ce que la news annonce et pourquoi elle porte (ou ne porte pas) le mouvement — c'est la seule phrase que le trader lira."#;

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
        let news = fingerprint(SYSTEM_NEWS);
        let deep = fingerprint(SYSTEM_DEEP);
        assert_eq!(
            (news, deep),
            (NEWS_FINGERPRINT, DEEP_FINGERPRINT),
            "a quality rubric changed: bump RUBRIC_VERSION (currently {RUBRIC_VERSION}) \
             and set NEWS_FINGERPRINT = {news}, DEEP_FINGERPRINT = {deep}",
        );
    }

    /// The uniformity precondition. If either string gains a placeholder, every call
    /// becomes unique: scores stop being comparable and the prompt cache dies.
    #[test]
    fn the_rubrics_carry_no_interpolation_placeholders() {
        for rubric in [SYSTEM_NEWS, SYSTEM_DEEP] {
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
            (SYSTEM_NEWS, "{\"news_score\":"),
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
    /// news score it replaces. They no longer answer the same QUESTION — the news
    /// pass grades the catalyst alone, deep grades the whole dossier — but both must
    /// still answer it on 0..100, upward, for a long.
    #[test]
    fn both_passes_share_one_scale_and_one_direction() {
        assert!(SYSTEM_NEWS.contains("news_score (0 à 100)"));
        assert!(SYSTEM_DEEP.contains("quality_score (0 à 100)"));
        assert!(
            SYSTEM_DEEP.contains("LONG uniquement"),
            "this strategy never evaluates a short"
        );
        assert!(
            SYSTEM_NEWS.contains("porter le titre sur la séance"),
            "this strategy never evaluates a short"
        );
    }

    /// The news pass grades the CATALYST, and the payload it receives contains
    /// nothing else to grade. Without this the model re-scores the move the engine
    /// already measured, and a departure with no news at all can out-score one with
    /// a real headline — the exact failure this rubric was rewritten to remove.
    #[test]
    fn the_news_rubric_grades_the_catalyst_and_caps_a_missing_one() {
        assert!(SYSTEM_NEWS.contains("Rien d'autre"));
        assert!(
            SYSTEM_NEWS.contains("`aucune` → news_score au maximum 10"),
            "no catalyst must be a floor, not a middling score"
        );
    }

    /// Causality is the model's rule; freshness is the engine's. If the rubric also
    /// decayed the score by age, the decay would be applied twice — once here and
    /// once in `score::news_score`.
    #[test]
    fn causality_is_the_models_rule_and_freshness_is_not() {
        assert!(SYSTEM_NEWS.contains("minutes_vs_move"));
        assert!(
            SYSTEM_NEWS.contains("`age_minutes` est traité hors de ta note"),
            "the age decay is deterministic and must not be applied twice"
        );
        assert!(SYSTEM_DEEP.contains("mêmes règles de fraîcheur"));
    }

    /// The deep rubric is UNCHANGED from v3 on purpose. Pinned separately so the fact
    /// reads as deliberate rather than as an oversight: only the news half was
    /// rewritten.
    #[test]
    fn the_deep_rubric_is_deliberately_untouched() {
        assert_eq!(fingerprint(SYSTEM_DEEP), DEEP_FINGERPRINT);
        assert!(SYSTEM_DEEP.contains("Ta note REMPLACE celle du filtre rapide"));
    }
}
