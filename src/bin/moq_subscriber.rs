//! `moq_subscriber`: the headless MoQ subscriber.
//!
//! Connects to a Restream MoQ origin or a moq-relay broadcast over native
//! QUIC, reads the hang catalog and both media tracks, prints one JSON
//! object with startup delay, frame/keyframe/audio counts and A/V arrival
//! skew, and exits:
//!
//! - `0` after a completed watch window, however many frames arrived;
//! - `2` for a usage or configuration rejection (nothing connected: bad
//!   arguments, URL, ALPN, CA path or CA PEM);
//! - `3` for a runtime, connection or protocol failure (the JSON still
//!   reports the counts gathered before it).
//!
//! A short delivery window is not a protocol failure: the harness mode
//! judges the counts ([`moq_impairment`](../test_harness/)).

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use restream_engine::moq_subscribe_entry::{
    SubscriberConfig, SubscriberError, SubscriberOutcome, run,
};

const USAGE: &str = "usage: moq_subscriber --url <https://host:port/> --ca <pem> \
--broadcast <name> [--alpn <alpn>] [--watch-secs N] [--connect-timeout-secs N] \
[--catalog-timeout-secs N] [--server-name NAME] [--publish-ready-unix-ms N]";

fn usage_error(message: &str) -> ! {
    eprintln!("moq_subscriber: {message}\n{USAGE}");
    std::process::exit(2);
}

fn required(args: &mut Args, name: &str) -> String {
    args.value(name)
        .unwrap_or_else(|| usage_error(&format!("missing --{name}")))
}

fn number(args: &mut Args, name: &str, default: u64) -> u64 {
    match args.value(name) {
        None => default,
        Some(value) => value
            .parse()
            .unwrap_or_else(|_| usage_error(&format!("--{name} {:?} is not a number", value))),
    }
}

struct Args {
    values: std::collections::HashMap<String, String>,
}

impl Args {
    fn parse(raw: &[String]) -> Self {
        let mut values = std::collections::HashMap::new();
        let mut index = 0;
        while index < raw.len() {
            let arg = &raw[index];
            let Some(name) = arg.strip_prefix("--") else {
                usage_error(&format!("unexpected argument {arg:?}"));
            };
            let (name, inline) = match name.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (name, None),
            };
            let value = match inline {
                Some(value) => value,
                None => {
                    index += 1;
                    raw.get(index)
                        .cloned()
                        .unwrap_or_else(|| usage_error(&format!("--{name} needs a value")))
                }
            };
            values.insert(name.to_string(), value);
            index += 1;
        }
        Self { values }
    }

    fn value(&mut self, name: &str) -> Option<String> {
        self.values.remove(name)
    }

    fn finish(self) {
        if let Some(unknown) = self.values.keys().next() {
            usage_error(&format!("unknown option --{unknown}"));
        }
    }
}

fn epoch_millis(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut args = Args::parse(&raw);
    let url = required(&mut args, "url");
    let ca = PathBuf::from(required(&mut args, "ca"));
    let broadcast = required(&mut args, "broadcast");
    let alpn = args
        .value("alpn")
        .unwrap_or_else(|| "moq-lite-06".to_string());
    let watch_secs = number(&mut args, "watch-secs", 15);
    let connect_timeout_secs = number(&mut args, "connect-timeout-secs", 15);
    let catalog_timeout_secs = number(&mut args, "catalog-timeout-secs", 20);
    let server_name = args
        .value("server-name")
        .unwrap_or_else(|| "localhost".to_string());
    let publish_ready_unix_ms = args
        .value("publish-ready-unix-ms")
        .map(|value| {
            value.parse().unwrap_or_else(|_| {
                usage_error(&format!(
                    "--publish-ready-unix-ms {value:?} is not a number"
                ))
            })
        })
        .unwrap_or_else(|| epoch_millis(SystemTime::now()));
    args.finish();

    let config = SubscriberConfig {
        url,
        ca_pem: ca,
        broadcast,
        alpn,
        server_name,
        watch: Duration::from_secs(watch_secs),
        connect_timeout: Duration::from_secs(connect_timeout_secs),
        catalog_timeout: Duration::from_secs(catalog_timeout_secs),
        publish_ready_unix_ms,
    };
    let result = run(config);
    match &result {
        Ok(outcome) => match serde_json::to_string(&outcome.report) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("moq_subscriber: serialize report: {error}");
                std::process::exit(3);
            }
        },
        Err(error) => eprintln!("moq_subscriber: {error}"),
    }
    let code = exit_code(&result);
    if code != 0 {
        std::process::exit(code);
    }
}

/// `0` for a completed run, `2` for a rejected invocation, `3` for a runtime
/// or connection/protocol failure.
fn exit_code(result: &Result<SubscriberOutcome, SubscriberError>) -> i32 {
    match result {
        Ok(outcome) if outcome.protocol_failed() => 3,
        Ok(_) => 0,
        Err(SubscriberError::Config(_)) => 2,
        Err(SubscriberError::Runtime(_)) => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use restream_engine::moq_subscribe_entry::SubscriberReport;

    fn outcome(error: Option<&str>) -> SubscriberOutcome {
        SubscriberOutcome {
            report: SubscriberReport {
                error: error.map(str::to_string),
                ..SubscriberReport::default()
            },
        }
    }

    #[test]
    fn exit_codes_separate_usage_config_runtime_and_protocol() {
        assert_eq!(exit_code(&Ok(outcome(None))), 0);
        assert_eq!(exit_code(&Ok(outcome(Some("MoQ connect timed out")))), 3);
        assert_eq!(
            exit_code(&Err(SubscriberError::Config("bad url".into()))),
            2,
            "rejected configuration is a usage error, not a protocol failure"
        );
        assert_eq!(
            exit_code(&Err(SubscriberError::Runtime("no runtime".into()))),
            3
        );
    }

    #[test]
    fn protocol_failure_report_keeps_partial_counts() {
        let mut report = SubscriberReport {
            error: Some("MoQ session closed".into()),
            ..SubscriberReport::default()
        };
        report.counters.video_frames = 12;
        report.counters.keyframes = 1;
        report.counters.audio_frames = 30;
        report.counters.audio_bytes = 900;
        let value = serde_json::to_value(&report).expect("report json");
        assert_eq!(value["videoFrames"], 12);
        assert_eq!(value["keyframes"], 1);
        assert_eq!(value["audioFrames"], 30);
        assert_eq!(value["audioBytes"], 900);
        assert_eq!(value["error"], "MoQ session closed");
        let result = Ok(SubscriberOutcome { report });
        assert_eq!(exit_code(&result), 3);
    }
}
