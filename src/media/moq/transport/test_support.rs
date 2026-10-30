//! Loopback QUIC fixtures over real compio-quic endpoints, for the transport
//! tests and the MoQ ingest/egress/origin tests. With the `test-support`
//! feature they also build for dependents' tests, which nothing in this
//! crate's own non-test build calls; hence the per-item `dead_code` allows.

use std::net::SocketAddr;
use std::sync::LazyLock;
use std::time::Duration;

use compio::quic::{ClientConfig, Endpoint};

use super::{
    Accepted, MOQ_ALPNS, MoqSession, SERVER_ALPNS, TransportError, accept, client_endpoint,
    connect, server_endpoint,
};
use restream_moq::tls;
use restream_srt::address_family::AddressFamily;

/// The server name the loopback certificate is issued for.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const LOOPBACK_SERVER_NAME: &str = "localhost";

/// A self-signed `localhost` certificate every loopback fixture shares:
/// (certificate PEM, key PEM).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) static LOOPBACK_CERT: LazyLock<(Vec<u8>, Vec<u8>)> = LazyLock::new(|| {
    let certified = rcgen::generate_simple_self_signed(vec![LOOPBACK_SERVER_NAME.to_owned()])
        .expect("self-signed certificate");
    (
        certified.cert.pem().into_bytes(),
        certified.signing_key.serialize_pem().into_bytes(),
    )
});

/// A listener on an ephemeral loopback port offering `alpns`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn loopback_server(alpns: &[&str]) -> (Endpoint, SocketAddr) {
    let (cert, key) = &*LOOPBACK_CERT;
    let config = tls::server_config(alpns, cert, key).expect("loopback server TLS");
    let endpoint = server_endpoint(SocketAddr::from(([127, 0, 0, 1], 0)), config)
        .await
        .expect("loopback server endpoint");
    let addr = endpoint.local_addr().expect("loopback server address");
    (endpoint, addr)
}

/// A client TLS configuration offering `alpns` and trusting [`LOOPBACK_CERT`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn loopback_client_tls(alpns: &[&str]) -> ClientConfig {
    tls::client_config(alpns, Some(&LOOPBACK_CERT.0), false).expect("loopback client TLS")
}

/// Runs `test` on a fresh Compio runtime, failing it after 30 s.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn run<F: Future>(test: F) -> F::Output {
    compio::runtime::Runtime::new()
        .expect("compio runtime")
        .block_on(async move {
            compio::time::timeout(Duration::from_secs(30), test)
                .await
                .expect("test timed out")
        })
}

/// A listener offering [`SERVER_ALPNS`] and a client endpoint, both on
/// loopback. Keep it alive as long as the sessions it made.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct Loopback {
    pub(crate) server: Endpoint,
    pub(crate) client: Endpoint,
    pub(crate) addr: SocketAddr,
}

#[cfg_attr(not(test), allow(dead_code))]
impl Loopback {
    pub(crate) async fn new() -> Self {
        let (server, addr) = loopback_server(&SERVER_ALPNS).await;
        let client = client_endpoint(AddressFamily::V4)
            .await
            .expect("loopback client endpoint");
        Self {
            server,
            client,
            addr,
        }
    }

    /// The next connection the listener accepts, through [`accept`].
    pub(crate) async fn accept(&self) -> Result<Accepted, TransportError> {
        accept(self.server.wait_incoming().await.expect("listener open")).await
    }

    /// A native QUIC session pair: (client, accepted server side).
    pub(crate) async fn native(&self) -> (MoqSession, Accepted) {
        self.native_offering(MOQ_ALPNS).await
    }

    /// [`Self::native`] with the client offering only `alpns`.
    pub(crate) async fn native_offering(&self, alpns: &[&str]) -> (MoqSession, Accepted) {
        let dial = connect(
            &self.client,
            self.addr,
            LOOPBACK_SERVER_NAME,
            loopback_client_tls(alpns),
        );
        let (client, server) = futures_util::future::join(dial, self.accept()).await;
        (
            client.expect("native connect"),
            server.expect("native accept"),
        )
    }

    /// A WebTransport session pair: the client CONNECTs `url` offering
    /// `protocols` as subprotocols.
    pub(crate) async fn webtransport(
        &self,
        url: &str,
        protocols: &[&str],
    ) -> (
        Result<MoqSession, TransportError>,
        Result<Accepted, TransportError>,
    ) {
        let url = url::Url::parse(url).expect("CONNECT URL");
        let dial = async {
            let conn = self
                .client
                .connect(
                    self.addr,
                    LOOPBACK_SERVER_NAME,
                    Some(loopback_client_tls(&["h3"])),
                )
                .map_err(|error| TransportError::Handshake(error.to_string()))?
                .await
                .map_err(TransportError::Connection)?;
            super::webtransport::connect(conn, url, protocols).await
        };
        futures_util::future::join(dial, self.accept()).await
    }
}
