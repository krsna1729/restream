//! Loudness after ITU-R BS.1770 / EBU R128: each channel through the
//! K-weighting filter (a high shelf, then a high pass), mean square per
//! 100 ms block, and the short-term value over the last 3 s.
//!
//! Every channel weighs 1: the source's layout is not known here, and the
//! surround weights matter only past stereo.

use std::collections::VecDeque;

/// Short-term window: 30 blocks of 100 ms.
const SHORT_TERM_BLOCKS: usize = 30;

/// The floor reported for silence: R128's absolute gate.
pub const LOUDNESS_FLOOR_LUFS: f64 = -70.0;

#[derive(Debug, Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 3],
}

impl Biquad {
    /// Direct form II transposed; `state` is the filter's two delays.
    fn run(&self, state: &mut [f64; 2], x: f64) -> f64 {
        let y = self.b[0] * x + state[0];
        state[0] = self.b[1] * x - self.a[1] * y + state[1];
        state[1] = self.b[2] * x - self.a[2] * y;
        y
    }
}

/// The two K-weighting stages for `sample_rate`, from their analogue
/// prototypes (the coefficients BS.1770 tabulates at 48 kHz fall out).
fn k_weighting(sample_rate: f64) -> [Biquad; 2] {
    use std::f64::consts::PI;
    let f0 = 1_681.974_450_955_533;
    let gain_db = 3.999_843_853_973_347;
    let q = 0.707_175_236_955_419_6;
    let k = (PI * f0 / sample_rate).tan();
    let vh = 10f64.powf(gain_db / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    let f0 = 38.135_470_876_024_44;
    let q = 0.500_327_037_323_877_3;
    let k = (PI * f0 / sample_rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let high_pass = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    [shelf, high_pass]
}

/// Short-term loudness of a stream of samples.
pub struct LoudnessMeter {
    sample_rate: u32,
    channels: usize,
    filters: [Biquad; 2],
    /// Per channel, each stage's delays.
    state: Vec<[[f64; 2]; 2]>,
    block_len: usize,
    /// Sum over channels of the weighted squares in the current block.
    block_sum: f64,
    block_fill: usize,
    /// Mean square (summed over channels) of the last blocks, newest last.
    blocks: VecDeque<f64>,
}

impl LoudnessMeter {
    /// `None` for a rate or a channel count the meter cannot weigh.
    pub fn new(sample_rate: u32, channels: usize) -> Option<Self> {
        if !(8_000..=384_000).contains(&sample_rate) || !(1..=16).contains(&channels) {
            return None;
        }
        Some(Self {
            sample_rate,
            channels,
            filters: k_weighting(f64::from(sample_rate)),
            state: vec![[[0.0; 2]; 2]; channels],
            block_len: (sample_rate / 10) as usize,
            block_sum: 0.0,
            block_fill: 0,
            blocks: VecDeque::with_capacity(SHORT_TERM_BLOCKS + 1),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Feeds one stretch of audio, one slice per channel (planar), all the
    /// same length. Extra planes are ignored; missing ones count as silence.
    pub fn push_planar(&mut self, planes: &[&[f32]]) {
        let frames = planes.iter().map(|plane| plane.len()).min().unwrap_or(0);
        for index in 0..frames {
            let mut square = 0.0;
            for (channel, plane) in planes.iter().take(self.channels).enumerate() {
                let [shelf, high_pass] = &mut self.state[channel];
                let x = f64::from(plane[index]);
                let y = self.filters[1].run(high_pass, self.filters[0].run(shelf, x));
                square += y * y;
            }
            self.block_sum += square;
            self.block_fill += 1;
            if self.block_fill == self.block_len {
                if self.blocks.len() == SHORT_TERM_BLOCKS {
                    self.blocks.pop_front();
                }
                self.blocks
                    .push_back(self.block_sum / self.block_len as f64);
                self.block_sum = 0.0;
                self.block_fill = 0;
            }
        }
    }

    /// Loudness over the last 3 s, in LUFS, floored at
    /// [`LOUDNESS_FLOOR_LUFS`]; `None` until 3 s have been fed.
    pub fn short_term_lufs(&self) -> Option<f64> {
        if self.blocks.len() < SHORT_TERM_BLOCKS {
            return None;
        }
        let mean = self.blocks.iter().sum::<f64>() / self.blocks.len() as f64;
        if mean <= 0.0 {
            return Some(LOUDNESS_FLOOR_LUFS);
        }
        Some((-0.691 + 10.0 * mean.log10()).max(LOUDNESS_FLOOR_LUFS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, amplitude: f64, seconds: f64) -> Vec<f32> {
        let count = (f64::from(rate) * seconds) as usize;
        (0..count)
            .map(|n| {
                (amplitude * (2.0 * std::f64::consts::PI * freq * n as f64 / f64::from(rate)).sin())
                    as f32
            })
            .collect()
    }

    #[test]
    fn a_stereo_1khz_tone_at_minus_23_dbfs_reads_minus_23_lufs() {
        // EBU Tech 3341 test 1: a 1 kHz sine at -23 dBFS on both channels.
        for rate in [44_100, 48_000] {
            let tone = sine(rate, 1000.0, 10f64.powf(-23.0 / 20.0), 4.0);
            let mut meter = LoudnessMeter::new(rate, 2).unwrap();
            meter.push_planar(&[&tone, &tone]);
            let lufs = meter.short_term_lufs().unwrap();
            assert!((lufs + 23.0).abs() < 0.1, "{rate} Hz read {lufs}");
        }
    }

    #[test]
    fn ten_db_down_reads_ten_lu_lower_and_one_channel_three_lower() {
        let rate = 48_000;
        let loud = sine(rate, 1000.0, 10f64.powf(-20.0 / 20.0), 4.0);
        let quiet = sine(rate, 1000.0, 10f64.powf(-30.0 / 20.0), 4.0);
        let read = |planes: &[&[f32]]| {
            let mut meter = LoudnessMeter::new(rate, planes.len()).unwrap();
            meter.push_planar(planes);
            meter.short_term_lufs().unwrap()
        };
        let stereo = read(&[&loud, &loud]);
        assert!((stereo - read(&[&quiet, &quiet]) - 10.0).abs() < 0.05);
        assert!(
            (stereo - read(&[&loud]) - 3.01).abs() < 0.05,
            "mono is half the power"
        );
    }

    #[test]
    fn nothing_until_three_seconds_then_silence_reads_the_floor() {
        let mut meter = LoudnessMeter::new(48_000, 2).unwrap();
        let silence = vec![0.0f32; 48_000 * 2];
        meter.push_planar(&[&silence, &silence]);
        assert_eq!(
            meter.short_term_lufs(),
            None,
            "2 s is not a short-term window"
        );
        meter.push_planar(&[&silence[..48_000], &silence[..48_000]]);
        assert_eq!(meter.short_term_lufs(), Some(LOUDNESS_FLOOR_LUFS));
    }

    #[test]
    fn low_rumble_counts_for_less_than_the_same_level_at_1khz() {
        // The high pass of K-weighting: 20 Hz carries little loudness.
        let rate = 48_000;
        let read = |freq| {
            let tone = sine(rate, freq, 0.1, 4.0);
            let mut meter = LoudnessMeter::new(rate, 1).unwrap();
            meter.push_planar(&[&tone]);
            meter.short_term_lufs().unwrap()
        };
        assert!(read(1000.0) - read(20.0) > 10.0);
    }

    #[test]
    fn rates_and_channel_counts_out_of_range_are_refused() {
        assert!(LoudnessMeter::new(4_000, 2).is_none());
        assert!(LoudnessMeter::new(48_000, 0).is_none());
        assert!(LoudnessMeter::new(48_000, 17).is_none());
    }
}
