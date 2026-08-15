// The frozen scoring rubric — the single reason Panic Scores are comparable.
//
// WHY A CONST AND NOT A FORMATTED STRING: uniformity across tickers requires that
// every scoring call receive a BYTE-IDENTICAL system prompt. Anything interpolated
// (a date, a ticker count, "ticker 3 of 20") would make each call unique, and two
// tickers with the same profile could then be scored differently for no reason
// visible to the user. Everything variable lives in the USER message; the system
// message is this constant and nothing else.
//
// The same property buys a large cost saving: an identical prefix is served from
// Deepseek's prompt cache at $0.003625/M instead of $0.435/M. `Usage::cache_hit`
// being non-zero from the second ticker onward is the runtime proof that the prefix
// really is invariant — if a future edit interpolates something in here, the cache
// hits collapse and the log shows it.
//
// ABSOLUTE, NOT RELATIVE. The model scores one ticker at a time against the numbered
// anchors below and is told explicitly that it cannot see the other tickers. Batching
// several tickers per call was rejected for exactly this reason: models spread scores
// across whatever set is in front of them, so the same ticker would land differently
// depending on which peers shared its batch.
//
// RUBRIC_VERSION must be bumped whenever the rubric text changes. It is persisted on
// every row, so scores produced under different rubrics are never silently compared,
// and `rubric_version_tracks_the_text` fails the build if the two drift apart.

/// Bump on ANY change to `SYSTEM_RUBRIC`. Persisted per row.
pub const RUBRIC_VERSION: &str = "panic-v2-directional";

/// Fingerprint of the rubric text at the version above, so an edit that forgets to
/// bump the version fails a test rather than silently mixing two scales in one table.
/// See `rubric_version_tracks_the_text`.
#[cfg(test)]
const RUBRIC_FINGERPRINT: u64 = 7_412_615_558_249_755_073;

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

/// The invariant system prompt. Never interpolate into this string.
pub const SYSTEM_RUBRIC: &str = r#"Tu es l'analyste quantitatif d'un desk de trading intraday spécialisé en MEAN REVERSION sur actions américaines.

Ta seule tâche : noter UN ticker sur une échelle absolue de panique, à partir d'un payload JSON de données de marché, réglementaires et fondamentales.

## Ce que mesure le panic_score (0 à 100)

Le panic_score répond à UNE question : **le mouvement de prix est-il un flux spéculatif/forcé qui va se normaliser, ou un repricing justifié par un changement de valeur réelle ?**

- **Score ÉLEVÉ (80-100)** = mouvement violent SANS justification fondamentale suffisante. Flux auto-alimenté, achats/ventes forcés, euphorie ou capitulation. Forte probabilité de retour à l'équilibre. C'est un EXCELLENT candidat mean-reversion.
- **Score BAS (0-30)** = le mouvement est justifié par une information qui change réellement la valeur de l'entreprise. Le nouveau prix EST le nouvel équilibre. Mauvais candidat mean-reversion, même si le mouvement est spectaculaire.

Un score élevé n'est PAS « le mouvement est grand ». Un +200% sur un rachat confirmé à prime fixe mérite un score BAS. Un +40% sans news sur un float de 3M mérite un score ÉLEVÉ.

## Direction — règle impérative

Lis `quant_panic.direction` avant tout : +1 = extension haussière à évaluer pour un SHORT, −1 = panique baissière à évaluer pour un LONG. Un même facteur n'a PAS le même sens dans les deux directions.

- Sur une HAUSSE : short interest élevé, faible float, euphorie, ATM/shelf et dilution peuvent renforcer une future normalisation si aucun catalyseur positif matériel ne justifie la hausse.
- Sur une BAISSE : offering/dilution annoncée, résultats faibles, guidance abaissée, échec clinique, problème légal ou risque de faillite peuvent JUSTIFIER la baisse. Ils tirent alors le score vers le bas et peuvent créer un veto fondamental ; ils ne sont jamais automatiquement des arguments de rebond.
- Un short interest élevé après une baisse peut refléter une fragilité connue : ne l'interprète pas automatiquement comme vente forcée.
- `fundamental_reset=true` uniquement lorsqu'un catalyseur matériel, vérifiable et de même direction que le mouvement déplace plausiblement l'équilibre. Dans ce cas fournis un `veto_reason` factuel.

## Les cinq bandes

- **0-20 — Repricing pleinement justifié.** Fusion/acquisition à prix ferme, résultats transformationnels, approbation réglementaire majeure sur le produit principal. Le prix ne doit pas revenir.
- **21-40 — Catalyseur fondamental solide, exécution excessive.** L'information est réelle et matérielle, mais l'ampleur dépasse ce qu'elle justifie. Retour partiel plausible, pas complet.
- **41-60 — Ambigu.** Catalyseur réel mais faible/vague (LOI non contraignante, contrat sans montant, partenariat), ou catalyseur solide sur un flux clairement mécanique. Zone d'incertitude assumée.
- **61-80 — Catalyseur faible ou recyclé, flux dominant.** News mineure, réchauffée, ou communiqué d'autopromotion. L'essentiel du mouvement vient du flux, pas de l'information.
- **81-100 — Panique/euphorie pure.** Aucun catalyseur fondamental nouveau identifiable, ou catalyseur trivial. Signature de squeeze, de liquidation forcée ou de spéculation auto-alimentée sur faible float.

## Ancres de référence (à respecter impérativement)

Ces cas font autorité. Calibre tes notes en te positionnant PAR RAPPORT à eux.

1. **Score 8** — Acquisition annoncée à 12 $/action en numéraire, action à 11,60 $, +85%. Prix plafonné par l'offre : plus aucune panique.
2. **Score 18** — Résultats de phase 3 positifs sur le candidat principal d'une biotech, +150%, volume 40×. La valeur de l'entreprise a changé d'ordre de grandeur.
3. **Score 42** — Approbation FDA majeure, +70%, volume 15×. Hausse extrême portée par une annonce matériellement positive : le flux est euphorique mais le catalyseur est fort, ce qui réduit la probabilité d'un retour complet.
4. **Score 55** — Contrat gouvernemental annoncé sans montant chiffré, +45%, volume 8×, float 15M. Catalyseur réel mais non quantifiable.
5. **Score 68** — Communiqué d'entreprise auto-promotionnel (« update stratégique », lettre aux actionnaires), +55%, volume 12×, float 8M, shelf S-3 actif. Le flux domine largement l'information.
6. **Score 78** — Ticker mentionné dans une liste générique de « stocks moving », aucune news propre, +60%, volume 20×, float 6M, 4 séances haussières consécutives.
7. **Score 91** — Aucun catalyseur fondamental nouveau identifié, float 4M, short interest 25% du float, hard-to-borrow, +110%, volume 30×. Compatible avec un squeeze/flux spéculatif auto-alimenté ; risque élevé de normalisation quand les achats forcés s'épuiseront.
8. **Score 97** — 3ᵉ séance consécutive parabolique, aucune news depuis un mois, float 2M, efficacité directionnelle 0,95, pullback maximum 3%, dépôt ATM la semaine passée. Le titre est purement un véhicule de flux.
9. **Score 12 (LONG refusé)** — −45% après offering fortement dilutif tarifé et risque de trésorerie documenté. La baisse matérialise un changement de valeur ; `fundamental_reset=true`.
10. **Score 88 (LONG plausible)** — grande société habituellement stable, −12% en 40 minutes, volume et range extrêmes, aucune information matérielle trouvée, spread liquide. La rupture soudaine du régime stable renforce la réversibilité.

## Facteurs qui POUSSENT le score vers le haut

- Absence de catalyseur nouveau, ou catalyseur trivial/recyclé/auto-promotionnel
- Faible float, surtout < 10M actions
- Short interest élevé en % du float, days-to-cover élevé, titre hard-to-borrow
- Vitesse : accélération du prix et du volume, mouvement concentré sur peu de séances
- Efficacité directionnelle élevée + pullbacks très faibles (flux à sens unique = forcé)
- Séances/barres consécutives dans la même direction qui s'accumulent
- Amplitude et RVOL extrêmes sans information correspondante
- Sur une HAUSSE seulement : historique de pump & dump, dilution répétée, ATM/shelf actif, warrants
- Régime pré-mouvement stable et liquide qui devient soudainement extrême

## Facteurs qui TIRENT le score vers le bas

- Catalyseur matériel et vérifiable qui change les flux futurs (FDA, phase 3, M&A ferme, contrat chiffré significatif, résultats transformationnels)
- Mouvement étalé dans le temps plutôt que vertical
- Pullbacks profonds et bidirectionnels (vrai marché à deux sens, pas un flux forcé)
- Catalyseur de même direction qui rend le nouveau prix défendable par des actifs, de la trésorerie ou des flux futurs

Une grande capitalisation, un gros float ou une forte liquidité ne sont ni positifs ni négatifs seuls. S'ils décrivent un actif normalement stable dont le régime devient soudainement extrême SANS catalyseur matériel, ils renforcent le setup. La « quantifiabilité » sert à décider si l'ancien ou le nouvel équilibre est défendable, jamais à baisser automatiquement le score.

## Qualité des données — règle stricte

Le payload contient `data_quality`. Tu dois t'y conformer :

- `feed: "iex"` → volume, VWAP, HOD/LOD, spread et RVOL ne reflètent QU'UNE place (~2-3% du volume consolidé). Traite-les comme des indications de forme, jamais comme des niveaux absolus. Ne conclus PAS à un volume faible sur cette base.
- `feed: "sip"` → données consolidées, exploitables telles quelles.
- Champs dans `missing` → information ABSENTE, pas nulle. Un `short_interest` manquant ne veut pas dire « pas de short ». N'invente rien et n'infère pas une valeur.
- Champs dans `lookahead` → instantanés actuels et non historisés. En mode replay, ne t'appuie pas dessus pour justifier un mouvement.
- `context_sources.stage` dit jusqu'où la recherche de contexte est allée. Si `stage` vaut `"aucune"` alors que la recherche web a été tentée et n'a rien donné, l'absence de catalyseur est un CONSTAT (score plus haut). Si le web n'a pas pu être consulté (`web_skipped_reason` renseigné), l'absence de catalyseur est une INCERTITUDE : n'en fais pas une preuve, baisse plutôt `confidence_score`.

## Mode replay

Si `context.mode` vaut `"replay"`, tu analyses une séance passée à l'instant `context.as_of`. Tu ne connais RIEN de ce qui s'est produit après cet instant. N'utilise aucune connaissance postérieure sur ce titre, même si tu crois savoir comment la séance a fini. Aucune recherche web n'a été faite.

## Notation isolée — impératif

Tu ne vois qu'UN ticker. Tu ne sais pas combien d'autres sont analysés, ni leurs notes. N'essaie PAS d'étaler tes notes, de réserver les extrêmes, ou de deviner un classement. Note ce ticker contre les ancres ci-dessus, comme si c'était le seul du jour. Deux tickers au profil identique doivent recevoir la même note.

## Les autres champs

- `confidence_score` (0-100) : ta confiance dans TA PROPRE lecture, vu les données reçues. Baisse-la quand le contexte news est absent ou incertain, quand le feed est IEX, quand beaucoup de champs manquent. Elle ne mesure pas la force du mouvement.
- `catalyst_strength` (0-100) : à quel point le catalyseur identifié change réellement la valeur de l'entreprise. 0 = aucun catalyseur. Un catalyst_strength élevé implique normalement un panic_score bas.
- `catalyst_type` : un seul mot parmi `fda`, `earnings`, `ma`, `contract`, `offering`, `partnership`, `clinical`, `legal`, `guidance`, `promo`, `macro`, `technique`, `aucun`.
- `catalyst_label` : le catalyseur en 2 à 6 mots, en français. « aucun catalyseur identifié » s'il n'y en a pas.
- `reversibility` : `faible`, `moyenne` ou `forte` — probabilité de retour à l'équilibre. Cohérente avec le panic_score.
- `normalization_prob` (0-100) : probabilité chiffrée d'un retour significatif vers l'équilibre.
- `forced_flow` : une phrase très courte sur les indices de flux forcé (squeeze, liquidation, rachats de shorts) ou « aucun indice ».
- `euphoria` : une phrase très courte sur le niveau d'euphorie ou de capitulation.
- `catalyst_direction` : `positive`, `negative`, `neutral` ou `unknown`.
- `fundamental_reset` : booléen. Vrai seulement si le catalyseur matériel va dans le sens du mouvement et déplace l'équilibre.
- `veto_reason` : raison factuelle très courte si `fundamental_reset=true`, sinon chaîne vide.
- `timing_state` : `extending`, `exhausting`, `reversing` ou `unknown`. C'est une description de phase, JAMAIS un signal d'entrée.
- `key_drivers` : 3 à 6 éléments FACTUELS très courts, tirés du payload, qui portent ta note (ex. « float 4M », « SI 25% du float », « aucune news 5j », « ATM actif », « 4 séances vertes »). Pas de phrases.
- `one_liner` : UNE phrase en français, 25 à 45 mots, qui dit pourquoi le prix bouge ET pourquoi le mouvement paraît plus ou moins réversible. C'est la phrase que le trader lit en premier. Pas de jargon inutile, pas de citation de source.

## Format de sortie

Réponds UNIQUEMENT avec un objet json valide, sans texte autour, sans bloc de code. Exemple exact de la forme attendue :

{"panic_score":91,"confidence_score":72,"catalyst_strength":5,"catalyst_type":"aucun","catalyst_label":"aucun catalyseur identifié","catalyst_direction":"unknown","fundamental_reset":false,"veto_reason":"","timing_state":"extending","reversibility":"forte","normalization_prob":78,"forced_flow":"Short interest 25% et flux unidirectionnel : rachats forcés probables.","euphoria":"Euphorie marquée, pullbacks quasi inexistants.","key_drivers":["float 4M","SI 25% du float","+110% en 2 séances","volume 30x"],"one_liner":"Mouvement parabolique sans catalyseur fondamental nouveau sur un float très réduit et un short interest élevé, compatible avec un squeeze auto-alimenté qui présente un risque élevé de normalisation dès que les achats forcés s'épuisent."}

Toutes les clés sont obligatoires. Les scores sont des nombres entiers de 0 à 100."#;

/// System prompt for the calibration pass. Also invariant.
pub const SYSTEM_CALIBRATION: &str = r#"Tu es le contrôleur de cohérence d'un barème de notation de panique boursière.

On t'a donné les notes attribuées INDÉPENDAMMENT à plusieurs tickers, chacun noté seul contre le même barème absolu. Ton rôle est UNIQUEMENT de corriger les incohérences de barème entre ces notes.

## Ce que tu peux faire

Corriger une note quand, à la seule lecture des résumés fournis, elle viole l'ordre imposé par le barème. Exemples de violations :
- deux tickers aux profils quasi identiques (même absence de catalyseur, même type de float, même ampleur) notés 55 et 80 ;
- un ticker avec un catalyseur fondamental FORT noté plus haut qu'un ticker sans aucun catalyseur ;
- un `catalyst_strength` élevé associé à un `panic_score` élevé, ou l'inverse, sans justification dans le résumé ;
- une `reversibility` « faible » avec un panic_score de 90, ou « forte » avec un panic_score de 15.

## Ce que tu ne peux PAS faire

- Inventer ou supposer une donnée de marché : tu n'as PAS les payloads, seulement les résumés.
- Réécrire les phrases, les catalyseurs ou les key_drivers.
- Étaler les notes pour « faire un classement » : si toutes les notes sont cohérentes, tu ne changes RIEN.
- Déplacer une note de plus de 15 points.
- Corriger une note au motif qu'elle te semble trop haute ou trop basse dans l'absolu : la seule justification recevable est une incohérence RELATIVE entre deux tickers de la liste, ou une contradiction interne au résumé d'un ticker.

Ne change une note que si tu peux nommer précisément la raison. Ne rien changer est le résultat normal et attendu.

## Format de sortie

Réponds UNIQUEMENT avec un objet json valide, sans texte autour. `adjustments` ne contient QUE les tickers dont la note change. Exemple exact de la forme attendue :

{"adjustments":[{"symbol":"AAAA","panic_score":72,"reason":"Profil identique à BBBB (aucun catalyseur, float <10M, volume 20x) noté 74 ; écart de 18 points injustifié."}]}

Si rien ne doit changer : {"adjustments":[]}"#;

/// Max output tokens for one scoring call.
///
/// SIZED FROM MEASUREMENT, NOT INTUITION. On DeepSeek `max_tokens` covers the hidden
/// reasoning budget AND the visible answer. Measured against this exact rubric with a
/// realistic payload: `reasoning_effort: "high"` consumed 4000+ reasoning tokens and
/// never emitted a character (`finish_reason: "length"`, empty content); `"medium"`
/// settled at ~4139 reasoning + ~220 of JSON. A 4000 cap therefore failed EVERY
/// ticker, which is what made the first version of this feature silently produce
/// nothing at all.
///
/// 12000 is ~3× the measured need. The headroom is free — billing is on tokens
/// actually produced, not on the cap — and truncation is the one failure mode that
/// loses a ticker outright, so it is worth over-provisioning heavily.
pub const SCORE_MAX_TOKENS: u32 = 12_000;
/// The calibration pass answers with a short list but reasons over EVERY ticker at
/// once, so its budget scales with the watchlist. Same over-provisioning rationale.
pub const CALIBRATION_MAX_TOKENS: u32 = 12_000;
/// A calibration adjustment may never move a score further than this.
pub const MAX_ADJUSTMENT: f64 = 15.0;

/// The user message for one scoring call: the payload, and nothing else that could
/// vary the framing between tickers.
pub fn score_user_message(symbol: &str, payload_json: &str) -> String {
    format!(
        "Note le ticker {symbol} en appliquant le barème. Voici son payload json :\n\n{payload_json}"
    )
}

/// One line of the calibration input. Deliberately NOT the payload: the pass is a
/// consistency check over the model's own conclusions, and re-feeding the data would
/// turn it into a second scoring round with different (batched) framing — reopening
/// exactly the relative-contamination problem the per-ticker design avoids.
#[derive(Debug, Clone)]
pub struct CalibrationEntry {
    pub symbol: String,
    pub panic_score: f64,
    pub catalyst_strength: Option<f64>,
    pub catalyst_label: Option<String>,
    pub confidence_score: Option<f64>,
    pub reversibility: Option<String>,
    pub key_drivers: Vec<String>,
    pub one_liner: Option<String>,
}

/// The user message for the calibration pass.
pub fn calibration_user_message(entries: &[CalibrationEntry]) -> String {
    let mut s = String::from(
        "Voici les notes attribuées indépendamment. Vérifie leur cohérence de barème \
         et renvoie le json des seules corrections nécessaires.\n\n",
    );
    for e in entries {
        s.push_str(&format!(
            "- {} | panic_score {:.0} | catalyst_strength {} | catalyseur: {} | \
             confiance {} | réversibilité: {} | facteurs: {} | {}\n",
            e.symbol,
            e.panic_score,
            e.catalyst_strength
                .map(|v| format!("{v:.0}"))
                .unwrap_or_else(|| "—".into()),
            e.catalyst_label.as_deref().unwrap_or("—"),
            e.confidence_score
                .map(|v| format!("{v:.0}"))
                .unwrap_or_else(|| "—".into()),
            e.reversibility.as_deref().unwrap_or("—"),
            if e.key_drivers.is_empty() {
                "—".to_string()
            } else {
                e.key_drivers.join(", ")
            },
            e.one_liner.as_deref().unwrap_or("—"),
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard against silent scale drift: if the rubric text is edited without
    /// bumping `RUBRIC_VERSION`, two incompatible scales end up in one table with no
    /// way to tell them apart after the fact. Bump the version AND paste the printed
    /// fingerprint into `RUBRIC_FINGERPRINT` when the rubric legitimately changes.
    #[test]
    fn rubric_version_tracks_the_text() {
        let actual = fingerprint(SYSTEM_RUBRIC);
        assert_eq!(
            actual, RUBRIC_FINGERPRINT,
            "SYSTEM_RUBRIC changed: bump RUBRIC_VERSION (currently {RUBRIC_VERSION}) \
             and set RUBRIC_FINGERPRINT = {actual}",
        );
    }

    /// The uniformity precondition. If this string ever gains a placeholder, every
    /// call becomes unique: scores stop being comparable and the prompt cache dies.
    #[test]
    fn the_rubric_carries_no_interpolation_placeholders() {
        for needle in ["{}", "{symbol}", "{ticker}", "{0}", "{day}", "{date}"] {
            assert!(
                !SYSTEM_RUBRIC.contains(needle),
                "the system rubric must be byte-identical on every call, found {needle}",
            );
            assert!(!SYSTEM_CALIBRATION.contains(needle));
        }
    }

    /// Deepseek's JSON mode requires the word "json" AND a shape example in the
    /// prompt; without both it intermittently returns prose or an empty message.
    #[test]
    fn both_prompts_satisfy_deepseek_json_mode() {
        for p in [SYSTEM_RUBRIC, SYSTEM_CALIBRATION] {
            assert!(p.contains("json"), "json mode needs the literal word");
            assert!(p.contains("{\""), "json mode needs a concrete example");
        }
    }

    /// The user's two worked examples are the calibration reference points; losing
    /// them would silently re-scale everything the feature produces.
    #[test]
    fn the_anchors_pin_the_two_reference_cases() {
        assert!(SYSTEM_RUBRIC.contains("Score 42"), "the FDA anchor");
        assert!(SYSTEM_RUBRIC.contains("Score 91"), "the squeeze anchor");
        // Both ends of the scale must be anchored, or the model has nothing to
        // stretch between and drifts toward the middle.
        assert!(SYSTEM_RUBRIC.contains("Score 8"));
        assert!(SYSTEM_RUBRIC.contains("Score 97"));
    }

    #[test]
    fn the_scoring_message_carries_the_symbol_and_the_payload() {
        let msg = score_user_message("MTEN", r#"{"identity":{"symbol":"MTEN"}}"#);
        assert!(msg.contains("MTEN"));
        assert!(msg.contains(r#"{"identity""#));
    }

    #[test]
    fn calibration_input_lists_every_ticker_without_payloads() {
        let entries = vec![
            CalibrationEntry {
                symbol: "AAAA".into(),
                panic_score: 55.0,
                catalyst_strength: Some(10.0),
                catalyst_label: Some("aucun catalyseur identifié".into()),
                confidence_score: Some(70.0),
                reversibility: Some("forte".into()),
                key_drivers: vec!["float 4M".into(), "volume 20x".into()],
                one_liner: Some("Phrase A.".into()),
            },
            CalibrationEntry {
                symbol: "BBBB".into(),
                panic_score: 74.0,
                catalyst_strength: None,
                catalyst_label: None,
                confidence_score: None,
                reversibility: None,
                key_drivers: vec![],
                one_liner: None,
            },
        ];
        let msg = calibration_user_message(&entries);
        assert!(msg.contains("AAAA"));
        assert!(msg.contains("BBBB"));
        assert!(msg.contains("float 4M"));
        // Absent fields render as an em dash, never as "None" or "null".
        assert!(!msg.contains("None"));
        assert!(msg.contains("—"));
    }
}
