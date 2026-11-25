//! Notification channel, delivery, and push operations on the API state.

use super::AppState;
use crate::application::notify::{Delivery, NotifyChannel, NotifyEvent, PushSubscription};
use crate::application::services::notify_service::ChannelDraft;

impl AppState {
    /// Lists configured notification channels.
    pub(crate) async fn notification_channels(
        &self,
    ) -> crate::application::services::ServiceResult<Vec<NotifyChannel>> {
        self.notify_service.channels().await
    }

    /// Creates one notification channel and returns it.
    pub(crate) async fn create_notification_channel(
        &self,
        id: String,
        secret: String,
        draft: ChannelDraft,
        now_ms: i64,
    ) -> crate::application::services::ServiceResult<NotifyChannel> {
        self.notify_service
            .create_channel(id, secret, draft, now_ms)
            .await
    }

    /// Updates one notification channel.
    pub(crate) async fn update_notification_channel(
        &self,
        id: &str,
        draft: ChannelDraft,
    ) -> crate::application::services::ServiceResult<NotifyChannel> {
        self.notify_service.update_channel(id, draft).await
    }

    /// Deletes one notification channel.
    pub(crate) async fn delete_notification_channel(
        &self,
        id: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.notify_service.delete_channel(id).await
    }

    /// Sends a test event to one notification channel.
    pub(crate) async fn test_notification_channel(
        &self,
        id: &str,
        event: &NotifyEvent,
    ) -> crate::application::services::ServiceResult<Delivery> {
        self.notify_service.test_channel(id, event).await
    }

    /// Returns one webhook channel's signing secret.
    pub(crate) async fn notification_channel_secret(
        &self,
        id: &str,
    ) -> crate::application::services::ServiceResult<Option<String>> {
        self.notify_service.channel_secret(id).await
    }

    /// Lists recent notification deliveries.
    pub(crate) async fn notification_deliveries(
        &self,
        limit: u32,
    ) -> crate::application::services::ServiceResult<Vec<Delivery>> {
        self.notify_service.deliveries(limit).await
    }

    /// Returns the VAPID public key browsers subscribe with.
    pub(crate) async fn notification_push_public_key(
        &self,
    ) -> crate::application::services::ServiceResult<String> {
        self.notify_service.push_public_key().await
    }

    /// Lists one member's push subscriptions.
    pub(crate) async fn notification_subscriptions(
        &self,
        username: &str,
    ) -> crate::application::services::ServiceResult<Vec<PushSubscription>> {
        self.notify_service.subscriptions_of(username).await
    }

    /// Stores one push subscription.
    pub(crate) async fn subscribe_notifications(
        &self,
        subscription: PushSubscription,
    ) -> crate::application::services::ServiceResult<PushSubscription> {
        self.notify_service.subscribe(subscription).await
    }

    /// Deletes one of a member's push subscriptions.
    pub(crate) async fn unsubscribe_notifications(
        &self,
        username: &str,
        id: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.notify_service.unsubscribe(username, id).await
    }

    /// Sends a test push to one member's subscriptions.
    pub(crate) async fn test_notification_push(
        &self,
        username: &str,
        event: &NotifyEvent,
    ) -> crate::application::services::ServiceResult<Vec<Delivery>> {
        self.notify_service.test_push(username, event).await
    }

    /// Dispatches one alert event without blocking the alert loop.
    pub(crate) fn dispatch_notification_event(&self, event: NotifyEvent) {
        let notify = self.notify_service.clone();
        tokio::spawn(async move {
            notify.dispatch(&event).await;
        });
    }
}
