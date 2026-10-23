//! SQLite adapter for notification channels, push subscriptions and deliveries.

use crate::application::ports::*;
use sqlx::SqlitePool;

#[derive(Clone)]
pub struct SqliteNotifyStore {
    pool: SqlitePool,
}

impl SqliteNotifyStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn notify_error(err: sqlx::Error) -> NotifyStoreError {
    NotifyStoreError::new(err.to_string())
}

fn severity(value: &str) -> crate::application::notify::NotifySeverity {
    crate::application::notify::NotifySeverity::parse(value)
        .unwrap_or(crate::application::notify::NotifySeverity::Warning)
}

impl NotifyStore for SqliteNotifyStore {
    fn list_channels(&self) -> NotifyFuture<'_, Vec<crate::application::notify::NotifyChannel>> {
        Box::pin(async move {
            let rows = crate::db::notify_repo::list_channels(&self.pool)
                .await
                .map_err(notify_error)?;
            Ok(rows
                .into_iter()
                .filter_map(
                    |(
                        id,
                        kind,
                        name,
                        target,
                        secret,
                        min_severity,
                        on_resolved,
                        enabled,
                        created_at_ms,
                    )| {
                        Some(crate::application::notify::NotifyChannel {
                            kind: crate::application::notify::ChannelKind::parse(&kind)?,
                            id,
                            name,
                            target,
                            secret,
                            min_severity: severity(&min_severity),
                            on_resolved,
                            enabled,
                            created_at_ms,
                        })
                    },
                )
                .collect())
        })
    }

    fn save_channel<'a>(
        &'a self,
        channel: &'a crate::application::notify::NotifyChannel,
    ) -> NotifyFuture<'a, ()> {
        Box::pin(async move {
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
            crate::db::notify_repo::save_channel(&self.pool, &row)
                .await
                .map_err(notify_error)
        })
    }

    fn delete_channel<'a>(&'a self, id: &'a str) -> NotifyFuture<'a, bool> {
        Box::pin(async move {
            crate::db::notify_repo::delete_channel(&self.pool, id)
                .await
                .map_err(notify_error)
        })
    }

    fn list_subscriptions(
        &self,
    ) -> NotifyFuture<'_, Vec<crate::application::notify::PushSubscription>> {
        Box::pin(async move {
            let rows = crate::db::notify_repo::list_subscriptions(&self.pool)
                .await
                .map_err(notify_error)?;
            Ok(rows
                .into_iter()
                .map(
                    |(id, username, endpoint, p256dh, auth, min_severity, created_at_ms)| {
                        crate::application::notify::PushSubscription {
                            id,
                            username,
                            endpoint,
                            p256dh,
                            auth,
                            min_severity: severity(&min_severity),
                            created_at_ms,
                        }
                    },
                )
                .collect())
        })
    }

    fn save_subscription<'a>(
        &'a self,
        subscription: &'a crate::application::notify::PushSubscription,
    ) -> NotifyFuture<'a, ()> {
        Box::pin(async move {
            let row = (
                subscription.id.clone(),
                subscription.username.clone(),
                subscription.endpoint.clone(),
                subscription.p256dh.clone(),
                subscription.auth.clone(),
                subscription.min_severity.as_str().to_string(),
                subscription.created_at_ms,
            );
            crate::db::notify_repo::save_subscription(&self.pool, &row)
                .await
                .map_err(notify_error)
        })
    }

    fn delete_subscription<'a>(&'a self, id: &'a str) -> NotifyFuture<'a, bool> {
        Box::pin(async move {
            crate::db::notify_repo::delete_subscription(&self.pool, id)
                .await
                .map_err(notify_error)
        })
    }

    fn append_delivery<'a>(
        &'a self,
        delivery: &'a crate::application::notify::Delivery,
    ) -> NotifyFuture<'a, ()> {
        Box::pin(async move {
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
            crate::db::notify_repo::append_delivery(&self.pool, &row)
                .await
                .map_err(notify_error)
        })
    }

    fn list_deliveries(
        &self,
        limit: u32,
    ) -> NotifyFuture<'_, Vec<crate::application::notify::Delivery>> {
        Box::pin(async move {
            let rows = crate::db::notify_repo::list_deliveries(&self.pool, i64::from(limit))
                .await
                .map_err(notify_error)?;
            Ok(rows
                .into_iter()
                .map(
                    |(
                        id,
                        (
                            at_ms,
                            target_id,
                            target_name,
                            event,
                            alert_id,
                            title,
                            ok,
                            attempts,
                            detail,
                        ),
                    )| {
                        crate::application::notify::Delivery {
                            id,
                            at_ms,
                            target_id,
                            target_name,
                            event: match event.as_str() {
                                "opened" => crate::application::notify::EventKind::Opened,
                                "resolved" => crate::application::notify::EventKind::Resolved,
                                _ => crate::application::notify::EventKind::Test,
                            },
                            alert_id,
                            title,
                            ok,
                            attempts: u8::try_from(attempts).unwrap_or(u8::MAX),
                            detail,
                        }
                    },
                )
                .collect())
        })
    }
}
