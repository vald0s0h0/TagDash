// ntfy push notifications — the "my phone must ring" path for Micro Pullback alerts.
//
// A premarket departure is worth minutes, not hours. When the trader isn't at the
// desk, a desktop flash and a dock bounce are worth nothing; a phone that rings is
// worth the whole feature. This module turns a pushed AlertSignal into a maximum-
// priority ntfy message.
//
// WHY JSON AND NOT HEADERS. ntfy's simplest publish form puts the title and tags in
// HTTP headers (`Title:`, `Tags:`, `Priority:`). HTTP header values are ASCII/latin-1
// — every alert this app produces is French prose full of accented characters, and
// header-encoding it produces mojibake on the phone ("dÃ©part 10s"). The JSON publish
// form takes a UTF-8 body and has no such limit, so that is the only form used here.
//
// WHAT "MAXIMUM ALERT" ACTUALLY MEANS ON iOS. `priority: 5` (max/urgent) is what
// makes the ntfy iOS app ring and vibrate rather than post silently. Two honest
// limits, both outside this app's reach:
//   • The camera flash is an iOS accessibility feature (Réglages → Accessibilité →
//     Audio et visuel → Flash LED pour alertes). No push payload from any app can
//     turn it on; the user enables it once and it then fires for these alerts.
//   • Bypassing silent mode / a Focus needs Apple's Critical Alerts entitlement,
//     which ntfy does not have. The reliable equivalent is to allow the ntfy app in
//     the iPhone's Focus exceptions, or to mark the ntfy notifications as
//     time-sensitive.
// Saying priority 5 "bypasses silent mode" would be a lie, so it isn't claimed here
// or in the UI.
//
// FIRE AND FORGET, AND EXACTLY ONCE. Sending is spawned, never awaited by the engine
// — a slow phone must not delay a scan pass. And it is deduplicated by `alert_id`,
// because Micro Pullback deliberately re-publishes the same alert as its tempo depth
// widens and as A+ scores land: without the guard, one departure would buzz the
// phone five times.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

use crate::config::secrets::Secrets;
use crate::types::AlertSignal;

/// The public server, used when no custom one is configured.
const DEFAULT_SERVER: &str = "https://ntfy.sh";

/// ntfy's maximum priority. 5 = "max/urgent": the iOS app rings and vibrates instead
/// of posting quietly.
const PRIORITY_MAX: u8 = 5;

/// Alert ids already pushed to the phone. Bounded below; cleared when it grows past
/// a session's worth of alerts.
static SENT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
/// A generous session's worth of alerts. Past this the set is cleared wholesale
/// rather than tracked forever — a duplicate buzz hours later is a far smaller
/// problem than an unbounded set in a process that runs for weeks.
const SENT_CAP: usize = 500;

/// Where to publish. Built from the secrets; None when no topic is configured, which
/// is the "feature not set up" state rather than an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NtfyConfig {
    pub server: String,
    pub topic: String,
    pub token: Option<String>,
}

impl NtfyConfig {
    /// Read the configuration out of the secrets. The topic is the only required
    /// part: a public ntfy.sh topic needs neither a server nor a token.
    pub fn from_secrets(s: &Secrets) -> Option<Self> {
        let topic = nonblank(&s.ntfy_topic)?;
        Some(Self {
            server: nonblank(&s.ntfy_server)
                .map(|u| u.trim_end_matches('/').to_string())
                .unwrap_or_else(|| DEFAULT_SERVER.to_string()),
            topic,
            token: nonblank(&s.ntfy_token),
        })
    }
}

fn nonblank(o: &Option<String>) -> Option<String> {
    o.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The JSON publish body. Field names are ntfy's own.
#[derive(Debug, Serialize)]
struct Publish<'a> {
    topic: &'a str,
    title: &'a str,
    message: &'a str,
    priority: u8,
    tags: Vec<&'a str>,
}

/// Publish one message. Returns the server's error text on failure so the Settings
/// test button can show something actionable rather than "échec".
pub async fn publish(
    cfg: &NtfyConfig,
    title: &str,
    message: &str,
    tags: Vec<&str>,
) -> Result<(), String> {
    let body = Publish {
        topic: &cfg.topic,
        title,
        message,
        priority: PRIORITY_MAX,
        tags,
    };
    // Posting to the server ROOT with `topic` in the body is ntfy's JSON publish
    // form. Posting to /<topic> would treat the body as the raw message text.
    let mut req = crate::http::client().post(&cfg.server).json(&body);
    if let Some(t) = &cfg.token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.map_err(|e| format!("réseau: {e}"))?;
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let detail = resp.text().await.unwrap_or_default();
    let detail = detail.trim();
    Err(if detail.is_empty() {
        format!("ntfy a répondu {status}")
    } else {
        format!("ntfy a répondu {status}: {detail}")
    })
}

/// Title and body for one alert, as they appear on the lock screen.
///
/// The title has to be readable at a glance while the phone is face-down on a desk,
/// so it leads with the ticker and the price — the two things that decide whether the
/// trader reaches for the phone at all. The engine's full reason line goes in the
/// body, where there is room for it.
pub fn render(alert: &AlertSignal) -> (String, String) {
    let price = alert.price.map(|p| format!(" ${p:.2}")).unwrap_or_default();
    let change = alert
        .change_day_pct
        .map(|c| format!(" · {c:+.1}%"))
        .unwrap_or_default();
    let title = format!("🚀 {}{}{}", alert.symbol, price, change);
    (title, alert.reason.clone())
}

/// Push an alert to the phone, at most once per `alert_id`.
///
/// Spawned and never awaited: the engine's one-second loop must not wait on a
/// network round trip. A failure is logged to stderr and dropped — a phone that
/// didn't buzz must never be the reason an alert fails to reach the screen.
pub fn notify_alert_once(
    secrets: &std::sync::Arc<std::sync::RwLock<Secrets>>,
    alert: &AlertSignal,
) {
    let Some(cfg) = NtfyConfig::from_secrets(&secrets.read().unwrap()) else {
        return;
    };
    if !claim(&alert.alert_id) {
        return;
    }
    let (title, message) = render(alert);
    let symbol = alert.symbol.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = publish(&cfg, &title, &message, vec!["rotating_light"]).await {
            eprintln!("[tagdash] ntfy: échec de l'envoi pour {symbol}: {e}");
        }
    });
}

/// Reserve an alert id. Returns false when it was already sent — the caller then
/// does nothing.
fn claim(alert_id: &str) -> bool {
    let set = SENT.get_or_init(|| Mutex::new(HashSet::new()));
    let Ok(mut guard) = set.lock() else {
        // A poisoned mutex must not silence the phone; err toward sending.
        return true;
    };
    if guard.len() >= SENT_CAP {
        guard.clear();
    }
    guard.insert(alert_id.to_string())
}

/// Forget every sent id. Called when a Market Replay restarts, so replaying the same
/// session twice can push twice rather than going silent the second time.
pub fn reset_sent() {
    if let Some(set) = SENT.get() {
        if let Ok(mut g) = set.lock() {
            g.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Session, Side};

    fn secrets(server: Option<&str>, topic: Option<&str>, token: Option<&str>) -> Secrets {
        Secrets {
            ntfy_server: server.map(str::to_string),
            ntfy_topic: topic.map(str::to_string),
            ntfy_token: token.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn the_topic_is_the_only_required_setting() {
        // A public ntfy.sh topic needs neither a server nor a token.
        let cfg = NtfyConfig::from_secrets(&secrets(None, Some("tagdash-abc"), None)).unwrap();
        assert_eq!(cfg.server, "https://ntfy.sh");
        assert_eq!(cfg.topic, "tagdash-abc");
        assert_eq!(cfg.token, None);
    }

    #[test]
    fn no_topic_means_the_feature_is_simply_not_set_up() {
        assert!(NtfyConfig::from_secrets(&secrets(None, None, Some("tk_x"))).is_none());
        assert!(NtfyConfig::from_secrets(&secrets(None, Some("   "), None)).is_none());
    }

    /// A trailing slash on the server plus the JSON publish form would post to
    /// `https://host//`, which some reverse proxies reject.
    #[test]
    fn a_custom_server_is_normalised() {
        let cfg =
            NtfyConfig::from_secrets(&secrets(Some("https://push.example.com/"), Some("t"), None))
                .unwrap();
        assert_eq!(cfg.server, "https://push.example.com");
    }

    #[test]
    fn blank_values_are_treated_as_unset_not_as_empty_strings() {
        let cfg = NtfyConfig::from_secrets(&secrets(Some("  "), Some(" t "), Some("  "))).unwrap();
        assert_eq!(cfg.server, "https://ntfy.sh");
        assert_eq!(cfg.topic, "t", "surrounding whitespace is trimmed");
        assert_eq!(cfg.token, None);
    }

    fn alert(id: &str) -> AlertSignal {
        AlertSignal {
            alert_id: id.into(),
            timestamp: chrono::Utc::now(),
            symbol: "TEST".into(),
            strategy_id: "micro_pullback".into(),
            strategy_name: "Micro Pullback".into(),
            priority: 5,
            session: Session::Premarket,
            price: Some(5.30),
            bid: None,
            ask: None,
            spread: None,
            volume: Some(500_000),
            rvol: Some(12.0),
            change_day_pct: Some(10.4),
            float_shares: Some(4_000_000),
            news_today: true,
            halted: Some(false),
            latency_ui_ms: None,
            reason: "Micro Pullback — départ 10s : +1,5% · tape 6,0 tr/s".into(),
            display_timeframe: Some("10s".into()),
            side: Some(Side::Long),
            tempo_depth: Some(vec![10, 20]),
        }
    }

    /// The lock-screen line has to answer "do I reach for the phone?" before the
    /// user unlocks anything: ticker, price, move.
    #[test]
    fn the_title_leads_with_what_decides_at_a_glance() {
        let (title, message) = render(&alert("a"));
        assert!(title.contains("TEST"), "{title}");
        assert!(title.contains("$5.30"), "{title}");
        assert!(title.contains("+10.4%"), "{title}");
        assert_eq!(
            message,
            alert("a").reason,
            "the body carries the full reason"
        );
    }

    /// The accented body is exactly why this module publishes JSON rather than
    /// putting the text in HTTP headers, which are ASCII/latin-1 only.
    #[test]
    fn the_payload_survives_french_accents() {
        let (title, message) = render(&alert("a"));
        let body = Publish {
            topic: "t",
            title: &title,
            message: &message,
            priority: PRIORITY_MAX,
            tags: vec!["rotating_light"],
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("départ"), "accents must round-trip: {json}");
        assert!(json.contains("\"priority\":5"), "maximum priority: {json}");
    }

    /// `SENT` is process-global and `reset_sent` wipes it, so these two tests would
    /// race each other under the default parallel runner — one clearing the other's
    /// claim mid-assertion. They share a lock rather than pretending to be isolated.
    static DEDUP_TESTS: Mutex<()> = Mutex::new(());

    /// Micro Pullback re-publishes the same alert as its tempo depth widens and as
    /// the A+ scores land. Without the guard one departure would buzz five times.
    #[test]
    fn an_alert_id_is_only_ever_pushed_once() {
        let _g = DEDUP_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        reset_sent();
        assert!(claim("mp-1-AAA"));
        assert!(
            !claim("mp-1-AAA"),
            "a re-publish must not buzz the phone again"
        );
        assert!(claim("mp-2-BBB"), "a different alert still gets through");
    }

    /// Replaying the same session twice must be able to push twice, or testing the
    /// pipeline in replay silently does nothing the second time.
    #[test]
    fn a_replay_restart_clears_the_dedup_set() {
        let _g = DEDUP_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        reset_sent();
        assert!(claim("mp-9-ZZZ"));
        reset_sent();
        assert!(claim("mp-9-ZZZ"));
    }
}
