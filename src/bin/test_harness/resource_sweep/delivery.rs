//! Receiver-side per-destination delivery and fairness for the rated window.
//!
//! Follows the nginx-media capacity-quality method: every sampler tick records
//! the bytes each destination path has received at the MediaMTX sink
//! (`/v3/paths/list` `bytesReceived`) and the bytes Restream has received from
//! its publishers (the offered media). Per destination this yields the average
//! delivery ratio (destination rate / offered rate per pipeline) and the worst
//! single-interval ratio, which exposes stalls an average hides. Across
//! destinations it reports how many met the delivery floor and Jain's fairness
//! index over their rates. Measured at the receiver so the system under test
//! never grades itself, and identically for any Restream build.

use std::collections::BTreeMap;

use super::*;

/// Destinations at or above this share of the offered rate count as delivered
/// (nginx-media `CAPACITY_QUALITY_MIN_DELIVERY_RATIO`). Wire framing makes a
/// healthy ratio sit slightly above 1.0.
pub(super) const DELIVERY_FLOOR: f64 = 0.95;

/// In-process harness sink counters (`MSR_PEER=sink`); empty for MediaMTX.
#[derive(Clone, Copy, Default)]
pub(super) struct HarnessSinks<'a> {
    pub(super) rtmp: &'a [Arc<GeneralizedSinkMetrics>],
    pub(super) srt: Option<&'a crate::harness_srt_sink::SrtSinkCountersHandle>,
}

#[derive(Debug, Clone)]
pub(super) struct DeliverySample {
    pub(super) at: Instant,
    /// Cumulative receiver bytes per destination path.
    pub(super) destinations: BTreeMap<String, u64>,
    /// Cumulative publisher bytes received by Restream, summed over pipelines.
    pub(super) offered_bytes: u64,
    pub(super) pipelines: usize,
    /// Restream's own per-feed delivery telemetry at this tick, when the build
    /// reports it (`/api/v1/pipelines/{id}/telemetry` `delivery`).
    pub(super) reported: Option<ReportedDelivery>,
}

/// Restream-reported delivery folded over every feed: the cross-check for the
/// receiver-side numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct ReportedDelivery {
    pub(super) rated: usize,
    pub(super) delivered: usize,
    pub(super) ratio_min: f64,
    pub(super) jain_min: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct DeliverySummary {
    pub(super) destinations: usize,
    pub(super) delivered: usize,
    pub(super) offered_bps: f64,
    pub(super) ratio_min: f64,
    pub(super) ratio_median: f64,
    pub(super) interval_ratio_min: f64,
    pub(super) jain: f64,
    pub(super) reported: Option<ReportedDelivery>,
}

/// Jain's fairness index: `(Σx)² / (n·Σx²)`, 1.0 when all rates are equal and
/// `1/n` when one destination receives everything.
pub(super) fn jain(values: &[f64]) -> f64 {
    let total: f64 = values.iter().sum();
    let squares: f64 = values.iter().map(|value| value * value).sum();
    if values.is_empty() || squares == 0.0 {
        0.0
    } else {
        total * total / (values.len() as f64 * squares)
    }
}

fn bytes_rate(later: u64, earlier: u64, seconds: f64) -> f64 {
    later.saturating_sub(earlier) as f64 * 8.0 / seconds
}

/// Summarize a rated window from its first and last samples (average rates)
/// and every consecutive pair (interval rates). A destination missing from a
/// later sample contributes zero delivery rather than vanishing.
pub(super) fn summarize(samples: &[DeliverySample]) -> DeliverySummary {
    let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
        return DeliverySummary::default();
    };
    let seconds = last.at.duration_since(first.at).as_secs_f64();
    if seconds <= 0.0 || last.pipelines == 0 {
        return DeliverySummary::default();
    }
    let per_pipeline = |sample_bps: f64| sample_bps / last.pipelines as f64;
    let offered_bps = per_pipeline(bytes_rate(last.offered_bytes, first.offered_bytes, seconds));
    if offered_bps <= 0.0 {
        return DeliverySummary::default();
    }
    let rates: Vec<f64> = first
        .destinations
        .iter()
        .map(|(name, start)| {
            let end = last.destinations.get(name).copied().unwrap_or(*start);
            bytes_rate(end, *start, seconds)
        })
        .collect();
    let mut ratios: Vec<f64> = rates.iter().map(|rate| rate / offered_bps).collect();
    ratios.sort_by(f64::total_cmp);

    let mut interval_ratio_min = f64::INFINITY;
    for pair in samples.windows(2) {
        let span = pair[1].at.duration_since(pair[0].at).as_secs_f64();
        let offered = per_pipeline(bytes_rate(
            pair[1].offered_bytes,
            pair[0].offered_bytes,
            span,
        ));
        if span <= 0.0 || offered <= 0.0 {
            continue;
        }
        for (name, start) in &first.destinations {
            let before = pair[0].destinations.get(name).copied().unwrap_or(*start);
            let after = pair[1].destinations.get(name).copied().unwrap_or(before);
            interval_ratio_min = interval_ratio_min.min(bytes_rate(after, before, span) / offered);
        }
    }

    DeliverySummary {
        destinations: ratios.len(),
        delivered: ratios
            .iter()
            .filter(|ratio| **ratio >= DELIVERY_FLOOR)
            .count(),
        offered_bps,
        ratio_min: ratios.first().copied().unwrap_or(0.0),
        ratio_median: ratios.get(ratios.len() / 2).copied().unwrap_or(0.0),
        interval_ratio_min: if interval_ratio_min.is_finite() {
            interval_ratio_min
        } else {
            0.0
        },
        jain: jain(&rates),
        reported: last.reported,
    }
}

/// One tick: receiver bytes per destination plus Restream's per-pipeline
/// publisher bytes. MediaMTX peers report per path; the harness SRT sink
/// (`MSR_PEER=sink`) reports per connection, so SRT fan-out can be measured
/// with a receiver that is not the bottleneck.
pub(super) async fn sample(
    env: &ResourceSweepEnv,
    api: &RampApi,
    sinks: HarnessSinks<'_>,
) -> Result<DeliverySample, String> {
    let client = reqwest::Client::new();
    let mut destinations = BTreeMap::new();
    for (listener, metrics) in sinks.rtmp.iter().enumerate() {
        for (connection, bytes) in metrics.per_connection_bytes().into_iter().enumerate() {
            destinations.insert(format!("rtmp-sink:{listener}:{connection}"), bytes);
        }
    }
    if let Some(sink) = sinks.srt {
        for ((port, peer), bytes) in sink.per_peer_bytes() {
            destinations.insert(format!("srt-sink:{port}:{peer}"), bytes);
        }
    }
    let mediamtx_instances = if env.peer_mode == ResourceSweepPeer::Mediamtx {
        env.peer_count.max(1)
    } else {
        0
    };
    for index in 0..mediamtx_instances {
        let (_, _, _, api_port) = peer_instance_ports(env, index);
        let body = client
            .get(format!("http://127.0.0.1:{api_port}/v3/paths/list"))
            .send()
            .await
            .map_err(|error| format!("mediamtx paths: {error}"))?
            .text()
            .await
            .map_err(|error| format!("mediamtx paths body: {error}"))?;
        let paths: Value =
            serde_json::from_str(&body).map_err(|error| format!("mediamtx paths json: {error}"))?;
        for item in paths["items"].as_array().into_iter().flatten() {
            if let (Some(name), Some(bytes)) =
                (item["name"].as_str(), item["bytesReceived"].as_u64())
            {
                destinations.insert(format!("{index}:{name}"), bytes);
            }
        }
    }
    let health = api.get_json("/api/v1/engine/health").await?;
    let pipelines = health["pipelines"].as_object();
    let mut reported: Option<ReportedDelivery> = None;
    for pipeline_id in pipelines.into_iter().flat_map(|pipelines| pipelines.keys()) {
        let Ok(telemetry) = api
            .get_json(&format!("/api/v1/pipelines/{pipeline_id}/telemetry"))
            .await
        else {
            continue;
        };
        for feed in telemetry["delivery"].as_array().into_iter().flatten() {
            let (Some(rated), Some(delivered)) =
                (feed["rated"].as_u64(), feed["delivered"].as_u64())
            else {
                continue;
            };
            let entry = reported.get_or_insert(ReportedDelivery {
                ratio_min: f64::INFINITY,
                jain_min: f64::INFINITY,
                ..ReportedDelivery::default()
            });
            entry.rated += rated as usize;
            entry.delivered += delivered as usize;
            if let Some(ratio) = feed["ratioMin"].as_f64() {
                entry.ratio_min = entry.ratio_min.min(ratio);
            }
            if let Some(jain) = feed["jain"].as_f64() {
                entry.jain_min = entry.jain_min.min(jain);
            }
        }
    }
    let offered_bytes = pipelines
        .into_iter()
        .flat_map(|pipelines| pipelines.values())
        .filter_map(|pipeline| pipeline["input"]["bytesReceived"].as_u64())
        .sum();
    Ok(DeliverySample {
        at: Instant::now(),
        destinations,
        offered_bytes,
        pipelines: pipelines.map_or(0, |pipelines| pipelines.len()),
        reported,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at: Instant, offered: u64, destinations: &[(&str, u64)]) -> DeliverySample {
        DeliverySample {
            at,
            destinations: destinations
                .iter()
                .map(|(name, bytes)| (name.to_string(), *bytes))
                .collect(),
            offered_bytes: offered,
            pipelines: 1,
            reported: None,
        }
    }

    #[test]
    fn jain_is_one_for_equal_rates_and_one_over_n_for_a_single_winner() {
        assert!((jain(&[5.0, 5.0, 5.0, 5.0]) - 1.0).abs() < 1e-9);
        assert!((jain(&[8.0, 0.0, 0.0, 0.0]) - 0.25).abs() < 1e-9);
        assert_eq!(jain(&[]), 0.0);
    }

    #[test]
    fn a_starving_destination_lowers_ratio_count_and_fairness() {
        let start = Instant::now();
        let second = start + Duration::from_secs(1);
        let end = start + Duration::from_secs(2);
        let samples = [
            sample(start, 0, &[("a", 0), ("b", 0)]),
            sample(second, 1_000, &[("a", 1_000), ("b", 0)]),
            sample(end, 2_000, &[("a", 2_000), ("b", 1_000)]),
        ];
        let summary = summarize(&samples);
        assert_eq!(summary.destinations, 2);
        assert_eq!(summary.delivered, 1);
        assert!((summary.ratio_min - 0.5).abs() < 1e-9);
        // "b" received nothing in the first second.
        assert_eq!(summary.interval_ratio_min, 0.0);
        assert!(summary.jain < 1.0);
    }

    #[test]
    fn full_delivery_to_every_destination_is_fair() {
        let start = Instant::now();
        let end = start + Duration::from_secs(4);
        let samples = [
            sample(start, 10, &[("a", 10), ("b", 20)]),
            sample(end, 4_010, &[("a", 4_050), ("b", 4_060)]),
        ];
        let summary = summarize(&samples);
        assert_eq!(summary.delivered, 2);
        assert!(summary.ratio_min >= 1.0);
        assert!((summary.jain - 1.0).abs() < 1e-9);
    }
}
