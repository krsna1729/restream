//! Alert notifications: which alert changes are worth telling someone about,
//! who wants which, and what each kind of channel is sent. Pure; the
//! notifier task (`infrastructure::notifier`) evaluates and delivers.
//!
//! An alert must be present for [`CONFIRM_TICKS`] evaluations in a row
//! before it is announced as opened, and absent as long before it is
//! announced as resolved, so a condition that flickers for one sample does
//! not page anyone twice.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// Evaluations an alert must hold (present or absent) before it changes.
pub const CONFIRM_TICKS: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NotifySeverity {
    Warning,
    Critical,
}

impl NotifySeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "warning" => Some(Self::Warning),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// What a notification says about one alert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertBrief {
    pub id: String,
    pub severity: NotifySeverity,
    pub title: String,
    pub cause: String,
    pub recommended_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_id: Option<String>,
}

impl From<&restream_health::alerts::Alert> for AlertBrief {
    fn from(alert: &restream_health::alerts::Alert) -> Self {
        Self {
            id: alert.id.clone(),
            severity: match alert.severity {
                restream_health::alerts::Severity::Critical => NotifySeverity::Critical,
                restream_health::alerts::Severity::Warning => NotifySeverity::Warning,
            },
            title: alert.title.clone(),
            cause: alert.cause.clone(),
            recommended_action: alert.recommended_action.clone(),
            pipeline_id: alert.pipeline_id.clone(),
            output_id: alert.output_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventKind {
    Opened,
    Resolved,
    /// Sent on request, to check a channel works.
    Test,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Opened => "opened",
            Self::Resolved => "resolved",
            Self::Test => "test",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyEvent {
    pub kind: EventKind,
    pub at_ms: i64,
    pub alert: AlertBrief,
}

#[derive(Debug, Clone, Copy)]
struct Seen {
    /// Consecutive evaluations in the current presence state.
    streak: u8,
    present: bool,
    announced: bool,
}

/// Turns successive open-alert sets into confirmed opened and resolved
/// events. The first evaluation only learns what is already open: an engine
/// restart during an outage does not re-announce it.
#[derive(Debug, Default)]
pub struct AlertTransitions {
    seen: HashMap<String, (Seen, AlertBrief)>,
    seeded: bool,
}

impl AlertTransitions {
    pub fn observe(&mut self, current: &[AlertBrief], at_ms: i64) -> Vec<NotifyEvent> {
        let mut events = Vec::new();
        let ids: HashSet<&str> = current.iter().map(|alert| alert.id.as_str()).collect();
        if !self.seeded {
            self.seeded = true;
            for alert in current {
                let seen = Seen {
                    streak: CONFIRM_TICKS,
                    present: true,
                    announced: true,
                };
                self.seen.insert(alert.id.clone(), (seen, alert.clone()));
            }
            return events;
        }
        for alert in current {
            let entry = self.seen.entry(alert.id.clone()).or_insert((
                Seen {
                    streak: 0,
                    present: true,
                    announced: false,
                },
                alert.clone(),
            ));
            if !entry.0.present {
                entry.0 = Seen {
                    streak: 0,
                    present: true,
                    announced: entry.0.announced,
                };
            }
            entry.0.streak = entry.0.streak.saturating_add(1);
            entry.1 = alert.clone();
            if !entry.0.announced && entry.0.streak >= CONFIRM_TICKS {
                entry.0.announced = true;
                events.push(NotifyEvent {
                    kind: EventKind::Opened,
                    at_ms,
                    alert: alert.clone(),
                });
            }
        }
        let mut forget = Vec::new();
        for (id, (seen, brief)) in &mut self.seen {
            if ids.contains(id.as_str()) {
                continue;
            }
            if seen.present {
                *seen = Seen {
                    streak: 0,
                    present: false,
                    announced: seen.announced,
                };
            }
            seen.streak = seen.streak.saturating_add(1);
            if seen.streak >= CONFIRM_TICKS {
                if seen.announced {
                    events.push(NotifyEvent {
                        kind: EventKind::Resolved,
                        at_ms,
                        alert: brief.clone(),
                    });
                }
                forget.push(id.clone());
            }
        }
        for id in forget {
            self.seen.remove(&id);
        }
        events
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChannelKind {
    /// A signed JSON POST to any HTTPS endpoint.
    Webhook,
    Slack,
    Discord,
}

impl ChannelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Webhook => "webhook",
            Self::Slack => "slack",
            Self::Discord => "discord",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "webhook" => Some(Self::Webhook),
            "slack" => Some(Self::Slack),
            "discord" => Some(Self::Discord),
            _ => None,
        }
    }
}

/// A configured destination for alert notifications.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyChannel {
    pub id: String,
    pub kind: ChannelKind,
    pub name: String,
    /// The URL notifications are posted to. A secret: Slack and Discord
    /// webhook URLs carry their own credential.
    pub target: String,
    /// Signs webhook bodies; absent for Slack and Discord.
    #[serde(skip_serializing)]
    pub secret: Option<String>,
    pub min_severity: NotifySeverity,
    pub on_resolved: bool,
    pub enabled: bool,
    pub created_at_ms: i64,
}

/// A browser's push subscription, owned by the member who made it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PushSubscription {
    pub id: String,
    pub username: String,
    pub endpoint: String,
    #[serde(skip_serializing)]
    pub p256dh: String,
    #[serde(skip_serializing)]
    pub auth: String,
    pub min_severity: NotifySeverity,
    pub created_at_ms: i64,
}

/// One delivery's final outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    pub id: i64,
    pub at_ms: i64,
    /// A channel id, or `push:<subscription id>`.
    pub target_id: String,
    pub target_name: String,
    pub event: EventKind,
    pub alert_id: String,
    pub title: String,
    pub ok: bool,
    pub attempts: u8,
    /// The HTTP status or transport error of the last attempt.
    pub detail: String,
}

/// Whether a recipient with `min_severity` (and, for channels, the
/// resolved switch) wants this event. A test always goes through.
pub fn wants(min_severity: NotifySeverity, on_resolved: bool, event: &NotifyEvent) -> bool {
    match event.kind {
        EventKind::Test => true,
        EventKind::Resolved => on_resolved && event.alert.severity >= min_severity,
        EventKind::Opened => event.alert.severity >= min_severity,
    }
}

/// Waits before each retry of a failed delivery: three tries over about
/// 20 s, then give up. Bounded on purpose: a down receiver must not turn
/// the notifier into a retry storm of its own.
pub const RETRY_DELAYS_MS: [u64; 2] = [2_000, 15_000];

/// A wait with up to a quarter of jitter added, so channels that failed
/// together do not retry together. `unit` is uniform in [0, 1).
pub fn jittered(base_ms: u64, unit: f64) -> u64 {
    base_ms + (base_ms as f64 * 0.25 * unit.clamp(0.0, 1.0)) as u64
}

/// The wait before retry `attempt` (0-based), or `None`: give up.
pub fn retry_delay_ms(attempt: usize, unit: f64) -> Option<u64> {
    RETRY_DELAYS_MS
        .get(attempt)
        .map(|base| jittered(*base, unit))
}

/// The one-line headline every channel leads with.
pub fn headline(event: &NotifyEvent) -> String {
    let severity = match event.alert.severity {
        NotifySeverity::Critical => "CRITICAL",
        NotifySeverity::Warning => "WARNING",
    };
    match event.kind {
        EventKind::Opened => format!("[{severity}] {}", event.alert.title),
        EventKind::Resolved => format!("[RESOLVED] {}", event.alert.title),
        EventKind::Test => format!("[TEST] {}", event.alert.title),
    }
}

/// A link into the dashboard's incident view, when the engine knows its
/// public URL.
pub fn incident_link(dashboard_url: Option<&str>, event: &NotifyEvent) -> Option<String> {
    let base = dashboard_url?.trim_end_matches('/');
    let mut link = format!("{base}/?w=incidents");
    if let Some(pipeline) = &event.alert.pipeline_id {
        link.push_str("&p=");
        link.push_str(&urlencoding_component(pipeline));
    }
    Some(link)
}

fn urlencoding_component(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// The JSON body a channel of `kind` is sent for `event`.
pub fn render_body(
    kind: ChannelKind,
    event: &NotifyEvent,
    dashboard_url: Option<&str>,
) -> serde_json::Value {
    let link = incident_link(dashboard_url, event);
    let detail = if event.kind == EventKind::Resolved {
        "The condition has cleared.".to_string()
    } else {
        format!(
            "{} Next: {}",
            event.alert.cause, event.alert.recommended_action
        )
    };
    match kind {
        ChannelKind::Webhook => serde_json::json!({
            "schema": "restream.notify.v1",
            "event": event.kind,
            "atMs": event.at_ms,
            "alert": event.alert,
            "link": link,
        }),
        ChannelKind::Slack => {
            let mut text = format!("*{}*\n{}", headline(event), detail);
            if let Some(link) = &link {
                text.push_str(&format!("\n<{link}|Open in restream>"));
            }
            serde_json::json!({ "text": text })
        }
        ChannelKind::Discord => {
            let color = match (event.kind, event.alert.severity) {
                (EventKind::Resolved, _) => 0x2e_a0_43,
                (_, NotifySeverity::Critical) => 0xd1_24_2f,
                (_, NotifySeverity::Warning) => 0xd2_99_22,
            };
            serde_json::json!({
                "content": headline(event),
                "embeds": [{
                    "title": event.alert.title,
                    "description": detail,
                    "color": color,
                    "url": link,
                }],
            })
        }
    }
}

/// What a browser push shows: the service worker (`public/sw.js`) renders
/// it as a system notification and opens `url` on click.
pub fn push_payload(event: &NotifyEvent) -> serde_json::Value {
    serde_json::json!({
        "title": headline(event),
        "body": if event.kind == EventKind::Resolved {
            "The condition has cleared.".to_string()
        } else {
            event.alert.cause.clone()
        },
        "tag": event.alert.id,
        "severity": event.alert.severity,
        "url": match &event.alert.pipeline_id {
            Some(pipeline) => format!("/?w=incidents&p={}", urlencoding_component(pipeline)),
            None => "/?w=incidents".to_string(),
        },
    })
}

/// The `X-Restream-Signature` header of a webhook body: `t=<unix
/// seconds>,v1=<hex HMAC-SHA256 of "<t>.<body>">` keyed by the channel
/// secret. A receiver recomputes it and rejects old timestamps to stop
/// replays.
pub fn webhook_signature(secret: &str, timestamp_s: u64, body: &[u8]) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    let mut context = ring::hmac::Context::with_key(&key);
    context.update(timestamp_s.to_string().as_bytes());
    context.update(b".");
    context.update(body);
    let tag = context.sign();
    let hex: String = tag
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("t={timestamp_s},v1={hex}")
}

/// A target URL a channel may post to: http(s) with a host.
pub fn valid_channel_target(target: &str) -> bool {
    let Some((scheme, rest)) = target.split_once("://") else {
        return false;
    };
    matches!(scheme, "http" | "https")
        && rest
            .split(['/', '?', '#'])
            .next()
            .is_some_and(|host| !host.is_empty())
}

/// A browser push endpoint: HTTPS on a named host. Any member may register
/// one, so the engine never posts to an address literal or a local name
/// on their behalf.
pub fn valid_push_endpoint(endpoint: &str) -> bool {
    let Some(rest) = endpoint.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host.split(':').next().unwrap_or("");
    !host.is_empty()
        && !host.starts_with('[')
        && host.contains('.')
        && host.parse::<std::net::Ipv4Addr>().is_err()
        && !host.eq_ignore_ascii_case("localhost")
        && !host.to_ascii_lowercase().ends_with(".localhost")
        && !host.to_ascii_lowercase().ends_with(".local")
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod tests;
