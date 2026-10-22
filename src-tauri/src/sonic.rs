//! Sonic similarity: which tracks sound alike, and in which order to play
//! them.
//!
//! Everything comes from the client's own analysis (`player/analysis.rs`,
//! `player/timbre.rs`): no server plugin, no second service. A track is a
//! point made of its standardized sound vector, its tempo and its key; the
//! distance between two tracks weighs all three:
//!
//! - **Sound:** Euclidean distance of the vectors after each dimension is
//!   standardized over the library (so brightness in kHz and a cepstral
//!   coefficient count alike), divided by the square root of the dimension
//!   count — two random tracks land near 1.4.
//! - **Tempo:** how far apart the BPM are on a log scale, with half and double
//!   time counting as the same groove; 7 % apart is 1, and it stops growing
//!   at 1.5 — past mixable is past mixable.
//! - **Key:** 0 for the same Camelot code, a little for a harmonic neighbour,
//!   1 for a clash.
//!
//! On top of that distance sit three ways of ordering: a radio around one
//! seed, a path that walks from one track to another, and the continuation
//! Auto-DJ uses. All of them build the sequence one track at a time and look
//! at the transition from the previous track as well as at the goal, which
//! is what keeps them from drifting off or jumping around.

use crate::player::analysis::{MusicalKey, TrackAnalysis};
use crate::player::timbre::{SonicFeatures, DIMENSIONS, SONIC_VERSION};
use std::collections::HashSet;

/// Weight of the tempo and the key term against the sound term.
const TEMPO_WEIGHT: f32 = 0.35;
const KEY_WEIGHT: f32 = 0.25;
const TEMPO_CAP: f32 = 1.5;
/// What an unknown tempo costs: half of "cannot be mixed".
const UNKNOWN_TEMPO: f32 = TEMPO_CAP / 2.0;
/// A tempo or key that is not known costs this much: neither a match nor a
/// clash.
const UNKNOWN_TERM: f32 = 0.5;
/// How strongly a path step must also fit after the previous step.
const PATH_SMOOTHING: f32 = 0.5;

/// One analysed track.
#[derive(Debug, Clone)]
struct Point {
    item_id: String,
    /// Standardized sound vector. Every dimension counts the same: on a real
    /// library, weighting the cepstral spreads or the level down made the
    /// neighbours no better (`evaluate`).
    sound: Vec<f32>,
    bpm: Option<f32>,
    /// How sure the tempo is, 0..1: a pad without a beat still gets a BPM
    /// from the analysis, and it must not count like a drummer's.
    tempo_trust: f32,
    key: Option<MusicalKey>,
    /// How far this track's own neighbours are (mean sound distance to its
    /// closest few): the local density the distances are measured against.
    reach: f32,
}

/// Every analysed track with a sound vector, ready to compare.
#[derive(Debug, Default)]
pub struct SonicIndex {
    points: Vec<Point>,
    /// Mean [`Point::reach`], so scaled distances keep their usual size.
    mean_reach: f32,
}

/// Neighbours [`Point::reach`] averages over.
const REACH_NEIGHBOURS: usize = 7;
/// Points the reach is estimated against: all of a small library, an even
/// spread of a large one (n × 1000 distances instead of n²).
const REACH_SAMPLE: usize = 1000;

/// Raw input for [`SonicIndex::build`].
pub struct Analysed {
    pub item_id: String,
    pub analysis: TrackAnalysis,
}

fn usable(features: &Option<SonicFeatures>) -> Option<&[f32]> {
    features
        .as_ref()
        .filter(|f| f.version == SONIC_VERSION && f.vector.len() == DIMENSIONS)
        .map(|f| f.vector.as_slice())
}

impl SonicIndex {
    pub fn build(rows: Vec<Analysed>) -> Self {
        let rows: Vec<(Analysed, Vec<f32>)> = rows
            .into_iter()
            .filter_map(|row| {
                let vector = usable(&row.analysis.sonic)?.to_vec();
                Some((row, vector))
            })
            .collect();
        if rows.is_empty() {
            return Self::default();
        }
        let n = rows.len() as f32;
        let mut mean = [0f32; DIMENSIONS];
        for (_, vector) in &rows {
            for (m, v) in mean.iter_mut().zip(vector) {
                *m += v / n;
            }
        }
        let mut spread = [0f32; DIMENSIONS];
        for (_, vector) in &rows {
            for ((s, v), m) in spread.iter_mut().zip(vector).zip(&mean) {
                *s += (v - m).powi(2) / n;
            }
        }
        // A dimension that does not vary carries no information; a floor
        // keeps it from dividing by nothing.
        let spread: Vec<f32> = spread.iter().map(|s| s.sqrt().max(1e-6)).collect();
        let scale = 1.0 / (DIMENSIONS as f32).sqrt();
        let points = rows
            .into_iter()
            .map(|(row, vector)| Point {
                sound: vector
                    .iter()
                    .enumerate()
                    .map(|(d, v)| (v - mean[d]) / spread[d] * scale)
                    .collect(),
                item_id: row.item_id,
                bpm: row.analysis.bpm,
                tempo_trust: tempo_trust(row.analysis.tempo_confidence),
                key: row.analysis.key,
                reach: 1.0,
            })
            .collect::<Vec<Point>>();
        let mut index = Self {
            points,
            mean_reach: 1.0,
        };
        index.measure_reach();
        index
    }

    /// Local scaling against hubs. In a space of a few dozen dimensions some
    /// tracks sit near the middle of everything and come out as "closest"
    /// to half the library — a rap track in the radio of a yodel. Measuring
    /// each distance against how far both tracks' own neighbours are (d /
    /// sqrt(reach_a · reach_b)) takes that advantage away: a hub has close
    /// neighbours everywhere, so its distances grow.
    fn measure_reach(&mut self) {
        let n = self.points.len();
        if n < 2 {
            return;
        }
        let step = n.div_ceil(REACH_SAMPLE).max(1);
        let sample: Vec<usize> = (0..n).step_by(step).collect();
        let reaches: Vec<f32> = (0..n)
            .map(|i| {
                let mut distances: Vec<f32> = sample
                    .iter()
                    .filter(|&&j| j != i)
                    .map(|&j| sound_distance(&self.points[i].sound, &self.points[j].sound))
                    .collect();
                distances.sort_by(f32::total_cmp);
                let k = distances.len().clamp(1, REACH_NEIGHBOURS);
                (distances.iter().take(k).sum::<f32>() / k as f32).max(1e-3)
            })
            .collect();
        self.mean_reach = reaches.iter().sum::<f32>() / n as f32;
        for (point, reach) in self.points.iter_mut().zip(reaches) {
            point.reach = reach;
        }
    }

    /// Sound distance between two vectors whose reaches are given.
    fn scaled_sound(&self, a: &[f32], reach_a: f32, b: &[f32], reach_b: f32) -> f32 {
        sound_distance(a, b) * self.mean_reach / (reach_a * reach_b).sqrt()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    fn find(&self, item_id: &str) -> Option<usize> {
        self.points.iter().position(|p| p.item_id == item_id)
    }

    pub fn contains(&self, item_id: &str) -> bool {
        self.find(item_id).is_some()
    }

    /// Up to `count` tracks around `seed`: the `count` that sound most like
    /// it, played in an order that flows (each next one the closest to the
    /// one before). Choosing first and ordering second is what keeps a radio
    /// from wandering off — a chain that only looks one step ahead walks
    /// away from its seed one small step at a time. The seed itself is not
    /// part of the result; `exclude` never is.
    pub fn radio(&self, seed: &str, count: usize, exclude: &HashSet<String>) -> Vec<String> {
        let Some(seed) = self.find(seed) else {
            return Vec::new();
        };
        let mut used = self.excluded(exclude);
        used[seed] = true;
        let chosen = self.nearest(&used, count, |c| self.distance(seed, c));
        self.flow(seed, chosen)
    }

    /// `steps` tracks that lead from `from` to `to`: step i aims at the point
    /// i/(steps+1) of the way, in sound and in tempo, and must also follow on
    /// from the step before. Neither end is part of the result. `None` when
    /// one of the two has not been analysed.
    pub fn path(
        &self,
        from: &str,
        to: &str,
        steps: usize,
        exclude: &HashSet<String>,
    ) -> Option<Vec<String>> {
        let (a, b) = (self.find(from)?, self.find(to)?);
        let mut used = self.excluded(exclude);
        used[a] = true;
        used[b] = true;
        let (start, end) = (&self.points[a], &self.points[b]);
        let mut previous = a;
        let mut out = Vec::new();
        for step in 1..=steps {
            let t = step as f32 / (steps + 1) as f32;
            let target: Vec<f32> = start
                .sound
                .iter()
                .zip(&end.sound)
                .map(|(x, y)| x + (y - x) * t)
                .collect();
            let target_reach = start.reach + (end.reach - start.reach) * t;
            // Geometric: halfway from 90 to 160 BPM is 120, not 125.
            let target_bpm = match (start.bpm, end.bpm) {
                (Some(x), Some(y)) => Some(x * (y / x).powf(t)),
                (x, y) => x.or(y),
            };
            let Some(next) = self.best(&used, |c| {
                let point = &self.points[c];
                self.scaled_sound(&target, target_reach, &point.sound, point.reach)
                    + TEMPO_WEIGHT * tempo_term(target_bpm, 1.0, point.bpm, point.tempo_trust)
                    + PATH_SMOOTHING * self.distance(previous, c)
            }) else {
                break;
            };
            used[next] = true;
            out.push(self.points[next].item_id.clone());
            previous = next;
        }
        Some(out)
    }

    /// What to play after `recent` (newest last): the tracks closest to the
    /// mean sound of the last few, played on from the newest. Follows a set
    /// that has been moving instead of snapping back to where it began.
    pub fn continuation(
        &self,
        recent: &[String],
        count: usize,
        exclude: &HashSet<String>,
    ) -> Vec<String> {
        let known: Vec<usize> = recent
            .iter()
            .rev()
            .filter_map(|id| self.find(id))
            .take(5)
            .collect();
        let Some(&last) = known.first() else {
            return Vec::new();
        };
        let mut centre = vec![0f32; DIMENSIONS];
        for &i in &known {
            for (c, v) in centre.iter_mut().zip(&self.points[i].sound) {
                *c += v / known.len() as f32;
            }
        }
        let centre_reach =
            known.iter().map(|&i| self.points[i].reach).sum::<f32>() / known.len() as f32;
        let mut used = self.excluded(exclude);
        for &i in &known {
            used[i] = true;
        }
        let chosen = self.nearest(&used, count, |c| {
            self.scaled_sound(
                &centre,
                centre_reach,
                &self.points[c].sound,
                self.points[c].reach,
            ) + TEMPO_WEIGHT
                * tempo_term(
                    self.points[last].bpm,
                    self.points[last].tempo_trust,
                    self.points[c].bpm,
                    self.points[c].tempo_trust,
                )
        });
        self.flow(last, chosen)
    }

    /// The `count` unused points with the lowest score.
    fn nearest(&self, used: &[bool], count: usize, score: impl Fn(usize) -> f32) -> Vec<usize> {
        let mut scored: Vec<(usize, f32)> = (0..self.points.len())
            .filter(|&i| !used[i])
            .map(|i| (i, score(i)))
            .collect();
        scored.sort_by(|a, b| a.1.total_cmp(&b.1));
        scored.truncate(count);
        scored.into_iter().map(|(i, _)| i).collect()
    }

    /// Order `chosen` starting after `from`: each next track is the one
    /// closest to the track before it.
    fn flow(&self, from: usize, mut chosen: Vec<usize>) -> Vec<String> {
        let mut previous = from;
        let mut out = Vec::with_capacity(chosen.len());
        while !chosen.is_empty() {
            let (at, _) = chosen
                .iter()
                .enumerate()
                .map(|(at, &c)| (at, self.distance(previous, c)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .expect("not empty");
            previous = chosen.swap_remove(at);
            out.push(self.points[previous].item_id.clone());
        }
        out
    }

    fn excluded(&self, exclude: &HashSet<String>) -> Vec<bool> {
        self.points
            .iter()
            .map(|p| exclude.contains(&p.item_id))
            .collect()
    }

    /// The unused point with the lowest score.
    fn best(&self, used: &[bool], score: impl Fn(usize) -> f32) -> Option<usize> {
        (0..self.points.len())
            .filter(|&i| !used[i])
            .map(|i| (i, score(i)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    fn distance(&self, a: usize, b: usize) -> f32 {
        let (a, b) = (&self.points[a], &self.points[b]);
        self.scaled_sound(&a.sound, a.reach, &b.sound, b.reach)
            + TEMPO_WEIGHT * tempo_term(a.bpm, a.tempo_trust, b.bpm, b.tempo_trust)
            + KEY_WEIGHT * key_distance(a.key, b.key)
    }
}

/// While analyses keep arriving (a library scan), an index this young is
/// still good enough: rebuilding after every single track would parse the
/// whole library again and again.
const REBUILD_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

/// The index over the analyses of `server` — the session's — rebuilt after
/// an analysis was written, at most every [`REBUILD_AFTER`] while they keep
/// coming. Parsing a large library's analyses takes a moment; this is the one
/// place that pays for it.
pub fn index(
    store: &crate::store::Store,
    server: &crate::identity::Server,
) -> std::sync::Arc<SonicIndex> {
    index_containing(store, server, &[])
}

/// [`index`] for a caller that needs `needed` in it: when one of them is
/// missing from a cached index that analyses have been written since, it is
/// rebuilt at once instead of after [`REBUILD_AFTER`] — a track analysed a
/// moment ago must not be reported as not analysed.
pub fn index_containing(
    store: &crate::store::Store,
    server: &crate::identity::Server,
    needed: &[&str],
) -> std::sync::Arc<SonicIndex> {
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Instant;
    struct Built {
        server: crate::identity::Server,
        generation: u64,
        at: Instant,
        index: Arc<SonicIndex>,
    }
    static CACHE: OnceLock<Mutex<Option<Built>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let generation = store.analysis_generation();
    // Held while building on purpose: a second caller waits for this build
    // instead of starting the same one.
    let mut cached = cache.lock().unwrap();
    if let Some(built) = cached.as_ref() {
        if built.server == *server
            && still_good(
                &built.index,
                built.generation != generation,
                built.at.elapsed(),
                needed,
            )
        {
            return built.index.clone();
        }
    }
    let rows = store
        .analyses_for(server)
        .unwrap_or_else(|e| {
            tracing::warn!("cannot read the stored analyses: {e}");
            Vec::new()
        })
        .into_iter()
        .filter_map(|(item_id, json)| {
            let analysis = serde_json::from_str(&json).ok()?;
            Some(Analysed { item_id, analysis })
        })
        .collect();
    let index = Arc::new(SonicIndex::build(rows));
    *cached = Some(Built {
        server: server.clone(),
        generation,
        at: Instant::now(),
        index: index.clone(),
    });
    index
}

/// Whether a cached index may be served instead of a rebuild: nothing was
/// written since it was built, or it is young and holds what the caller
/// needs. An empty index is never young — the first analyses make it useful.
fn still_good(
    index: &SonicIndex,
    written_since: bool,
    age: std::time::Duration,
    needed: &[&str],
) -> bool {
    !written_since
        || (age < REBUILD_AFTER
            && !index.points.is_empty()
            && needed.iter().all(|id| index.contains(id)))
}

/// Whether a stored analysis already carries a current sound vector.
pub fn has_sound(analysis: &TrackAnalysis) -> bool {
    usable(&analysis.sonic).is_some()
}

/// Whether the current sound analysis has run on this track, with or without
/// a vector to show for it (too short or too quiet gives none).
pub fn sound_attempted(analysis: &TrackAnalysis) -> bool {
    analysis
        .sonic
        .as_ref()
        .is_some_and(|f| f.version == SONIC_VERSION)
}

/// Whether the stored analysis of `item_id` is through the current sound
/// analysis without a sound to compare: too short or too quiet. Unlike a
/// track not analysed yet, no scan and no play will change that.
pub fn without_sound(store: &crate::store::Store, item_id: &str) -> bool {
    store
        .get_analysis(item_id)
        .and_then(|json| serde_json::from_str::<TrackAnalysis>(&json).ok())
        .is_some_and(|analysis| sound_attempted(&analysis) && !has_sound(&analysis))
}

/// Take the analyses of `gone` — tracks the server no longer returns to the
/// account `owner` — out of the index (`Store::release_analyses`), as far as
/// that holds for every account here. The server answers for the account
/// signed in, but an analysis serves every account of its server, and a
/// track hidden from this one may be another's. So where another account has
/// played on this server, only the tracks this account has played itself
/// are released: it saw them, and now they are gone. Returns how many.
pub fn release_gone(
    store: &crate::store::Store,
    owner: &crate::identity::Owner,
    gone: Vec<String>,
) -> crate::error::AppResult<usize> {
    if gone.is_empty() {
        return Ok(0);
    }
    let gone: Vec<String> = if crate::history::shared_with_others(store, owner)? {
        let played = crate::history::played_by(store, owner, &gone)?;
        gone.into_iter().filter(|id| played.contains(id)).collect()
    } else {
        gone
    };
    store.release_analyses(&owner.server, &gone)
}

/// The tempo part of a distance. An unknown or doubtful tempo costs what a
/// so-so match costs, never nothing: a track without a detected beat must
/// not win every comparison just because it has no tempo to disagree with.
fn tempo_term(a: Option<f32>, trust_a: f32, b: Option<f32>, trust_b: f32) -> f32 {
    if a.is_none() || b.is_none() {
        return UNKNOWN_TEMPO;
    }
    let trust = trust_a.min(trust_b);
    trust * tempo_distance(a, b) + (1.0 - trust) * UNKNOWN_TEMPO
}

/// Tempo confidence (autocorrelation peak, `analysis.rs`) mapped to how much
/// a tempo difference counts: nothing at the acceptance threshold, fully from
/// a clear beat on.
fn tempo_trust(confidence: f32) -> f32 {
    ((confidence - 0.15) / 0.25).clamp(0.0, 1.0)
}

fn sound_distance(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f32>()
        .sqrt()
}

/// 0 for the same tempo (or half/double time), 1 for 7 % apart, capped at
/// 1.5: past ~10 % two tracks cannot be mixed in time anyway, and further
/// apart must not outweigh how they sound.
fn tempo_distance(a: Option<f32>, b: Option<f32>) -> f32 {
    let (Some(a), Some(b)) = (a, b) else {
        return UNKNOWN_TERM;
    };
    if a <= 0.0 || b <= 0.0 {
        return UNKNOWN_TERM;
    }
    let octaves = (a / b).log2();
    let off = [octaves, octaves - 1.0, octaves + 1.0]
        .into_iter()
        .map(f32::abs)
        .fold(f32::INFINITY, f32::min);
    (off / 0.0976).min(TEMPO_CAP) // log2(1.07)
}

fn key_distance(a: Option<MusicalKey>, b: Option<MusicalKey>) -> f32 {
    match (a, b) {
        (Some(a), Some(b)) if a == b => 0.0,
        (Some(a), Some(b)) if a.compatible(b) => 0.3,
        (Some(_), Some(_)) => 1.0,
        _ => UNKNOWN_TERM,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::analysis::VERSION;

    /// A track whose sound is `level` in every dimension plus a little
    /// personality, so the index has spread to standardize.
    fn track(id: &str, level: f32, bpm: Option<f32>, key: Option<(u8, bool)>) -> Analysed {
        let vector = (0..DIMENSIONS)
            .map(|d| level + (d as f32 * 0.37 + level * 3.1).sin() * 0.05)
            .collect();
        Analysed {
            item_id: id.into(),
            analysis: TrackAnalysis {
                version: VERSION,
                bpm,
                tempo_confidence: 0.5,
                intro: None,
                outro: None,
                key: key.map(|(tonic, minor)| MusicalKey { tonic, minor }),
                key_correlation: 0.8,
                audible_start_ms: 0.0,
                audible_end_ms: 1.0,
                duration_ms: 1.0,
                server_duration_ms: None,
                sonic: Some(SonicFeatures {
                    version: SONIC_VERSION,
                    vector,
                }),
            },
        }
    }

    fn line(n: usize) -> SonicIndex {
        SonicIndex::build(
            (0..n)
                .map(|i| track(&format!("t{i}"), i as f32, Some(120.0), Some((0, false))))
                .collect(),
        )
    }

    #[test]
    fn tracks_without_a_current_sound_vector_are_left_out() {
        let mut stale = track("stale", 1.0, None, None);
        stale.analysis.sonic.as_mut().unwrap().version = SONIC_VERSION + 1;
        let mut none = track("none", 1.0, None, None);
        none.analysis.sonic = None;
        let index = SonicIndex::build(vec![track("a", 0.0, None, None), stale, none]);
        assert_eq!(index.len(), 1);
        assert!(index.contains("a") && !index.contains("stale"));
    }

    #[test]
    fn the_radio_starts_with_the_closest_and_skips_what_is_excluded() {
        let index = line(10);
        let exclude: HashSet<String> = ["t6".to_string()].into();
        let radio = index.radio("t5", 4, &exclude);
        assert_eq!(radio.len(), 4);
        assert_eq!(radio[0], "t4");
        assert!(!radio.contains(&"t5".to_string()) && !radio.contains(&"t6".to_string()));
        // Everything stays in the neighbourhood of the seed.
        assert!(radio.iter().all(|id| {
            let i: i32 = id[1..].parse().unwrap();
            (i - 5).abs() <= 3
        }));
        assert!(index.radio("unknown", 4, &HashSet::new()).is_empty());
    }

    #[test]
    fn a_path_walks_from_one_end_to_the_other_in_order() {
        let index = line(12);
        let path = index.path("t0", "t11", 4, &HashSet::new()).unwrap();
        let positions: Vec<i32> = path.iter().map(|id| id[1..].parse().unwrap()).collect();
        assert_eq!(positions.len(), 4);
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{positions:?}");
        assert!(positions[0] > 0 && positions[3] < 11);
        assert!(index.path("t0", "nope", 4, &HashSet::new()).is_none());
    }

    #[test]
    fn a_young_index_is_rebuilt_for_a_track_it_does_not_hold_yet() {
        use std::time::Duration;
        let index = line(3);
        let young = Duration::from_secs(5);
        let old = REBUILD_AFTER + Duration::from_secs(1);
        // Nothing written since it was built: good, whatever is asked for.
        assert!(still_good(&index, false, old, &["unknown"]));
        // Written since: a young index with what is needed is good enough …
        assert!(still_good(&index, true, young, &["t1"]));
        assert!(still_good(&index, true, young, &[]));
        // … but not when the track asked for is missing: it may be the one
        // analysed a moment ago …
        assert!(!still_good(&index, true, young, &["t1", "new"]));
        // … and not once it is old, or while it is empty.
        assert!(!still_good(&index, true, old, &[]));
        assert!(!still_good(&SonicIndex::default(), true, young, &[]));
    }

    #[test]
    fn tempo_and_key_break_a_tie_in_sound() {
        let index = SonicIndex::build(vec![
            track("seed", 0.0, Some(124.0), Some((9, true))),
            // Same sound, but a clashing key and an odd tempo …
            track("clash", 0.5, Some(97.0), Some((2, false))),
            // … against the same sound in a neighbouring key, double time.
            track("fits", 0.5, Some(248.0), Some((4, true))),
            track("far", 9.0, Some(124.0), Some((9, true))),
        ]);
        assert_eq!(index.radio("seed", 1, &HashSet::new()), ["fits"]);
    }

    #[test]
    fn a_continuation_follows_where_the_set_has_gone() {
        let index = line(20);
        let recent: Vec<String> = ["t2", "t8", "t9", "t10"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let next = index.continuation(&recent, 3, &HashSet::new());
        assert_eq!(next.len(), 3);
        assert!(!next.iter().any(|id| recent.contains(id)));
        let first: i32 = next[0][1..].parse().unwrap();
        assert!((7..=12).contains(&first), "{next:?}");
    }

    /// A tiny deterministic generator, so the families below vary from
    /// track to track without a dependency.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as f32) / (1u64 << 31) as f32
        }
        fn range(&mut self, lo: f32, hi: f32) -> f32 {
            lo + (hi - lo) * self.next()
        }
    }

    /// 20 s of one of four kinds of music, each track with its own tempo,
    /// pitches and balance. Mono at 22.05 kHz.
    fn render(family: usize, rng: &mut Lcg) -> Vec<f32> {
        use std::f32::consts::TAU;
        let rate = 22_050.0;
        let n = (rate * 20.0) as usize;
        let mut out = vec![0f32; n];
        let gain = rng.range(0.5, 1.0);
        match family {
            // Dub: deep sine bass and a slow, soft kick; almost nothing above.
            0 => {
                let (bass, bpm) = (rng.range(45.0, 65.0), rng.range(70.0, 82.0));
                let beat = (rate * 60.0 / bpm) as usize;
                for (i, v) in out.iter_mut().enumerate() {
                    let t = i as f32 / rate;
                    let k = (i % beat) as f32 / rate;
                    *v = 0.6 * (TAU * bass * t).sin()
                        + 0.5 * (-k * 12.0).exp() * (TAU * 50.0 * k).sin();
                }
            }
            // Techno: four-to-the-floor kick, offbeat noise hats, saw bass.
            1 => {
                let (bass, bpm) = (rng.range(80.0, 110.0), rng.range(124.0, 132.0));
                let beat = (rate * 60.0 / bpm) as usize;
                for (i, v) in out.iter_mut().enumerate() {
                    let t = i as f32 / rate;
                    let k = (i % beat) as f32 / rate;
                    let h = ((i + beat / 2) % beat) as f32 / rate;
                    let saw = 2.0 * (bass * t).fract() - 1.0;
                    *v = 0.7
                        * (-k * 25.0).exp()
                        * (TAU * (50.0 + 80.0 * (-k * 30.0).exp()) * k).sin()
                        + 0.35 * (-h * 60.0).exp() * (rng.next() * 2.0 - 1.0)
                        + 0.2 * saw;
                }
            }
            // Ambient: a slow chord of sines in the mids, no attacks.
            2 => {
                let root = rng.range(180.0, 300.0);
                let tones = [root, root * 1.25, root * 1.5, root * 2.0];
                for (i, v) in out.iter_mut().enumerate() {
                    let t = i as f32 / rate;
                    let swell = 0.6 + 0.4 * (TAU * 0.05 * t).sin();
                    *v = swell * tones.iter().map(|f| (TAU * f * t).sin()).sum::<f32>() * 0.2;
                }
            }
            // Noise rock: clipped square riffs and broadband noise, fast.
            _ => {
                let (note, bpm) = (rng.range(140.0, 260.0), rng.range(150.0, 172.0));
                let beat = (rate * 60.0 / bpm) as usize;
                for (i, v) in out.iter_mut().enumerate() {
                    let t = i as f32 / rate;
                    let k = (i % beat) as f32 / rate;
                    let square = if (note * t).fract() < 0.5 { 1.0 } else { -1.0 };
                    *v = (0.5 * square + 0.4 * (rng.next() * 2.0 - 1.0))
                        * (0.6 + 0.4 * (-k * 8.0).exp());
                }
            }
        }
        out.iter().map(|v| (v * gain).clamp(-1.0, 1.0)).collect()
    }

    /// The whole chain on audio, not on made-up vectors: analyser, timbre
    /// features, standardization, distance. Each track's closest neighbour
    /// has to be of its own kind.
    #[test]
    fn the_nearest_track_is_of_the_same_kind() {
        let mut rng = Lcg(7);
        let per_family = 4;
        let mut rows = Vec::new();
        for family in 0..4 {
            for i in 0..per_family {
                let mut analyzer = crate::player::analysis::Analyzer::new(22_050, 1);
                for sample in render(family, &mut rng) {
                    analyzer.push(sample);
                }
                rows.push(Analysed {
                    item_id: format!("{family}-{i}"),
                    analysis: analyzer.finish(),
                });
            }
        }
        assert!(
            rows.iter().all(|r| has_sound(&r.analysis)),
            "every track has a sound vector"
        );
        let index = SonicIndex::build(rows);
        let mut right = 0;
        let total = 4 * per_family;
        for family in 0..4 {
            for i in 0..per_family {
                let id = format!("{family}-{i}");
                let nearest = index.radio(&id, 1, &HashSet::new());
                if nearest
                    .first()
                    .is_some_and(|n| n.starts_with(&format!("{family}-")))
                {
                    right += 1;
                }
            }
        }
        assert_eq!(
            right, total,
            "{right} of {total} nearest neighbours of the same kind"
        );
    }

    /// Neighbours over a real database, for judging by hand:
    /// `SONARUNE_EVAL_DIR=<data dir> cargo test --release --lib sonic::tests::evaluate -- --ignored --nocapture`.
    /// Prints one JSON line per analysed track: its five nearest, a radio of
    /// ten, and for a few pairs a path. Plays nothing, and works on a copy:
    /// opening a store tidies it (`Store::tidy` deletes analyses released
    /// long ago and settles server ids), which must not happen to the real
    /// library behind a judging run. The copy is taken through a read-only
    /// connection, so a running app's database is read consistently.
    #[test]
    #[ignore]
    fn evaluate() {
        let Ok(dir) = std::env::var("SONARUNE_EVAL_DIR") else {
            return;
        };
        let source = std::path::Path::new(&dir).join(crate::store::DB_FILE);
        let copy = std::env::temp_dir().join(format!("sonarune-evaluate-{}", std::process::id()));
        std::fs::create_dir_all(&copy).unwrap();
        let copied = copy.join(crate::store::DB_FILE);
        rusqlite::Connection::open_with_flags(&source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
            .execute("VACUUM INTO ?1", [copied.to_string_lossy()])
            .unwrap();
        let store = crate::store::Store::open(&copy).unwrap();
        let server = crate::identity::Server::current(&store).expect("a server signed in");
        let index = index(&store, &server);
        let none = HashSet::new();
        for point in &index.points {
            let me = index.find(&point.item_id).unwrap();
            let nearest = index.nearest(
                &[false; 0]
                    .iter()
                    .copied()
                    .chain((0..index.points.len()).map(|i| i == me))
                    .collect::<Vec<_>>(),
                5,
                |c| index.distance(me, c),
            );
            let nearest: Vec<String> = nearest
                .into_iter()
                .map(|i| index.points[i].item_id.clone())
                .collect();
            println!(
                "{}",
                serde_json::json!({
                    "seed": point.item_id,
                    "bpm": point.bpm,
                    "key": point.key.map(|k| k.camelot()),
                    "nearest": nearest,
                    "radio": index.radio(&point.item_id, 10, &none),
                })
            );
        }
        let ids: Vec<&str> = index.points.iter().map(|p| p.item_id.as_str()).collect();
        for pair in ids.chunks(2).step_by(7).take(6) {
            if let [a, b] = pair {
                println!(
                    "{}",
                    serde_json::json!({"from": a, "to": b, "path": index.path(a, b, 8, &none)})
                );
            }
        }
        drop(store);
        let _ = std::fs::remove_dir_all(&copy);
    }

    #[test]
    fn tempo_distance_treats_double_time_as_the_same_groove() {
        assert!(tempo_distance(Some(120.0), Some(240.0)) < 1e-4);
        assert!(tempo_distance(Some(120.0), Some(60.0)) < 1e-4);
        assert!((tempo_distance(Some(100.0), Some(107.0)) - 1.0).abs() < 0.02);
        assert_eq!(tempo_distance(Some(100.0), Some(140.0)), TEMPO_CAP);
        assert_eq!(tempo_distance(None, Some(120.0)), UNKNOWN_TERM);
    }
}
