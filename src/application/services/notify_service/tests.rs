use std::sync::Mutex;

use super::*;
use crate::application::notify::AlertBrief;
use crate::application::ports::PostFuture;
use crate::infrastructure::sqlite_ports::SqliteNotifyStore;

/// One recorded send: the URL and its extra headers.
type Sent = (String, Vec<(&'static str, String)>);

/// Answers each send with the next scripted status (then the last one) and
/// records what was sent.
struct Scripted {
    statuses: Mutex<Vec<u16>>,
    sent: Mutex<Vec<Sent>>,
}

impl Scripted {
    fn new(statuses: &[u16]) -> Arc<Self> {
        Arc::new(Self {
            statuses: Mutex::new(statuses.iter().rev().copied().collect()),
            sent: Mutex::new(Vec::new()),
        })
    }

    fn next(&self) -> PostOutcome {
        let mut statuses = self.statuses.lock().unwrap();
        let status = if statuses.len() > 1 {
            statuses.pop().unwrap()
        } else {
            statuses[0]
        };
        PostOutcome {
            status: Some(status),
            error: None,
        }
    }
}

impl NotifyTransport for Scripted {
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        _body: Vec<u8>,
        headers: Vec<(&'static str, String)>,
    ) -> PostFuture<'a> {
        self.sent.lock().unwrap().push((url.to_string(), headers));
        let outcome = self.next();
        Box::pin(async move { outcome })
    }

    fn push<'a>(
        &'a self,
        subscription: &'a PushSubscription,
        _payload: Vec<u8>,
        urgent: bool,
        _subject: &'a str,
    ) -> PostFuture<'a> {
        self.sent.lock().unwrap().push((
            subscription.endpoint.clone(),
            vec![("urgent", urgent.to_string())],
        ));
        let outcome = self.next();
        Box::pin(async move { outcome })
    }

    fn push_public_key(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + '_>>
    {
        Box::pin(async { Ok("BKEY".to_string()) })
    }
}

async fn service(transport: Arc<Scripted>) -> NotifyService {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    NotifyService::with_ports(Arc::new(SqliteNotifyStore::new(pool)), transport)
        .with_retry_delays(vec![0, 0])
}

fn draft(kind: ChannelKind, min_severity: NotifySeverity) -> ChannelDraft {
    ChannelDraft {
        kind,
        name: "ops".into(),
        target: "https://hooks.example/x".into(),
        min_severity,
        on_resolved: false,
        enabled: true,
    }
}

fn event(kind: EventKind, severity: NotifySeverity) -> NotifyEvent {
    NotifyEvent {
        kind,
        at_ms: 1,
        alert: AlertBrief {
            id: "a1".into(),
            severity,
            title: "YouTube is down".into(),
            cause: "c".into(),
            recommended_action: "r".into(),
            pipeline_id: None,
            output_id: None,
        },
    }
}

fn subscription(id: &str, endpoint: &str, min_severity: NotifySeverity) -> PushSubscription {
    PushSubscription {
        id: id.into(),
        username: "ann".into(),
        endpoint: endpoint.into(),
        p256dh: "p".into(),
        auth: "a".into(),
        min_severity,
        created_at_ms: 0,
    }
}

#[tokio::test]
async fn a_failing_receiver_is_retried_then_given_up_and_logged() {
    let transport = Scripted::new(&[503, 503, 503]);
    let notify = service(transport.clone()).await;
    notify
        .create_channel(
            "c1".into(),
            "s".into(),
            draft(ChannelKind::Webhook, NotifySeverity::Warning),
            0,
        )
        .await
        .unwrap();
    let done = notify
        .dispatch(&event(EventKind::Opened, NotifySeverity::Critical))
        .await;
    assert_eq!(done.len(), 1);
    assert_eq!(
        (done[0].ok, done[0].attempts, done[0].detail.as_str()),
        (false, 3, "HTTP 503")
    );
    assert_eq!(
        notify.deliveries(10).await.unwrap()[0].attempts,
        3,
        "the outcome is logged"
    );
}

#[tokio::test]
async fn a_refusal_that_will_not_change_is_not_retried() {
    let transport = Scripted::new(&[403]);
    let notify = service(transport.clone()).await;
    notify
        .create_channel(
            "c1".into(),
            "s".into(),
            draft(ChannelKind::Slack, NotifySeverity::Warning),
            0,
        )
        .await
        .unwrap();
    let done = notify
        .dispatch(&event(EventKind::Opened, NotifySeverity::Warning))
        .await;
    assert_eq!((done[0].ok, done[0].attempts), (false, 1));
    let recovered = Scripted::new(&[500, 200]);
    let notify = service(recovered).await;
    notify
        .create_channel(
            "c1".into(),
            "s".into(),
            draft(ChannelKind::Discord, NotifySeverity::Warning),
            0,
        )
        .await
        .unwrap();
    let done = notify
        .dispatch(&event(EventKind::Opened, NotifySeverity::Warning))
        .await;
    assert_eq!((done[0].ok, done[0].attempts), (true, 2));
}

#[tokio::test]
async fn only_webhooks_are_signed_and_only_wanted_events_go_out() {
    let transport = Scripted::new(&[200]);
    let notify = service(transport.clone()).await;
    notify
        .create_channel(
            "hook".into(),
            "whsec".into(),
            draft(ChannelKind::Webhook, NotifySeverity::Critical),
            0,
        )
        .await
        .unwrap();
    let mut slack = draft(ChannelKind::Slack, NotifySeverity::Warning);
    slack.target = "https://hooks.slack.com/services/x".into();
    notify
        .create_channel("slack".into(), "ignored".into(), slack, 0)
        .await
        .unwrap();
    let mut off = draft(ChannelKind::Discord, NotifySeverity::Warning);
    off.enabled = false;
    notify
        .create_channel("off".into(), "x".into(), off, 0)
        .await
        .unwrap();

    // A warning reaches Slack only: the webhook wants critical, Discord is off.
    let done = notify
        .dispatch(&event(EventKind::Opened, NotifySeverity::Warning))
        .await;
    assert_eq!(
        done.iter()
            .map(|d| d.target_id.as_str())
            .collect::<Vec<_>>(),
        ["slack"]
    );
    notify
        .dispatch(&event(EventKind::Opened, NotifySeverity::Critical))
        .await;
    let sent = transport.sent.lock().unwrap();
    let signed: Vec<bool> = sent
        .iter()
        .map(|(_, headers)| {
            headers
                .iter()
                .any(|(name, _)| *name == "X-Restream-Signature")
        })
        .collect();
    let urls: Vec<&str> = sent.iter().map(|(url, _)| url.as_str()).collect();
    assert_eq!(
        urls,
        [
            "https://hooks.slack.com/services/x",
            "https://hooks.example/x",
            "https://hooks.slack.com/services/x"
        ]
    );
    assert_eq!(signed, [false, true, false]);
}

#[tokio::test]
async fn a_gone_browser_is_unsubscribed_and_urgency_follows_severity() {
    let transport = Scripted::new(&[410]);
    let notify = service(transport.clone()).await;
    notify
        .subscribe(subscription(
            "s1",
            "https://push.example.com/a",
            NotifySeverity::Warning,
        ))
        .await
        .unwrap();
    let done = notify
        .dispatch(&event(EventKind::Opened, NotifySeverity::Critical))
        .await;
    assert_eq!((done[0].ok, done[0].attempts), (false, 1));
    assert!(
        notify.subscriptions_of("ann").await.unwrap().is_empty(),
        "410: forgotten"
    );
    assert_eq!(
        transport.sent.lock().unwrap()[0].1,
        [("urgent", "true".to_string())]
    );
}

#[tokio::test]
async fn subscriptions_are_checked_and_owned() {
    let notify = service(Scripted::new(&[200])).await;
    assert!(
        notify
            .subscribe(subscription(
                "s1",
                "https://127.0.0.1/x",
                NotifySeverity::Warning
            ))
            .await
            .is_err()
    );
    notify
        .subscribe(subscription(
            "s1",
            "https://push.example.com/a",
            NotifySeverity::Critical,
        ))
        .await
        .unwrap();
    // The same browser re-subscribing replaces its row.
    notify
        .subscribe(subscription(
            "s2",
            "https://push.example.com/a",
            NotifySeverity::Warning,
        ))
        .await
        .unwrap();
    let mine = notify.subscriptions_of("ann").await.unwrap();
    assert_eq!(
        mine.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["s2"]
    );
    assert!(
        matches!(
            notify.unsubscribe("bob", "s2").await,
            Err(ServiceError::NotFound(_))
        ),
        "not bob's"
    );
    notify.unsubscribe("ann", "s2").await.unwrap();
}

#[tokio::test]
async fn channels_are_validated_and_keep_their_kind() {
    let notify = service(Scripted::new(&[200])).await;
    let mut bad = draft(ChannelKind::Webhook, NotifySeverity::Warning);
    bad.target = "file:///etc/passwd".into();
    assert!(
        notify
            .create_channel("c".into(), "s".into(), bad, 0)
            .await
            .is_err()
    );
    notify
        .create_channel(
            "c".into(),
            "s".into(),
            draft(ChannelKind::Webhook, NotifySeverity::Warning),
            0,
        )
        .await
        .unwrap();
    assert!(matches!(
        notify
            .update_channel("c", draft(ChannelKind::Slack, NotifySeverity::Warning))
            .await,
        Err(ServiceError::Conflict(_))
    ));
    let mut edit = draft(ChannelKind::Webhook, NotifySeverity::Critical);
    edit.name = "pager".into();
    let edited = notify.update_channel("c", edit).await.unwrap();
    assert_eq!(
        (
            edited.name.as_str(),
            edited.min_severity,
            edited.secret.as_deref()
        ),
        ("pager", NotifySeverity::Critical, Some("s"))
    );
}
