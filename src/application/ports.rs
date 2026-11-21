//! Application ports for external effects.

use crate::application::notify::PushSubscription;
use std::future::Future;
use std::pin::Pin;

/// How one HTTP attempt ended: the status it got, or why it got none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostOutcome {
    pub status: Option<u16>,
    pub error: Option<String>,
}

impl PostOutcome {
    pub fn ok(&self) -> bool {
        self.status
            .is_some_and(|status| (200..300).contains(&status))
    }
}

pub type PostFuture<'a> = Pin<Box<dyn Future<Output = PostOutcome> + Send + 'a>>;

/// Sends notifications over the network.
pub trait NotifyTransport: Send + Sync {
    /// POSTs a JSON body with extra headers.
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        body: Vec<u8>,
        headers: Vec<(&'static str, String)>,
    ) -> PostFuture<'a>;
    /// Encrypts `payload` for the subscription and sends it to its push
    /// service, signed with the engine's VAPID key naming `subject`.
    fn push<'a>(
        &'a self,
        subscription: &'a PushSubscription,
        payload: Vec<u8>,
        urgent: bool,
        subject: &'a str,
    ) -> PostFuture<'a>;
    /// The VAPID public key browsers subscribe with (base64url).
    fn push_public_key(&self) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>>;
}
