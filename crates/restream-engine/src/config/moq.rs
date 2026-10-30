//! MoQ (Media over QUIC) ingest listener settings, `RESTREAM_MOQ_*`.
//!
//! The listener is off unless `RESTREAM_MOQ_INGEST_LISTEN` names an address;
//! then it needs the PEM certificate chain and private key its QUIC endpoint
//! presents. A half-configured listener is reported once at startup and stays
//! off: it never takes the engine down.

use std::net::SocketAddr;

use super::env_var;

pub const MOQ_INGEST_LISTEN_ENV: &str = "RESTREAM_MOQ_INGEST_LISTEN";
pub const MOQ_TLS_CERT_ENV: &str = "RESTREAM_MOQ_TLS_CERT";
pub const MOQ_TLS_KEY_ENV: &str = "RESTREAM_MOQ_TLS_KEY";
pub const MOQ_MAX_CONNECTIONS_ENV: &str = "RESTREAM_MOQ_MAX_CONNECTIONS";
pub const MOQ_ORIGIN_MAX_VIEWERS_ENV: &str = "RESTREAM_MOQ_ORIGIN_MAX_VIEWERS";

/// Concurrent MoQ handshakes and publishers when
/// `RESTREAM_MOQ_MAX_CONNECTIONS` is unset or unparseable.
pub const MOQ_DEFAULT_MAX_CONNECTIONS: usize = 64;
const MOQ_MAX_CONNECTIONS_CEILING: usize = 4096;
/// Concurrent origin subscribers (direct viewers) when
/// `RESTREAM_MOQ_ORIGIN_MAX_VIEWERS` is unset or unparseable.
pub const MOQ_DEFAULT_ORIGIN_MAX_VIEWERS: usize = 64;
const MOQ_ORIGIN_MAX_VIEWERS_CEILING: usize = 4096;

/// A configured MoQ ingest listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoqIngestConfig {
    /// UDP address the QUIC endpoint binds (an IP literal and a non-zero port).
    pub listen: SocketAddr,
    /// PEM certificate chain path.
    pub tls_cert_path: String,
    /// PEM private key path.
    pub tls_key_path: String,
    /// Handshakes in progress and publishers at once; further handshakes are
    /// refused. An admitted viewer leaves this count (`origin_max_viewers`).
    pub max_connections: usize,
    /// Origin subscribers (direct viewers over native QUIC or WebTransport)
    /// served at once across every pipeline; further viewers are refused.
    pub origin_max_viewers: usize,
}

/// Whether the MoQ ingest listener runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MoqIngestSetting {
    /// `RESTREAM_MOQ_INGEST_LISTEN` is unset: nothing MoQ runs.
    #[default]
    Off,
    On(MoqIngestConfig),
    /// Set but unusable; the message names the variable to fix.
    Invalid(String),
}

impl MoqIngestSetting {
    pub fn from_env() -> Self {
        Self::parse(
            env_var(MOQ_INGEST_LISTEN_ENV).as_deref(),
            env_var(MOQ_TLS_CERT_ENV).as_deref(),
            env_var(MOQ_TLS_KEY_ENV).as_deref(),
            env_var(MOQ_MAX_CONNECTIONS_ENV).as_deref(),
            env_var(MOQ_ORIGIN_MAX_VIEWERS_ENV).as_deref(),
        )
    }

    /// The setting the five variables describe; blank counts as unset.
    pub fn parse(
        listen: Option<&str>,
        tls_cert: Option<&str>,
        tls_key: Option<&str>,
        max_connections: Option<&str>,
        origin_max_viewers: Option<&str>,
    ) -> Self {
        fn present(value: Option<&str>) -> Option<&str> {
            value.map(str::trim).filter(|value| !value.is_empty())
        }
        let Some(listen) = present(listen) else {
            return Self::Off;
        };
        let listen = match listen.parse::<SocketAddr>() {
            Ok(listen) if listen.port() != 0 => listen,
            Ok(_) => {
                return Self::Invalid(format!(
                    "{MOQ_INGEST_LISTEN_ENV} needs a non-zero port: the ingest URLs advertise it"
                ));
            }
            Err(_) => {
                return Self::Invalid(format!(
                    "{MOQ_INGEST_LISTEN_ENV} must be an IP:port such as 0.0.0.0:4443, got {listen:?}"
                ));
            }
        };
        let (Some(tls_cert), Some(tls_key)) = (present(tls_cert), present(tls_key)) else {
            return Self::Invalid(format!(
                "{MOQ_INGEST_LISTEN_ENV} is set but {MOQ_TLS_CERT_ENV} and {MOQ_TLS_KEY_ENV} \
                 must both name PEM files"
            ));
        };
        let max_connections = present(max_connections)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(MOQ_DEFAULT_MAX_CONNECTIONS)
            .clamp(1, MOQ_MAX_CONNECTIONS_CEILING);
        let origin_max_viewers = present(origin_max_viewers)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(MOQ_DEFAULT_ORIGIN_MAX_VIEWERS)
            .clamp(1, MOQ_ORIGIN_MAX_VIEWERS_CEILING);
        Self::On(MoqIngestConfig {
            listen,
            tls_cert_path: tls_cert.to_string(),
            tls_key_path: tls_key.to_string(),
            max_connections,
            origin_max_viewers,
        })
    }

    pub fn config(&self) -> Option<&MoqIngestConfig> {
        match self {
            Self::On(config) => Some(config),
            Self::Off | Self::Invalid(_) => None,
        }
    }

    /// The UDP port publishers connect to, when the listener is configured.
    pub fn port(&self) -> Option<u16> {
        self.config().map(|config| config.listen.port())
    }

    pub fn summary(&self) -> serde_json::Value {
        match self {
            Self::Off => serde_json::json!({ "ingest": "off" }),
            Self::On(config) => serde_json::json!({
                "ingest": "on",
                "listen": config.listen.to_string(),
                "tlsCert": config.tls_cert_path,
                "tlsKey": config.tls_key_path,
                "maxConnections": config.max_connections,
                "originMaxViewers": config.origin_max_viewers,
            }),
            Self::Invalid(error) => serde_json::json!({ "ingest": "invalid", "error": error }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_or_blank_listen_address_leaves_moq_off_whatever_else_is_set() {
        assert_eq!(
            MoqIngestSetting::parse(None, None, None, None, None),
            MoqIngestSetting::Off
        );
        assert_eq!(
            MoqIngestSetting::parse(Some("  "), Some("c.pem"), Some("k.pem"), Some("8"), None),
            MoqIngestSetting::Off
        );
        assert_eq!(MoqIngestSetting::Off.port(), None);
    }

    #[test]
    fn a_full_setting_turns_the_listener_on_with_the_default_connection_cap() {
        let setting = MoqIngestSetting::parse(
            Some("0.0.0.0:4443"),
            Some("c.pem"),
            Some(" k.pem "),
            None,
            None,
        );
        assert_eq!(
            setting,
            MoqIngestSetting::On(MoqIngestConfig {
                listen: "0.0.0.0:4443".parse().unwrap(),
                tls_cert_path: "c.pem".to_string(),
                tls_key_path: "k.pem".to_string(),
                max_connections: MOQ_DEFAULT_MAX_CONNECTIONS,
                origin_max_viewers: MOQ_DEFAULT_ORIGIN_MAX_VIEWERS,
            })
        );
        assert_eq!(setting.port(), Some(4443));
        let ipv6 =
            MoqIngestSetting::parse(Some("[::1]:9443"), Some("c"), Some("k"), Some("3"), None);
        assert_eq!(
            ipv6.config().map(|c| (c.listen.port(), c.max_connections)),
            Some((9443, 3))
        );
    }

    #[test]
    fn the_connection_and_viewer_caps_are_clamped_and_garbage_falls_back_to_the_default() {
        let caps = |value| {
            MoqIngestSetting::parse(
                Some("127.0.0.1:1"),
                Some("c"),
                Some("k"),
                Some(value),
                Some(value),
            )
            .config()
            .map(|config| (config.max_connections, config.origin_max_viewers))
        };
        assert_eq!(caps("0"), Some((1, 1)));
        assert_eq!(
            caps("1000000"),
            Some((MOQ_MAX_CONNECTIONS_CEILING, MOQ_ORIGIN_MAX_VIEWERS_CEILING))
        );
        assert_eq!(
            caps("many"),
            Some((MOQ_DEFAULT_MAX_CONNECTIONS, MOQ_DEFAULT_ORIGIN_MAX_VIEWERS))
        );
        assert_eq!(caps("7"), Some((7, 7)));
    }

    #[test]
    fn a_half_configured_listener_is_invalid_and_runs_nothing() {
        for (listen, cert, key, names) in [
            (
                "localhost:4443",
                Some("c"),
                Some("k"),
                MOQ_INGEST_LISTEN_ENV,
            ),
            ("0.0.0.0", Some("c"), Some("k"), MOQ_INGEST_LISTEN_ENV),
            ("0.0.0.0:0", Some("c"), Some("k"), "non-zero port"),
            ("0.0.0.0:4443", None, Some("k"), MOQ_TLS_CERT_ENV),
            ("0.0.0.0:4443", Some("c"), Some(""), MOQ_TLS_KEY_ENV),
        ] {
            let setting = MoqIngestSetting::parse(Some(listen), cert, key, None, None);
            let MoqIngestSetting::Invalid(error) = &setting else {
                panic!("{listen} listener config should be invalid");
            };
            assert!(error.contains(names), "{error}");
            assert_eq!(setting.config(), None);
            assert_eq!(setting.port(), None);
        }
    }

    #[test]
    fn the_summary_names_the_state_without_reading_the_files() {
        assert_eq!(MoqIngestSetting::Off.summary()["ingest"], "off");
        let on = MoqIngestSetting::parse(Some("0.0.0.0:4443"), Some("c"), Some("k"), None, None);
        assert_eq!(on.summary()["listen"], "0.0.0.0:4443");
        assert_eq!(on.summary()["maxConnections"], MOQ_DEFAULT_MAX_CONNECTIONS);
        assert_eq!(
            on.summary()["originMaxViewers"],
            MOQ_DEFAULT_ORIGIN_MAX_VIEWERS
        );
        let invalid = MoqIngestSetting::parse(Some("x"), None, None, None, None);
        assert_eq!(invalid.summary()["ingest"], "invalid");
    }
}
