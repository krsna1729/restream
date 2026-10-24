//! Per-destination baselines: each running output learns its own normal
//! for a few signals and is flagged when it leaves it for a while, before a
//! fixed limit (the 0.95 delivery floor, say) would notice. Plain
//! statistics: an exponentially weighted mean and variance per signal.
//!
//! - Nothing is judged until a signal has [`WARM_UP_SAMPLES`] behind it.
//! - A signal is anomalous after [`SUSTAIN_SAMPLES`] in a row past
//!   [`ENTER_SIGMAS`] deviations on its bad side, and clears after as many
//!   back within [`EXIT_SIGMAS`].
//! - The baseline does not learn while a signal is off (or about to be), so
//!   a slow fault does not become the new normal.
//! - The deviation is never under a floor per signal: a destination that
//!   is perfectly steady is not flagged for a wobble nobody would notice.
//!
//! Fed by the flight recorder's sampler every interval; read by the alert
//! rules. An output that stops running loses its baseline and learns again.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;

/// Samples before a signal is judged: 5 min at the 2 s sampling interval.
pub const WARM_UP_SAMPLES: u32 = 150;
/// Weight of each new sample: about a 5 min memory at 2 s.
const ALPHA: f64 = 1.0 / 150.0;
pub const ENTER_SIGMAS: f64 = 4.0;
pub const EXIT_SIGMAS: f64 = 2.0;
/// Samples in a row to enter or leave an anomaly: 10 s at 2 s.
pub const SUSTAIN_SAMPLES: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Signal {
    /// Delivered over offered; bad when low.
    DeliveryRatio,
    /// Round trip to the destination; bad when high.
    RttMs,
    /// SRT packets retransmitted per second; bad when high.
    RetransPerSec,
}

impl Signal {
    const ALL: [Signal; 3] = [Signal::DeliveryRatio, Signal::RttMs, Signal::RetransPerSec];

    /// How far past the mean a value is, in deviations, on the bad side
    /// (negative on the good side).
    fn badness(self, value: f64, mean: f64, sigma: f64) -> f64 {
        let spread = sigma.max(self.floor(mean));
        match self {
            Signal::DeliveryRatio => (mean - value) / spread,
            Signal::RttMs | Signal::RetransPerSec => (value - mean) / spread,
        }
    }

    /// The least deviation counted, whatever the measured spread.
    fn floor(self, mean: f64) -> f64 {
        match self {
            Signal::DeliveryRatio => 0.005,
            Signal::RttMs => (mean.abs() * 0.1).max(5.0),
            Signal::RetransPerSec => (mean.abs() * 0.5).max(1.0),
        }
    }
}

/// One output's readings at one instant; a signal its protocol does not
/// report is `None`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Readings {
    pub delivery_ratio: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub retrans_per_sec: Option<f64>,
}

impl Readings {
    fn get(&self, signal: Signal) -> Option<f64> {
        match signal {
            Signal::DeliveryRatio => self.delivery_ratio,
            Signal::RttMs => self.rtt_ms,
            Signal::RetransPerSec => self.retrans_per_sec,
        }
        .filter(|value| value.is_finite())
    }
}

/// One signal's learned normal and its current standing.
#[derive(Debug, Clone, Default)]
struct Baseline {
    mean: f64,
    variance: f64,
    samples: u32,
    /// Consecutive samples past the entry line (or, while anomalous, back
    /// within the exit line).
    streak: u32,
    /// When the current streak began: an anomaly dates from its first bad
    /// sample, not from when it was confirmed.
    streak_from_ms: u64,
    anomalous_since_ms: Option<u64>,
    last_value: f64,
}

impl Baseline {
    fn observe(&mut self, signal: Signal, at_ms: u64, value: f64) {
        self.last_value = value;
        if self.samples >= WARM_UP_SAMPLES {
            let badness = signal.badness(value, self.mean, self.variance.sqrt());
            match self.anomalous_since_ms {
                None if badness > ENTER_SIGMAS => {
                    if self.streak == 0 {
                        self.streak_from_ms = at_ms;
                    }
                    self.streak += 1;
                    if self.streak >= SUSTAIN_SAMPLES {
                        self.anomalous_since_ms = Some(self.streak_from_ms);
                        self.streak = 0;
                    }
                    // Not learned: it may be the start of a fault.
                    return;
                }
                None => self.streak = 0,
                Some(_) if badness < EXIT_SIGMAS => {
                    self.streak += 1;
                    if self.streak >= SUSTAIN_SAMPLES {
                        self.anomalous_since_ms = None;
                        self.streak = 0;
                    }
                    return;
                }
                Some(_) => {
                    self.streak = 0;
                    return;
                }
            }
        }
        if self.samples == 0 {
            self.mean = value;
            self.variance = 0.0;
        } else {
            let delta = value - self.mean;
            self.mean += ALPHA * delta;
            self.variance = (1.0 - ALPHA) * (self.variance + ALPHA * delta * delta);
        }
        self.samples = self.samples.saturating_add(1);
    }
}

/// A destination off its usual for a signal.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Anomaly {
    pub pipeline_id: String,
    pub output_id: String,
    pub signal: Signal,
    pub value: f64,
    pub usual: f64,
    pub since_ms: u64,
}

#[derive(Debug, Default)]
struct OutputBaselines {
    pipeline_id: String,
    signals: HashMap<Signal, Baseline>,
}

/// Every running output's baselines.
#[derive(Debug, Default)]
pub struct DestinationBaselines {
    outputs: Mutex<HashMap<String, OutputBaselines>>,
}

impl DestinationBaselines {
    /// One sample of every running output: `(pipeline, output, readings)`.
    /// An output absent from the sample is forgotten.
    pub fn observe(&self, at_ms: u64, sample: Vec<(String, String, Readings)>) {
        let mut outputs = crate::sync::lock(&self.outputs);
        let present: std::collections::HashSet<&str> = sample
            .iter()
            .map(|(_, output, _)| output.as_str())
            .collect();
        outputs.retain(|output, _| present.contains(output.as_str()));
        for (pipeline_id, output_id, readings) in sample {
            let entry = outputs.entry(output_id).or_default();
            entry.pipeline_id = pipeline_id;
            for signal in Signal::ALL {
                if let Some(value) = readings.get(signal) {
                    entry
                        .signals
                        .entry(signal)
                        .or_default()
                        .observe(signal, at_ms, value);
                }
            }
        }
    }

    /// The destinations off their usual now, ordered by output and signal.
    pub fn anomalies(&self) -> Vec<Anomaly> {
        let outputs = crate::sync::lock(&self.outputs);
        let mut found: Vec<Anomaly> = outputs
            .iter()
            .flat_map(|(output_id, output)| {
                output.signals.iter().filter_map(|(signal, baseline)| {
                    baseline.anomalous_since_ms.map(|since_ms| Anomaly {
                        pipeline_id: output.pipeline_id.clone(),
                        output_id: output_id.clone(),
                        signal: *signal,
                        value: baseline.last_value,
                        usual: baseline.mean,
                        since_ms,
                    })
                })
            })
            .collect();
        found.sort_by(|a, b| {
            (a.output_id.as_str(), a.signal as u8).cmp(&(b.output_id.as_str(), b.signal as u8))
        });
        found
    }
}

#[cfg(test)]
#[path = "destination_baselines_tests.rs"]
mod tests;
