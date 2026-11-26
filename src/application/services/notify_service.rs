//! Who is told about alerts and how: notification channels (webhook, Slack,
//! Discord), members' browser push subscriptions, and the delivery of each
//! event to everyone who wants it, with a bounded retry and a log of how
//! each delivery ended.

use std::sync::{Arc, OnceLock};

use futures_util::future::join_all;

use crate::application::notify::{
    ChannelKind, Delivery, EventKind, NotifyChannel, NotifyEvent, NotifySeverity, PushSubscription,
    RETRY_DELAYS_MS, jittered, push_payload, render_body, valid_channel_target,
    valid_push_endpoint, wants, webhook_signature,
};
use crate::application::ports::{NotifyTransport, PostOutcome};
use crate::db::{DeadlineDb, notify_repo};

use crate::application::service_error::{ServiceError, ServiceResult};

fn store_error<E: std::fmt::Display>(context: &str) -> impl Fn(E) -> ServiceError + '_ {
    move |err| ServiceError::internal(format!("{context}: {err}"))
}

/// What deliveries say about the installation.
#[derive(Debug, Clone, Default)]
pub struct NotifyContext {
    /// The dashboard's public URL, for links in messages.
    pub public_url: Option<String>,
    /// The VAPID `sub` claim: a `mailto:` or `https:` contact.
    pub push_subject: String,
}

#[derive(Clone)]
pub struct NotifyService {
    db: DeadlineDb,
    transport: Arc<dyn NotifyTransport>,
    context: Arc<OnceLock<NotifyContext>>,
    retry_delays_ms: Vec<u64>,
}

/// A channel as created or edited; the service keeps the rest.
#[derive(Debug, Clone)]
pub struct ChannelDraft {
    pub kind: ChannelKind,
    pub name: String,
    pub target: String,
    pub min_severity: NotifySeverity,
    pub on_resolved: bool,
    pub enabled: bool,
}

impl NotifyService {
    pub fn new(db: DeadlineDb, transport: Arc<dyn NotifyTransport>) -> Self {
        Self {
            db,
            transport,
            context: Arc::new(OnceLock::new()),
            retry_delays_ms: RETRY_DELAYS_MS.to_vec(),
        }
    }

    /// Sets the installation context once, at startup.
    pub fn configure(&self, context: NotifyContext) {
        let _ = self.context.set(context);
    }

    /// Tests retry without waiting the production delays.
    pub fn with_retry_delays(mut self, delays_ms: Vec<u64>) -> Self {
        self.retry_delays_ms = delays_ms;
        self
    }

    fn context(&self) -> NotifyContext {
        self.context
            .get()
            .cloned()
            .unwrap_or_else(|| NotifyContext {
                public_url: None,
                push_subject: "mailto:restream@localhost".to_string(),
            })
    }

    pub async fn channels(&self) -> ServiceResult<Vec<NotifyChannel>> {
        let rows = self
            .db
            .read(notify_repo::list_channels)
            .await
            .map_err(store_error("list channels"))?;
        Ok(rows.into_iter().filter_map(channel_from_row).collect())
    }

    async fn save_channel(&self, channel: &NotifyChannel) -> ServiceResult<()> {
        let row = (
            channel.id.clone(),
            channel.kind.as_str().to_string(),
            channel.name.clone(),
            channel.target.clone(),
            channel.secret.clone(),
            channel.min_severity.as_str().to_string(),
            channel.on_resolved,
            channel.enabled,
            channel.created_at_ms,
        );
        self.db
            .write(move |pool| async move { notify_repo::save_channel(&pool, &row).await })
            .await
            .map_err(store_error("save channel"))
    }

    async fn list_subscriptions(&self) -> ServiceResult<Vec<PushSubscription>> {
        let rows = self
            .db
            .read(notify_repo::list_subscriptions)
            .await
            .map_err(store_error("list subscriptions"))?;
        Ok(rows.into_iter().map(subscription_from_row).collect())
    }

    /// `false` when no such subscription exists.
    async fn delete_subscription(&self, id: &str) -> ServiceResult<bool> {
        let id = id.to_owned();
        self.db
            .write(move |pool| async move { notify_repo::delete_subscription(&pool, &id).await })
            .await
            .map_err(store_error("delete subscription"))
    }

    fn check(draft: &ChannelDraft) -> ServiceResult<()> {
        if draft.name.trim().is_empty() || draft.name.len() > 80 {
            return Err(ServiceError::conflict(
                "A channel needs a name of 1-80 characters.",
            ));
        }
        if !valid_channel_target(&draft.target) || draft.target.len() > 2048 {
            return Err(ServiceError::conflict("The target must be an http(s) URL."));
        }
        Ok(())
    }

    /// Adds a channel; a webhook gets `secret` to sign its bodies.
    pub async fn create_channel(
        &self,
        id: String,
        secret: String,
        draft: ChannelDraft,
        now_ms: i64,
    ) -> ServiceResult<NotifyChannel> {
        Self::check(&draft)?;
        let channel = NotifyChannel {
            id,
            kind: draft.kind,
            name: draft.name.trim().to_string(),
            target: draft.target,
            secret: (draft.kind == ChannelKind::Webhook).then_some(secret),
            min_severity: draft.min_severity,
            on_resolved: draft.on_resolved,
            enabled: draft.enabled,
            created_at_ms: now_ms,
        };
        self.save_channel(&channel).await?;
        Ok(channel)
    }

    async fn channel(&self, id: &str) -> ServiceResult<NotifyChannel> {
        self.channels()
            .await?
            .into_iter()
            .find(|channel| channel.id == id)
            .ok_or_else(|| ServiceError::not_found(format!("no channel {id}")))
    }

    pub async fn update_channel(
        &self,
        id: &str,
        draft: ChannelDraft,
    ) -> ServiceResult<NotifyChannel> {
        Self::check(&draft)?;
        let current = self.channel(id).await?;
        if draft.kind != current.kind {
            return Err(ServiceError::conflict(
                "A channel's kind cannot change; add a new one.",
            ));
        }
        let channel = NotifyChannel {
            name: draft.name.trim().to_string(),
            target: draft.target,
            min_severity: draft.min_severity,
            on_resolved: draft.on_resolved,
            enabled: draft.enabled,
            ..current
        };
        self.save_channel(&channel).await?;
        Ok(channel)
    }

    pub async fn delete_channel(&self, id: &str) -> ServiceResult<()> {
        let owned_id = id.to_owned();
        if !self
            .db
            .write(move |pool| async move { notify_repo::delete_channel(&pool, &owned_id).await })
            .await
            .map_err(store_error("delete channel"))?
        {
            return Err(ServiceError::not_found(format!("no channel {id}")));
        }
        Ok(())
    }

    /// The webhook secret, for an admin to configure the receiver.
    pub async fn channel_secret(&self, id: &str) -> ServiceResult<Option<String>> {
        Ok(self.channel(id).await?.secret)
    }

    pub async fn push_public_key(&self) -> ServiceResult<String> {
        self.transport
            .push_public_key()
            .await
            .map_err(|err| ServiceError::internal(format!("VAPID key: {err}")))
    }

    pub async fn subscriptions_of(&self, username: &str) -> ServiceResult<Vec<PushSubscription>> {
        Ok(self
            .list_subscriptions()
            .await?
            .into_iter()
            .filter(|subscription| subscription.username == username)
            .collect())
    }

    pub async fn subscribe(
        &self,
        subscription: PushSubscription,
    ) -> ServiceResult<PushSubscription> {
        if !valid_push_endpoint(&subscription.endpoint) || subscription.endpoint.len() > 2048 {
            return Err(ServiceError::conflict(
                "A push endpoint must be an https URL on a public host name.",
            ));
        }
        if subscription.p256dh.len() > 200 || subscription.auth.len() > 100 {
            return Err(ServiceError::conflict(
                "The subscription keys are too long.",
            ));
        }
        let row = (
            subscription.id.clone(),
            subscription.username.clone(),
            subscription.endpoint.clone(),
            subscription.p256dh.clone(),
            subscription.auth.clone(),
            subscription.min_severity.as_str().to_string(),
            subscription.created_at_ms,
        );
        self.db
            .write(move |pool| async move { notify_repo::save_subscription(&pool, &row).await })
            .await
            .map_err(store_error("save subscription"))?;
        Ok(subscription)
    }

    /// Removes a member's own subscription.
    pub async fn unsubscribe(&self, username: &str, id: &str) -> ServiceResult<()> {
        if !self
            .subscriptions_of(username)
            .await?
            .iter()
            .any(|subscription| subscription.id == id)
        {
            return Err(ServiceError::not_found(format!("no subscription {id}")));
        }
        self.delete_subscription(id).await?;
        Ok(())
    }

    pub async fn deliveries(&self, limit: u32) -> ServiceResult<Vec<Delivery>> {
        let limit = i64::from(limit.clamp(1, 500));
        let rows = self
            .db
            .read(|pool| notify_repo::list_deliveries(pool, limit))
            .await
            .map_err(store_error("list deliveries"))?;
        Ok(rows.into_iter().map(delivery_from_row).collect())
    }

    /// Tells everyone who wants `event`, concurrently, and logs each outcome.
    pub async fn dispatch(&self, event: &NotifyEvent) -> Vec<Delivery> {
        let channels = self.channels().await.unwrap_or_default();
        let subscriptions = self.list_subscriptions().await.unwrap_or_default();
        let channel_sends = channels
            .iter()
            .filter(|channel| {
                channel.enabled && wants(channel.min_severity, channel.on_resolved, event)
            })
            .map(|channel| self.deliver_channel(channel, event));
        let push_sends = subscriptions
            .iter()
            .filter(|subscription| wants(subscription.min_severity, true, event))
            .map(|subscription| self.deliver_push(subscription, event));
        let (mut done, pushed) =
            futures_util::future::join(join_all(channel_sends), join_all(push_sends)).await;
        done.extend(pushed);
        done
    }

    /// Sends a test event to one channel now.
    pub async fn test_channel(&self, id: &str, event: &NotifyEvent) -> ServiceResult<Delivery> {
        let channel = self.channel(id).await?;
        Ok(self.deliver_channel(&channel, event).await)
    }

    /// Sends a test event to every subscription of one member.
    pub async fn test_push(
        &self,
        username: &str,
        event: &NotifyEvent,
    ) -> ServiceResult<Vec<Delivery>> {
        let subscriptions = self.subscriptions_of(username).await?;
        Ok(join_all(
            subscriptions
                .iter()
                .map(|subscription| self.deliver_push(subscription, event)),
        )
        .await)
    }

    /// Tries until a 2xx, a refusal that will not change (a 4xx other than
    /// 408 and 429), or the retries run out.
    async fn attempt<F, Fut>(&self, mut send: F) -> (PostOutcome, u8)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = PostOutcome>,
    {
        let mut attempts = 0u8;
        loop {
            let outcome = send().await;
            attempts += 1;
            let permanent = outcome.status.is_some_and(|status| {
                (400..500).contains(&status) && status != 408 && status != 429
            });
            if outcome.ok() || permanent {
                return (outcome, attempts);
            }
            let index = usize::from(attempts - 1);
            let Some(delay) = self
                .retry_delays_ms
                .get(index)
                .map(|base| jittered(*base, rand::random::<f64>()))
            else {
                return (outcome, attempts);
            };
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
    }

    async fn deliver_channel(&self, channel: &NotifyChannel, event: &NotifyEvent) -> Delivery {
        let context = self.context();
        let body = render_body(channel.kind, event, context.public_url.as_deref())
            .to_string()
            .into_bytes();
        let (outcome, attempts) = self
            .attempt(|| {
                let mut headers = vec![("X-Restream-Event", event.kind.as_str().to_string())];
                if let Some(secret) = &channel.secret {
                    let now_s = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    headers.push((
                        "X-Restream-Signature",
                        webhook_signature(secret, now_s, &body),
                    ));
                }
                self.transport
                    .post_json(&channel.target, body.clone(), headers)
            })
            .await;
        self.log(
            event,
            channel.id.clone(),
            channel.name.clone(),
            outcome,
            attempts,
        )
        .await
    }

    async fn deliver_push(&self, subscription: &PushSubscription, event: &NotifyEvent) -> Delivery {
        let context = self.context();
        let payload = push_payload(event).to_string().into_bytes();
        let urgent =
            event.alert.severity == NotifySeverity::Critical && event.kind == EventKind::Opened;
        let (outcome, attempts) = self
            .attempt(|| {
                self.transport
                    .push(subscription, payload.clone(), urgent, &context.push_subject)
            })
            .await;
        // 404 and 410: the browser unsubscribed; stop sending to it.
        if matches!(outcome.status, Some(404 | 410)) {
            let _ = self.delete_subscription(&subscription.id).await;
        }
        self.log(
            event,
            format!("push:{}", subscription.id),
            format!("{}'s browser", subscription.username),
            outcome,
            attempts,
        )
        .await
    }

    async fn log(
        &self,
        event: &NotifyEvent,
        target_id: String,
        target_name: String,
        outcome: PostOutcome,
        attempts: u8,
    ) -> Delivery {
        let delivery = Delivery {
            id: 0,
            at_ms: jiff::Timestamp::now().as_millisecond(),
            target_id,
            target_name,
            event: event.kind,
            alert_id: event.alert.id.clone(),
            title: event.alert.title.clone(),
            ok: outcome.ok(),
            attempts,
            detail: match (outcome.status, outcome.error) {
                (Some(status), _) => format!("HTTP {status}"),
                (None, Some(error)) => error,
                (None, None) => "no answer".to_string(),
            },
        };
        let row = (
            delivery.at_ms,
            delivery.target_id.clone(),
            delivery.target_name.clone(),
            delivery.event.as_str().to_string(),
            delivery.alert_id.clone(),
            delivery.title.clone(),
            delivery.ok,
            i64::from(delivery.attempts),
            delivery.detail.clone(),
        );
        let logged = self
            .db
            .write(move |pool| async move { notify_repo::append_delivery(&pool, &row).await })
            .await;
        if let Err(error) = logged {
            tracing::warn!(err = %error, "failed to log notification delivery");
        }
        delivery
    }
}

/// A stored severity this build does not know reads as a warning.
fn severity(value: &str) -> NotifySeverity {
    NotifySeverity::parse(value).unwrap_or(NotifySeverity::Warning)
}

/// A channel of a kind this build does not know is skipped.
fn channel_from_row(row: notify_repo::ChannelRow) -> Option<NotifyChannel> {
    let (id, kind, name, target, secret, min_severity, on_resolved, enabled, created_at_ms) = row;
    Some(NotifyChannel {
        kind: ChannelKind::parse(&kind)?,
        id,
        name,
        target,
        secret,
        min_severity: severity(&min_severity),
        on_resolved,
        enabled,
        created_at_ms,
    })
}

fn subscription_from_row(row: notify_repo::SubscriptionRow) -> PushSubscription {
    let (id, username, endpoint, p256dh, auth, min_severity, created_at_ms) = row;
    PushSubscription {
        id,
        username,
        endpoint,
        p256dh,
        auth,
        min_severity: severity(&min_severity),
        created_at_ms,
    }
}

fn delivery_from_row((id, row): (i64, notify_repo::DeliveryRow)) -> Delivery {
    let (at_ms, target_id, target_name, event, alert_id, title, ok, attempts, detail) = row;
    Delivery {
        id,
        at_ms,
        target_id,
        target_name,
        event: match event.as_str() {
            "opened" => EventKind::Opened,
            "resolved" => EventKind::Resolved,
            _ => EventKind::Test,
        },
        alert_id,
        title,
        ok,
        attempts: u8::try_from(attempts).unwrap_or(u8::MAX),
        detail,
    }
}

#[cfg(test)]
mod tests;
