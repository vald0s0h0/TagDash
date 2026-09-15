// Le score Dislocation : trois tiers, rien d'autre.
//
//   score = (RVOL + ATTENTION + NEWS) / 3
//
// Chaque tiers vaut 0..100 et répond à une question distincte :
//
//   RVOL       le ticker se réveille-t-il par rapport à SON PROPRE passé ?
//   ATTENTION  le marché le regarde-t-il, LUI, nettement plus que le peloton ?
//   NEWS       la news qui explique le mouvement est-elle fraîche et bonne ?
//
// Les deux premiers se recalculent à chaque seconde depuis la bande ; le troisième
// vient du LLM NEWS SCORING et ne bouge plus une fois rendu (seule sa fraîcheur le
// fait décroître). Il n'y a AUCUN bonus ni pénalité en points : un secteur porteur,
// une juridiction à risque ou un reverse split ne déplacent plus le score. Ce sont
// des filtres d'univers (option) ou de l'information affichée, jamais de
// l'arithmétique cachée.
//
// ── POURQUOI UN PLANCHER SUR LA BASELINE RVOL ─────────────────────────────────
// Mesuré sur les 20 séances de fichiers minute (25 905 couples symbole × tranche de
// 10 min du premarket) : **50 % des baselines valent exactement zéro**. La formule
// `(volume+1)/(attendu+1)` dégénère alors en « volume brut », et « RVOL 2 055 » ne
// veut plus dire « 2 055 fois son habitude » mais « 2 054 actions ont été
// échangées ». C'est ce qui rendait le même nombre incomparable d'un ticker à
// l'autre — et c'est ce qui a fait passer JUNS le 21 août pour un RVOL de 2 055 à
// 11:02 sur 2 323 actions.
//
// Le plancher répond « comparé à quoi ? » : sous un volume de référence réel, on
// compare le ticker à ce plancher plutôt qu'à son propre bruit.
//
// ── D'OÙ VIENNENT LES ANCRES ──────────────────────────────────────────────────
// « 100 = rare mais pas exceptionnel, du genre deux fois par semaine ». Mesuré, pas
// choisi : sur ces mêmes 20 séances, plancher appliqué, le pic de RVOL minute par
// symbole-jour dépasse
//
//     600x   ~50 fois par semaine       2 788x  ~10 fois par semaine
//   1 680x   ~20 fois par semaine       6 133x   ~2 fois par semaine
//
// et l'attention, mesurée sur la séance du 21 août : JUNS z=17,6 · ADXN z=14,3 ·
// BIVI z=5,5 · puis chute à 2,75 pour le quatrième. Les deux vrais mouvements du
// jour sont détachés d'un ordre de grandeur du reste.

// ─── RVOL ──────────────────────────────────────────────────────────────────────

/// Volume de référence minimal, en actions par minute, sous lequel un ticker n'a
/// pas de baseline exploitable. ~p75 de la distribution réelle des baselines
/// premarket (la moitié valent zéro, la médiane des non-nulles est à 378).
pub const RVOL_BASELINE_FLOOR_PER_MIN: f64 = 500.0;

/// En dessous, il ne se passe rien : le tiers RVOL vaut 0.
pub const RVOL_ORDINARY: f64 = 3.0;
/// Le tiers RVOL vaut 100. ~2 fois par semaine sur les 20 séances mesurées.
pub const RVOL_RARE: f64 = 6_000.0;

/// Le plancher, appliqué à un attendu déjà exprimé pour la fenêtre `tempo_secs`.
/// Utilisé par la baseline roulante, dont la médiane est déjà par fenêtre.
pub fn floor_expected(expected_for_window: f64, tempo_secs: i64) -> f64 {
    let floor = RVOL_BASELINE_FLOOR_PER_MIN * tempo_secs as f64 / 60.0;
    if expected_for_window.is_finite() {
        expected_for_window.max(floor)
    } else {
        floor
    }
}

/// Attendu pour une fenêtre de `tempo_secs`, à partir d'un volume typique par
/// tranche de 10 minutes (la forme dans laquelle la baseline historique est
/// stockée), plancher compris.
pub fn expected_volume(typical_per_10min: f64, tempo_secs: i64) -> f64 {
    floor_expected(
        typical_per_10min.max(0.0) * tempo_secs as f64 / 600.0,
        tempo_secs,
    )
}

/// Le tiers RVOL, 0..100. Logarithmique : la distribution est violemment
/// asymétrique à droite, une échelle linéaire écraserait tout entre 0 et 1.
///
/// `None` — RVOL non mesurable — vaut 0, pas « le pire du marché » : c'est une
/// absence de preuve, et elle coûte simplement son tiers.
pub fn rvol_score(ratio: Option<f64>) -> f64 {
    let Some(r) = ratio else { return 0.0 };
    if !r.is_finite() || r <= RVOL_ORDINARY {
        return 0.0;
    }
    let span = (RVOL_RARE / RVOL_ORDINARY).ln();
    (100.0 * (r / RVOL_ORDINARY).ln() / span).clamp(0.0, 100.0)
}

// ─── Attention ─────────────────────────────────────────────────────────────────

/// Détachement, en sigmas robustes, au-delà duquel le ticker EST l'histoire de la
/// minute.
///
/// ATTENTION AU PIÈGE : `dominance_z` dépend de la TAILLE ET DE L'ÉTALEMENT de la
/// population du tick, pas seulement de la force du ticker. Le même allumage de
/// JUNS, le 21 août à 11:09, mesure z = 17,6 dans un run de séance complète (des
/// centaines de symboles actifs, médiane d'attention basse) et z = 3,1 dans un
/// replay de vingt minutes qui démarre à 07:00 (douze symboles actifs, tous des
/// mouvements, médiane haute et MAD large). Un tiers de score adossé au seul z
/// n'est donc pas comparable d'un run à l'autre — c'est précisément ce qui a fait
/// disparaître JUNS de la fenêtre 7h-7h20.
pub const ATTENTION_Z_RARE: f64 = 4.0;

/// Part du tiers attention qui vient du rang transversal plutôt que du détachement.
///
/// MAJORITAIRE, et pour la raison ci-dessus : le composite transversal est stable
/// entre les deux runs (99,4 dans les deux cas) parce qu'il est déjà fait de
/// percentiles — « en tête du peloton » est exactement ce qu'il mesure. Le z reste,
/// mais comme modulateur de « et de très loin », pas comme porteur principal.
pub const ATTENTION_RANK_WEIGHT: f64 = 0.55;

/// Le tiers ATTENTION, 0..100. `composite` est l'attention transversale 0..100 du
/// tick (flux, tape, continuité, déplacement), `z` sa distance à la médiane du
/// marché en sigmas robustes.
pub fn attention_score(z: f64, composite: f64) -> f64 {
    let detachment = if z.is_finite() {
        (z / ATTENTION_Z_RARE).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let lead = (composite / 100.0).clamp(0.0, 1.0);
    100.0 * ((1.0 - ATTENTION_RANK_WEIGHT) * detachment + ATTENTION_RANK_WEIGHT * lead)
}

// ─── News ──────────────────────────────────────────────────────────────────────

/// En dessous, la news est « à l'instant » : elle vaut sa note pleine.
pub const NEWS_FULL_MINUTES: i64 = 5;
/// Au-delà, le marché l'a lue : elle ne vaut plus rien pour un départ qui commence
/// maintenant. C'est ce plafond qui remplace l'ancien seuil dur — la décote fait
/// le travail toute seule, sans marche d'escalier.
pub const NEWS_MAX_MINUTES: i64 = 20;

/// Ce qu'il reste de la fenêtre de fraîcheur, 1.0 à 0.0.
///
/// Un âge négatif est une news horodatée dans le futur — dérive d'horloge côté
/// fournisseur, jamais un catalyseur — et vaut 0.
pub fn news_freshness(age_minutes: i64) -> f64 {
    if age_minutes < 0 {
        return 0.0;
    }
    if age_minutes <= NEWS_FULL_MINUTES {
        return 1.0;
    }
    if age_minutes >= NEWS_MAX_MINUTES {
        return 0.0;
    }
    (NEWS_MAX_MINUTES - age_minutes) as f64 / (NEWS_MAX_MINUTES - NEWS_FULL_MINUTES) as f64
}

/// Le tiers NEWS, 0..100 : la note du LLM, décotée par l'âge du titre.
pub fn news_score(model_score: f64, age_minutes: i64) -> f64 {
    (model_score.clamp(0.0, 100.0) * news_freshness(age_minutes)).clamp(0.0, 100.0)
}

/// Y a-t-il quelque chose à faire noter au modèle ? Sans titre dans la fenêtre, le
/// tiers vaut zéro et c'est une certitude : pas d'appel.
pub fn worth_a_model_call(age_minutes: Option<i64>) -> bool {
    age_minutes.is_some_and(|a| news_freshness(a) > 0.0)
}

// ─── Le score ──────────────────────────────────────────────────────────────────

/// Les trois tiers d'un candidat. `news` vaut `None` tant que le modèle n'a pas
/// répondu : « on ne sait pas encore » et « il n'y a rien » sont deux affirmations
/// opposées et ne doivent pas produire le même affichage.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Scores {
    pub rvol: f64,
    pub attention: f64,
    pub news: Option<f64>,
}

impl Scores {
    /// Le score affiché en gros. Un tiers news encore inconnu compte zéro : le
    /// total ne se gonfle jamais d'une note qui n'existe pas.
    pub fn total(&self) -> f64 {
        ((self.rvol + self.attention + self.news.unwrap_or(0.0)) / 3.0).clamp(0.0, 100.0)
    }

    /// Le classement AVANT le modèle : on ne peut ordonner que sur ce qu'on a déjà.
    pub fn tape(&self) -> f64 {
        ((self.rvol + self.attention) / 2.0).clamp(0.0, 100.0)
    }

    /// La ligne d'audit, dans l'ordre où le trader les lit.
    pub fn describe(&self) -> String {
        format!(
            "RVOL {:.0} · attention {:.0} · news {} → {:.0}",
            self.rvol,
            self.attention,
            match self.news {
                Some(n) => format!("{n:.0}"),
                None => "…".to_string(),
            },
            self.total()
        )
    }
}

/// Les deux interrupteurs qui retirent purement et simplement un ticker de la
/// surveillance. Ce ne sont pas des points : un ticker exclu n'est pas mal noté, il
/// n'existe pas pour la stratégie.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filters {
    pub hot_industry_only: bool,
    pub exclude_risk_domicile: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rvol_floor_stops_a_near_zero_baseline_from_inventing_a_ratio() {
        // JUNS, 21 août 11:02 : 2 323 actions sur une baseline effondrée. Le moteur
        // affichait 2 055x ; ce n'était que le volume brut déguisé.
        let expected = expected_volume(0.0, 60);
        assert_eq!(expected, RVOL_BASELINE_FLOOR_PER_MIN);
        let ratio = 2_323.0 / expected;
        assert!(ratio < 5.0, "2 323 actions ne sont pas un réveil, got {ratio}");

        // La même formule laisse intacte une baseline réelle.
        let real = expected_volume(18_239.0, 60);
        assert!((real - 1_823.9).abs() < 0.1, "got {real}");
    }

    #[test]
    fn the_rvol_third_spans_the_measured_distribution() {
        assert_eq!(rvol_score(None), 0.0);
        assert_eq!(rvol_score(Some(1.0)), 0.0);
        assert_eq!(rvol_score(Some(RVOL_ORDINARY)), 0.0);
        // Les ancres mesurées : ~10 fois par semaine à mi-parcours, 2 fois par
        // semaine au sommet.
        let ten_a_week = rvol_score(Some(2_788.0));
        assert!(
            (85.0..95.0).contains(&ten_a_week),
            "10/semaine devrait être haut sans saturer, got {ten_a_week}"
        );
        assert_eq!(rvol_score(Some(RVOL_RARE)), 100.0);
        assert_eq!(rvol_score(Some(1e9)), 100.0);
        // Monotone, sans marche.
        assert!(rvol_score(Some(50.0)) > rvol_score(Some(20.0)));
    }

    #[test]
    fn attention_survives_a_thin_population() {
        // Le même allumage, mesuré dans deux runs de tailles différentes, doit
        // donner à peu près la même note. C'est le test que l'ancienne pondération
        // échouait : 89 contre 34.
        let full_session = attention_score(17.6, 99.8); // séance complète
        let short_window = attention_score(3.13, 99.8); // replay de 20 min
        assert_eq!(full_session.round(), 100.0);
        assert!(short_window > 80.0, "got {short_window}");

        // Premier d'un marché mort : le rang, sans la prime de détachement.
        let dead = attention_score(0.0, 90.0);
        assert!((45.0..55.0).contains(&dead), "got {dead}");
        // Et il ne va nulle part : sans RVOL ni news, son total reste dérisoire.
        let alone = Scores {
            rvol: 0.0,
            attention: dead,
            news: Some(0.0),
        };
        assert!(alone.total() < 20.0, "got {}", alone.total());
    }

    #[test]
    fn freshness_decays_instead_of_cutting() {
        assert_eq!(news_freshness(0), 1.0);
        assert_eq!(news_freshness(5), 1.0);
        assert_eq!(news_freshness(20), 0.0);
        assert_eq!(news_freshness(120), 0.0);
        assert_eq!(news_freshness(-3), 0.0, "horodatage futur");
        let mid = news_freshness(12);
        assert!((0.5..0.6).contains(&mid), "got {mid}");
        // Une news excellente mais tiède ne vaut plus une news excellente.
        assert!(news_score(90.0, 15) < news_score(90.0, 2));
    }

    #[test]
    fn a_model_call_needs_something_to_grade() {
        assert!(worth_a_model_call(Some(0)));
        assert!(worth_a_model_call(Some(19)));
        assert!(!worth_a_model_call(Some(20)));
        assert!(!worth_a_model_call(Some(2_800)));
        assert!(!worth_a_model_call(None));
    }

    #[test]
    fn the_total_never_credits_a_note_that_does_not_exist() {
        let pending = Scores {
            rvol: 90.0,
            attention: 90.0,
            news: None,
        };
        assert_eq!(pending.total().round(), 60.0);
        assert_eq!(pending.tape().round(), 90.0);
        let judged = Scores {
            news: Some(90.0),
            ..pending
        };
        assert_eq!(judged.total().round(), 90.0);
        // Le classement avant modèle ne dépend pas du tiers manquant.
        assert_eq!(pending.tape(), judged.tape());
    }
}
