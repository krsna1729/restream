//! Sends notifications over HTTP: JSON posts to webhooks, Slack and Discord,
//! and encrypted Web Push messages signed with the engine's VAPID key.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use tokio::sync::OnceCell;

use crate::application::notify::PushSubscription;
use crate::application::ports::{
    MetaStore, MetaStoreWriter, NotifyTransport, PostFuture, PostOutcome,
};

use super::web_push::{self, VapidKey};

/// Where the engine keeps its VAPID private key (PKCS#8, base64).
const VAPID_META_KEY: &str = "vapidPrivateKeyPkcs8";
/// A push message the browser has not fetched within an hour is dropped.
const PUSH_TTL_SECONDS: &str = "3600";

pub struct HttpNotifyTransport {
    client: reqwest::Client,
    meta: Arc<dyn MetaStore>,
    meta_writer: Arc<dyn MetaStoreWriter>,
    vapid: OnceCell<VapidKey>,
}

impl HttpNotifyTransport {
    pub fn new(meta: Arc<dyn MetaStore>, meta_writer: Arc<dyn MetaStoreWriter>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            // A receiver must answer where it was configured; a redirect
            // could point the engine at an internal address.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("restream/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Self {
            client,
            meta,
            meta_writer,
            vapid: OnceCell::new(),
        }
    }

    /// The stored VAPID key, made and stored on first use.
    async fn vapid(&self) -> Result<&VapidKey, String> {
        self.vapid
            .get_or_try_init(|| async {
                let stored = self
                    .meta
                    .get_meta(VAPID_META_KEY)
                    .await
                    .map_err(|e| e.to_string())?;
                let pkcs8 = match stored.and_then(|value| STANDARD.decode(value).ok()) {
                    Some(pkcs8) => pkcs8,
                    None => {
                        let pkcs8 = VapidKey::generate_pkcs8().map_err(|e| e.to_string())?;
                        self.meta_writer
                            .set_meta(VAPID_META_KEY, &STANDARD.encode(&pkcs8))
                            .await
                            .map_err(|e| e.to_string())?;
                        pkcs8
                    }
                };
                VapidKey::from_pkcs8(&pkcs8).map_err(|e| e.to_string())
            })
            .await
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> PostOutcome {
        match request.send().await {
            Ok(response) => PostOutcome {
                status: Some(response.status().as_u16()),
                error: None,
            },
            Err(error) => PostOutcome {
                status: None,
                error: Some(if error.is_timeout() {
                    "timed out after 10 s".to_string()
                } else if error.is_connect() {
                    "could not connect".to_string()
                } else {
                    error.to_string()
                }),
            },
        }
    }
}

/// Browsers hand out base64url keys; some libraries pad them.
fn decode_key(value: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(value.trim_end_matches('=')).ok()
}

fn failed(error: impl Into<String>) -> PostOutcome {
    PostOutcome {
        status: None,
        error: Some(error.into()),
    }
}

impl NotifyTransport for HttpNotifyTransport {
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        body: Vec<u8>,
        headers: Vec<(&'static str, String)>,
    ) -> PostFuture<'a> {
        Box::pin(async move {
            let mut request = self
                .client
                .post(url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
            for (name, value) in headers {
                request = request.header(name, value);
            }
            self.send(request).await
        })
    }

    fn push<'a>(
        &'a self,
        subscription: &'a PushSubscription,
        payload: Vec<u8>,
        urgent: bool,
        subject: &'a str,
    ) -> PostFuture<'a> {
        Box::pin(async move {
            let vapid = match self.vapid().await {
                Ok(vapid) => vapid,
                Err(error) => return failed(format!("VAPID key: {error}")),
            };
            let (Some(p256dh), Some(auth)) = (
                decode_key(&subscription.p256dh),
                decode_key(&subscription.auth),
            ) else {
                return failed("the subscription's keys are not base64url");
            };
            let body = match web_push::encrypt(&p256dh, &auth, &payload) {
                Ok(body) => body,
                Err(error) => return failed(error.0),
            };
            let now_s = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let authorization = match vapid.authorization(&subscription.endpoint, subject, now_s) {
                Ok(authorization) => authorization,
                Err(error) => return failed(error.0),
            };
            let request = self
                .client
                .post(&subscription.endpoint)
                .header(reqwest::header::AUTHORIZATION, authorization)
                .header(reqwest::header::CONTENT_ENCODING, "aes128gcm")
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .header("TTL", PUSH_TTL_SECONDS)
                .header("Urgency", if urgent { "high" } else { "normal" })
                .body(body);
            self.send(request).await
        })
    }

    fn push_public_key(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + '_>>
    {
        Box::pin(async move { self.vapid().await.map(VapidKey::public_key) })
    }
}
