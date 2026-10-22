//! How a track sounds, as a short vector of numbers: the input to sonic
//! similarity (`crate::sonic`).
//!
//! Computed inside the tempo/key analysis from the same decimated mono
//! (~11 kHz) and the same onset spectra, so it costs one pass more of
//! arithmetic and no decoding. Everything here looks at one fixed band, 60 Hz
//! to 5 kHz: enough for the colour of a mix (bass weight, brightness, how
//! noisy or tonal it is), not for the shimmer of cymbals. Fixed, because the
//! analysis rate is the source rate over a whole factor — 11 025 Hz from
//! 44.1 kHz, 12 000 Hz from 48 kHz — and the same track must come out the
//! same from a FLAC and from its Opus transcode. The analyser low-passes the
//! signal at the top of the band before it decimates (`analysis.rs`), so
//! nothing above it folds in.
//!
//! Per sampled frame: 12 mel-cepstral coefficients (MFCC 1–12, the spectral
//! envelope without its loudness), spectral centroid, flatness and 85 %
//! rolloff. Per track: their means and spreads over the audible frames, plus
//! how much of the onset strength is sharp attacks, how much of it sits in
//! the bass, the average level and the dynamic range.

use serde::{Deserialize, Serialize};

/// Bump when the features change; older vectors are not compared with new
/// ones and get recomputed.
///
/// 2: a fixed band and an anti-alias filter instead of everything up to the
/// Nyquist frequency of the source; level and dynamic range over the audible
/// hops only.
pub const SONIC_VERSION: u32 = 2;

/// Length of [`SonicFeatures::vector`].
pub const DIMENSIONS: usize = 32;

const MEL_BANDS: usize = 26;
const CEPSTRA: usize = 12;
const MEL_LOW_HZ: f32 = 60.0;
const MEL_HIGH_HZ: f32 = 5_000.0;
/// Only frames this close to the loudest one describe the track; fade-ins,
/// tails and quiet breaks would otherwise pull every track towards silence.
const FRAME_FLOOR_DB: f32 = -30.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SonicFeatures {
    pub version: u32,
    /// [`DIMENSIONS`] raw values; `crate::sonic` standardizes them over the
    /// library before comparing.
    pub vector: Vec<f32>,
}

impl SonicFeatures {
    /// The analysis ran, and the track has no sound to compare (too short,
    /// too quiet). Stored so the library scan does not fetch it again.
    pub fn none() -> Self {
        Self {
            version: SONIC_VERSION,
            vector: Vec::new(),
        }
    }
}

/// One sampled frame.
struct Frame {
    hop: usize,
    cepstra: [f32; CEPSTRA],
    centroid_khz: f32,
    flatness: f32,
    rolloff_khz: f32,
}

/// Collects frames while the analyser runs; [`Timbre::finish`] folds them.
pub struct Timbre {
    /// Per mel band: (first bin, weights from that bin on).
    filters: Vec<(usize, Vec<f32>)>,
    /// DCT-II basis, `CEPSTRA` rows of `MEL_BANDS`.
    dct: Vec<[f32; MEL_BANDS]>,
    bin_khz: f32,
    /// First and last bin of the band the centroid, flatness and rolloff
    /// are taken over: the mel filters' [`MEL_LOW_HZ`]..[`MEL_HIGH_HZ`].
    band: (usize, usize),
    frames: Vec<Frame>,
}

fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10f32.powf(mel / 2595.0) - 1.0)
}

impl Timbre {
    /// `bins` magnitude bins of `bin_hz` each (an FFT of `2 * bins` points).
    pub fn new(bins: usize, bin_hz: f32) -> Self {
        let nyquist = bins as f32 * bin_hz;
        let high = MEL_HIGH_HZ.min(nyquist * 0.95);
        let (low_mel, high_mel) = (hz_to_mel(MEL_LOW_HZ), hz_to_mel(high));
        let edges: Vec<f32> = (0..MEL_BANDS + 2)
            .map(|i| {
                mel_to_hz(low_mel + (high_mel - low_mel) * i as f32 / (MEL_BANDS + 1) as f32)
                    / bin_hz
            })
            .collect();
        let filters = (0..MEL_BANDS)
            .map(|band| {
                let (lo, mid, hi) = (edges[band], edges[band + 1], edges[band + 2]);
                let first = lo.floor().max(0.0) as usize;
                let last = (hi.ceil() as usize).min(bins.saturating_sub(1));
                let weights = (first..=last)
                    .map(|bin| {
                        let b = bin as f32;
                        if b <= lo || b >= hi {
                            0.0
                        } else if b <= mid {
                            (b - lo) / (mid - lo).max(1e-6)
                        } else {
                            (hi - b) / (hi - mid).max(1e-6)
                        }
                    })
                    .collect();
                (first, weights)
            })
            .collect();
        let dct = (1..=CEPSTRA)
            .map(|k| {
                let mut row = [0f32; MEL_BANDS];
                for (n, slot) in row.iter_mut().enumerate() {
                    *slot = (std::f32::consts::PI * k as f32 * (n as f32 + 0.5) / MEL_BANDS as f32)
                        .cos();
                }
                row
            })
            .collect();
        let band = (
            (MEL_LOW_HZ / bin_hz).ceil() as usize,
            ((high / bin_hz).floor() as usize).min(bins.saturating_sub(1)),
        );
        Self {
            filters,
            dct,
            bin_khz: bin_hz / 1000.0,
            band,
            frames: Vec::new(),
        }
    }

    /// The onset bins the attack shares are measured over: from the first
    /// bin above DC (where the bass band of the analyser starts too) to the
    /// top of the band.
    pub fn flux_bins(&self) -> std::ops::RangeInclusive<usize> {
        1..=self.band.1
    }

    /// One frame: `power[bin]` is the squared magnitude of bin `bin`.
    pub fn push(&mut self, hop: usize, power: &[f32]) {
        let (low, high) = (self.band.0, self.band.1.min(power.len().saturating_sub(1)));
        if low > high {
            return;
        }
        let band = &power[low..=high];
        let total: f32 = band.iter().sum();
        if total <= 1e-12 {
            return;
        }
        let mut log_mel = [0f32; MEL_BANDS];
        for (slot, (first, weights)) in log_mel.iter_mut().zip(&self.filters) {
            let energy: f32 = weights
                .iter()
                .zip(power.iter().skip(*first))
                .map(|(w, p)| w * p)
                .sum();
            *slot = (energy + 1e-10).ln();
        }
        let mut cepstra = [0f32; CEPSTRA];
        for (slot, row) in cepstra.iter_mut().zip(&self.dct) {
            *slot = row.iter().zip(&log_mel).map(|(b, v)| b * v).sum::<f32>() / MEL_BANDS as f32;
        }

        let centroid_bin = band
            .iter()
            .enumerate()
            .map(|(i, p)| (low + i) as f32 * p)
            .sum::<f32>()
            / total;
        // Flatness: geometric over arithmetic mean, 0 (a pure tone) … 1 (noise).
        let floor = 1e-12f32;
        let log_mean = band.iter().map(|p| (p + floor).ln()).sum::<f32>() / band.len() as f32;
        let flatness = (log_mean.exp() / (total / band.len() as f32 + floor)).clamp(0.0, 1.0);
        let mut running = 0.0;
        let mut rolloff_bin = high;
        for (i, p) in band.iter().enumerate() {
            running += p;
            if running >= 0.85 * total {
                rolloff_bin = low + i;
                break;
            }
        }
        self.frames.push(Frame {
            hop,
            cepstra,
            centroid_khz: centroid_bin * self.bin_khz,
            flatness,
            rolloff_khz: rolloff_bin as f32 * self.bin_khz,
        });
    }

    /// Fold the frames between hops `start..end` into the feature vector.
    /// `level_db` is the level per hop, `envelope` / `low_envelope` the onset
    /// envelopes and `flux` the raw onset strength, all per hop. `None` when
    /// too little of the track was audible to say anything.
    pub fn finish(
        self,
        start: usize,
        end: usize,
        level_db: &[f32],
        flux: &[f32],
        envelope: &[f32],
        low_envelope: &[f32],
    ) -> Option<SonicFeatures> {
        let end = end.min(level_db.len());
        if end <= start {
            return None;
        }
        let audible = &level_db[start..end];
        let loudest = audible.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let frames: Vec<&Frame> = self
            .frames
            .iter()
            .filter(|f| {
                (start..end).contains(&f.hop)
                    && level_db
                        .get(f.hop)
                        .is_some_and(|l| *l >= loudest + FRAME_FLOOR_DB)
            })
            .collect();
        // Under ~2 s of sampled frames is not a track to compare.
        if frames.len() < 40 {
            return None;
        }

        let n = frames.len() as f32;
        let mut vector = Vec::with_capacity(DIMENSIONS);
        let mean_of =
            |value: &dyn Fn(&Frame) -> f32| frames.iter().map(|f| value(f)).sum::<f32>() / n;
        let spread_of = |value: &dyn Fn(&Frame) -> f32, mean: f32| {
            (frames
                .iter()
                .map(|f| (value(f) - mean).powi(2))
                .sum::<f32>()
                / n)
                .sqrt()
        };
        let mut means = [0f32; CEPSTRA];
        for (k, slot) in means.iter_mut().enumerate() {
            *slot = mean_of(&|f| f.cepstra[k]);
        }
        vector.extend_from_slice(&means);
        for (k, mean) in means.iter().enumerate() {
            vector.push(spread_of(&|f| f.cepstra[k], *mean));
        }
        let centroid = mean_of(&|f| f.centroid_khz);
        vector.push(centroid);
        vector.push(spread_of(&|f| f.centroid_khz, centroid));
        vector.push(mean_of(&|f| f.flatness));
        vector.push(mean_of(&|f| f.rolloff_khz));

        let sum = |values: &[f32]| {
            values[start.min(values.len())..end.min(values.len())]
                .iter()
                .map(|v| f64::from(*v))
                .sum::<f64>()
        };
        let (flux_sum, onset_sum, low_sum) = (sum(flux), sum(envelope), sum(low_envelope));
        // How much of the change is sharp attacks, and how much of those sits
        // in the kick/bass band.
        vector.push(if flux_sum > 1e-9 {
            (onset_sum / flux_sum) as f32
        } else {
            0.0
        });
        vector.push(if onset_sum > 1e-9 {
            (low_sum / onset_sum) as f32
        } else {
            0.0
        });

        // Level and dynamics of the music, not of the gaps in it: the stretch
        // between the first and the last audible hop can hold minutes of
        // digital silence (a hidden track), which would put the track's level
        // near -120 dB and its range near 100 dB.
        let mut levels: Vec<f32> = audible
            .iter()
            .copied()
            .filter(|level| *level > loudest + super::analysis::SILENCE_DB)
            .collect();
        if levels.is_empty() {
            return None;
        }
        levels.sort_by(f32::total_cmp);
        let at = |q: f32| levels[((levels.len() - 1) as f32 * q).round() as usize];
        vector.push(levels.iter().sum::<f32>() / levels.len() as f32);
        vector.push(at(0.9) - at(0.1));

        debug_assert_eq!(vector.len(), DIMENSIONS);
        if vector.iter().any(|v| !v.is_finite()) {
            return None;
        }
        Some(SonicFeatures {
            version: SONIC_VERSION,
            vector,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Timbre, DIMENSIONS};

    /// Power spectrum of a tone at `bin` with a little noise everywhere.
    fn tone(bins: usize, bin: usize, noise: f32) -> Vec<f32> {
        (0..bins)
            .map(|i| if i == bin { 1.0 } else { noise })
            .collect()
    }

    fn features(make: impl Fn(usize) -> Vec<f32>) -> Vec<f32> {
        let bins = 512;
        let mut timbre = Timbre::new(bins, 11_025.0 / 1024.0);
        let hops = 400;
        for hop in 0..hops {
            timbre.push(hop, &make(hop));
        }
        let level = vec![-10.0; hops];
        let flux = vec![1.0; hops];
        let envelope = vec![0.5; hops];
        let low = vec![0.25; hops];
        timbre
            .finish(0, hops, &level, &flux, &envelope, &low)
            .expect("enough frames")
            .vector
    }

    #[test]
    fn a_bright_noisy_frame_differs_from_a_dark_tonal_one() {
        let dark = features(|_| tone(512, 10, 1e-6));
        let bright = features(|_| (0..512).map(|i| 0.5 + (i % 7) as f32 * 0.01).collect());
        assert_eq!(dark.len(), DIMENSIONS);
        // Centroid, flatness and rolloff move the way the names say.
        assert!(
            bright[24] > dark[24] * 5.0,
            "centroid {} vs {}",
            bright[24],
            dark[24]
        );
        assert!(bright[26] > 0.9 && dark[26] < 0.1, "flatness");
        assert!(bright[27] > dark[27], "rolloff");
        // Onset shares come from the envelopes: half sharp, half of that low.
        assert!((dark[28] - 0.5).abs() < 1e-6 && (dark[29] - 0.5).abs() < 1e-6);
        // A steady level has no dynamic range.
        assert_eq!(dark[31], 0.0);
    }

    #[test]
    fn a_silent_gap_does_not_count_as_level_or_dynamics() {
        // Music, a long stretch of digital silence, then a hidden track: the
        // audible range runs from the first hop to the last.
        let bins = 512;
        let hops = 1_600;
        let gap = 400..1_400;
        let mut timbre = Timbre::new(bins, 11_025.0 / 1024.0);
        let mut level = vec![-12.0; hops];
        for (hop, slot) in level.iter_mut().enumerate() {
            if gap.contains(&hop) {
                *slot = -120.0;
                timbre.push(hop, &vec![0.0; bins]);
            } else {
                timbre.push(hop, &tone(bins, 40, 1e-4));
            }
        }
        let flux = vec![1.0; hops];
        let envelope = vec![0.5; hops];
        let low = vec![0.25; hops];
        let vector = timbre
            .finish(0, hops, &level, &flux, &envelope, &low)
            .expect("enough frames")
            .vector;
        assert_eq!(vector[30], -12.0, "level");
        assert_eq!(vector[31], 0.0, "dynamic range");
    }

    #[test]
    fn the_same_sound_gives_the_same_vector() {
        let a = features(|hop| tone(512, 40 + hop % 3, 1e-4));
        let b = features(|hop| tone(512, 40 + hop % 3, 1e-4));
        assert_eq!(a, b);
    }

    #[test]
    fn too_short_or_silent_is_nothing() {
        let mut timbre = Timbre::new(512, 10.0);
        for hop in 0..10 {
            timbre.push(hop, &tone(512, 5, 1e-3));
        }
        let level = vec![-10.0; 10];
        let zeros = vec![0.0; 10];
        assert!(timbre
            .finish(0, 10, &level, &zeros, &zeros, &zeros)
            .is_none());
        let mut silent = Timbre::new(512, 10.0);
        for hop in 0..100 {
            silent.push(hop, &vec![0.0; 512]);
        }
        let level = vec![-120.0; 100];
        let zeros = vec![0.0; 100];
        assert!(silent
            .finish(0, 100, &level, &zeros, &zeros, &zeros)
            .is_none());
    }
}
