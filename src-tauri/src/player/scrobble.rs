//! ListenBrainz scrobbling. Fire-and-forget submissions from the player
//! worker: "playing now" on track start, a listen when the ListenBrainz
//! rules are met (track played >= 4 minutes or >= half its length).

use super::QueueTrack;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const API_URL: &str = "https://api.listenbrainz.org/1/submit-listens";
/// Tracks shorter than this are never scrobbled (ListenBrainz guideline).
const MIN_TRACK_MS: u64 = 30_000;
/// How far a forward position step may exceed the wall-clock time between two
/// samples and still count as listening (position granularity, a track that
/// ends a little off its metadata duration).
const LISTEN_SLACK_MS: u64 = 1_000;

/// Time actually listened to the current track, for the listen rule. Where a
/// track stopped says nothing about that: one skipped through by seeking still
/// ends at its full length.
///
/// Fed with the playback position (every tick, before a seek, when the track
/// stops): a forward step no longer than the wall-clock time since the
/// previous sample is listening; anything else — a seek, a reopened source —
/// only moves the baseline. A paused track does not move, so a pause counts
/// nothing even without a sample of its own.
#[derive(Debug, Default)]
pub struct ListenClock {
    listened_ms: u64,
    /// When and at which position the previous sample was taken.
    last: Option<(Instant, u64)>,
}

impl ListenClock {
    /// A new track starts at `position_ms`: nothing listened yet.
    pub fn restart(&mut self, now: Instant, position_ms: u64) {
        *self = Self {
            listened_ms: 0,
            last: Some((now, position_ms)),
        };
    }

    /// Continue measuring from `position_ms` without counting the jump there.
    pub fn rebase(&mut self, now: Instant, position_ms: u64) {
        self.last = Some((now, position_ms));
    }

    pub fn sample(&mut self, now: Instant, position_ms: u64) {
        if let Some((at, from)) = self.last {
            let elapsed_ms = now.saturating_duration_since(at).as_millis() as u64;
            if let Some(step) = position_ms.checked_sub(from) {
                if step <= elapsed_ms.saturating_add(LISTEN_SLACK_MS) {
                    self.listened_ms = self.listened_ms.saturating_add(step);
                }
            }
        }
        self.last = Some((now, position_ms));
    }

    /// Hand out the listened time and start over: the track was reported as
    /// stopped and must not be counted a second time.
    pub fn take(&mut self) -> u64 {
        std::mem::take(self).listened_ms
    }
}

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client")
    })
}

/// ListenBrainz listen rule: >= 4 minutes listened or >= 50% of the track.
/// `listened_ms` is time actually listened ([`ListenClock`]), not a position.
pub fn should_scrobble(listened_ms: u64, duration_ms: u64) -> bool {
    duration_ms >= MIN_TRACK_MS
        && (listened_ms >= 240_000 || listened_ms.saturating_mul(2) >= duration_ms)
}

fn track_metadata(track: &QueueTrack) -> serde_json::Value {
    serde_json::json!({
        "artist_name": track.artist,
        "track_name": track.name,
        "release_name": track.album,
    })
}

pub fn submit_playing_now(token: String, track: QueueTrack) {
    submit(
        token,
        serde_json::json!({
            "listen_type": "playing_now",
            "payload": [{ "track_metadata": track_metadata(&track) }],
        }),
    );
}

fn listen_body(track: &QueueTrack) -> serde_json::Value {
    let listened_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    serde_json::json!({
        "listen_type": "single",
        "payload": [{
            "listened_at": listened_at,
            "track_metadata": track_metadata(track),
        }],
    })
}

pub fn submit_listen(token: String, track: QueueTrack) {
    submit(token, listen_body(&track));
}

/// Blocking variant, for the final listen while the app is shutting down.
/// `app.exit(0)` tears the async runtime down immediately, so a spawned
/// submission would never reach the network.
pub fn submit_listen_blocking(token: String, track: QueueTrack) {
    tauri::async_runtime::block_on(post(token, listen_body(&track)));
}

async fn post(token: String, body: serde_json::Value) {
    let result = http()
        .post(API_URL)
        .header("Authorization", format!("Token {token}"))
        .json(&body)
        .send()
        .await;
    match result {
        Ok(response) if !response.status().is_success() => {
            tracing::warn!("listenbrainz rejected submission: {}", response.status());
        }
        Err(e) => tracing::debug!("listenbrainz submission failed: {e}"),
        _ => {}
    }
}

fn submit(token: String, body: serde_json::Value) {
    tauri::async_runtime::spawn(post(token, body));
}

#[cfg(test)]
mod tests {
    use super::{should_scrobble, ListenClock};
    use std::time::{Duration, Instant};

    #[test]
    fn scrobble_rules() {
        assert!(!should_scrobble(20_000, 25_000), "too short overall");
        assert!(should_scrobble(100_000, 180_000), "half of 3 minutes");
        assert!(!should_scrobble(80_000, 180_000), "under half");
        assert!(should_scrobble(240_000, 600_000), "4 minute rule");
        assert!(!should_scrobble(200_000, 600_000), "long track, not enough");
    }

    /// Plays `ms` of audio in 400 ms ticks, sampling after each one.
    fn play(clock: &mut ListenClock, now: &mut Instant, position: &mut u64, ms: u64) {
        let step_ms = 400;
        let mut left = ms;
        while left > 0 {
            let step = left.min(step_ms);
            *now += Duration::from_millis(step);
            *position += step;
            clock.sample(*now, *position);
            left -= step;
        }
    }

    #[test]
    fn listen_clock_counts_playback_between_samples() {
        let mut now = Instant::now();
        let mut position = 0;
        let mut clock = ListenClock::default();
        clock.restart(now, 0);
        play(&mut clock, &mut now, &mut position, 100_000);
        assert_eq!(clock.take(), 100_000);
        assert_eq!(clock.take(), 0, "take starts over");
    }

    #[test]
    fn listen_clock_ignores_seeks_and_pauses() {
        let mut now = Instant::now();
        let mut position = 0;
        let mut clock = ListenClock::default();
        clock.restart(now, 0);
        play(&mut clock, &mut now, &mut position, 10_000);

        // A seek forward that was not announced: a step far beyond the wall
        // clock does not count.
        now += Duration::from_millis(400);
        position = 150_000;
        clock.sample(now, position);
        // A seek back does not count either; measuring goes on from there.
        now += Duration::from_millis(400);
        position = 5_000;
        clock.sample(now, position);
        play(&mut clock, &mut now, &mut position, 2_000);

        // An announced seek (rebase) is never counted, whatever the gap.
        clock.sample(now, position);
        position = 60_000;
        clock.rebase(now, position);
        play(&mut clock, &mut now, &mut position, 3_000);

        // Paused for ten minutes: the position stands still.
        now += Duration::from_secs(600);
        clock.sample(now, position);
        play(&mut clock, &mut now, &mut position, 1_000);

        assert_eq!(clock.take(), 16_000);
    }

    #[test]
    fn a_track_skipped_through_by_seeking_is_no_listen() {
        let duration = 180_000;
        let mut now = Instant::now();
        let mut position = 0;
        let mut clock = ListenClock::default();
        clock.restart(now, 0);
        play(&mut clock, &mut now, &mut position, 10_000);
        clock.sample(now, position);
        position = 170_000;
        clock.rebase(now, position);
        play(&mut clock, &mut now, &mut position, 9_800);
        // The track ends: the player samples at the duration.
        now += Duration::from_millis(250);
        clock.sample(now, duration);
        let listened = clock.take();
        assert_eq!(listened, 20_000);
        assert!(!should_scrobble(listened, duration));
    }

    #[test]
    fn a_track_played_to_the_end_counts_its_tail() {
        let duration = 60_000;
        let mut now = Instant::now();
        let mut position = 0;
        let mut clock = ListenClock::default();
        clock.restart(now, 0);
        play(&mut clock, &mut now, &mut position, 59_700);
        // Boundary noticed 300 ms after the last tick.
        now += Duration::from_millis(300);
        clock.sample(now, duration);
        let listened = clock.take();
        assert_eq!(listened, duration);
        assert!(should_scrobble(listened, duration));
    }
}
