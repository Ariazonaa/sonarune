//! ListenBrainz scrobbling. Fire-and-forget submissions from the player
//! worker: "playing now" on track start, a listen when the ListenBrainz
//! rules are met (track played >= 4 minutes or >= half its length).

use super::QueueTrack;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const API_URL: &str = "https://api.listenbrainz.org/1/submit-listens";
/// This much heard is a listen however long the track is (ListenBrainz rule).
const LISTEN_ALWAYS_MS: u64 = 240_000;
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
///
/// It also notes when the listening happened. The report comes whenever the
/// user moves on, which after a pause can be the next morning: a listen is
/// dated by when it was heard, not by when it was reported.
#[derive(Debug, Default)]
pub struct ListenClock {
    listened_ms: u64,
    /// When and at which position the previous sample was taken.
    last: Option<(Instant, u64)>,
    /// One moment read off both clocks: `Instant` measures, the calendar
    /// dates. Turns the instant of a sample into a time of day.
    anchor: Option<(Instant, SystemTime)>,
    /// When the listening began, and when it last went on.
    started_at: Option<SystemTime>,
    heard_until: Option<SystemTime>,
}

/// What the clock measured for a track that stopped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Listen {
    pub listened_ms: u64,
    /// When the listening began — the time ListenBrainz dates a listen by.
    /// `None` when nothing was heard.
    pub started_at: Option<SystemTime>,
    /// When it last went on: a pause or a sleep timer ended it there, however
    /// much later the track is reported stopped. The play history's time.
    pub ended_at: Option<SystemTime>,
}

impl ListenClock {
    /// A new track starts at `position_ms`: nothing listened yet.
    pub fn restart(&mut self, now: Instant, position_ms: u64) {
        *self = Self {
            last: Some((now, position_ms)),
            anchor: Some((now, SystemTime::now())),
            ..Self::default()
        };
    }

    /// Continue measuring from `position_ms` without counting the jump there.
    pub fn rebase(&mut self, now: Instant, position_ms: u64) {
        self.last = Some((now, position_ms));
    }

    #[cfg(test)]
    pub fn sample(&mut self, now: Instant, position_ms: u64) {
        self.sample_at(now, position_ms, 1.0);
    }

    /// `rate` is the playback speed: at 1.5x a second on the clock plays
    /// 1.5 s of the track, and that is listening, not a seek.
    pub fn sample_at(&mut self, now: Instant, position_ms: u64, rate: f32) {
        if let Some((at, from)) = self.last {
            let elapsed = now.saturating_duration_since(at);
            let elapsed_ms = elapsed.as_millis() as u64;
            let played_ms = (elapsed_ms as f64 * f64::from(rate.max(1.0))) as u64;
            if let Some(step) = position_ms.checked_sub(from) {
                if step > 0 && step <= played_ms.saturating_add(LISTEN_SLACK_MS) {
                    self.listened_ms = self.listened_ms.saturating_add(step);
                    let anchor = *self.anchor.get_or_insert_with(|| (now, SystemTime::now()));
                    self.started_at.get_or_insert(wall_time(anchor, at));
                    // The step played from the previous sample on, and took
                    // as long as it takes at this speed. Nothing samples a
                    // pause: the audio that ran on between the last tick and
                    // the pause is first noticed by the next sample, which
                    // after a night can be the report the morning after.
                    let speed = f64::from(rate.max(super::tempo::MIN_SPEED));
                    let took = Duration::from_millis((step as f64 / speed) as u64);
                    self.heard_until = Some(wall_time(anchor, at + took.min(elapsed)));
                }
            }
        }
        self.last = Some((now, position_ms));
    }

    /// Hand out what was listened and start over: the track was reported as
    /// stopped and must not be counted a second time.
    pub fn take(&mut self) -> Listen {
        let clock = std::mem::take(self);
        Listen {
            listened_ms: clock.listened_ms,
            started_at: clock.started_at,
            ended_at: clock.heard_until,
        }
    }
}

/// The time of day of `at`, by a moment both clocks were read at together.
fn wall_time((instant, system): (Instant, SystemTime), at: Instant) -> SystemTime {
    match at.checked_duration_since(instant) {
        Some(after) => system.checked_add(after),
        None => system.checked_sub(instant.duration_since(at)),
    }
    .unwrap_or(system)
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
///
/// The rule has no lower bound on the length (that is Last.fm's 30 s), so a
/// short interlude heard through counts like any track. A track whose length
/// the server does not know (0) can still reach the four minutes.
pub fn should_scrobble(listened_ms: u64, duration_ms: u64) -> bool {
    listened_ms > 0
        && (listened_ms >= LISTEN_ALWAYS_MS
            || (duration_ms > 0 && listened_ms.saturating_mul(2) >= duration_ms))
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

/// `listened_at` is when the listening began ([`Listen::started_at`]):
/// ListenBrainz wants the playback start time, not when the listen was sent.
fn listen_body(track: &QueueTrack, listened_at: Option<SystemTime>) -> serde_json::Value {
    let listened_at = listened_at
        .unwrap_or_else(SystemTime::now)
        .duration_since(UNIX_EPOCH)
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

pub fn submit_listen(token: String, track: QueueTrack, listened_at: Option<SystemTime>) {
    submit(token, listen_body(&track, listened_at));
}

/// Blocking variant, for the final listen while the app is shutting down.
/// `app.exit(0)` tears the async runtime down immediately, so a spawned
/// submission would never reach the network. Gives up after `within`: the
/// caller waits only that long, and a slow ListenBrainz must not hold the
/// player for the client's whole timeout.
pub fn submit_listen_blocking(
    token: String,
    track: QueueTrack,
    listened_at: Option<SystemTime>,
    within: Duration,
) {
    let body = listen_body(&track, listened_at);
    tauri::async_runtime::block_on(async move {
        if tokio::time::timeout(within, post(token, body))
            .await
            .is_err()
        {
            tracing::debug!("listenbrainz submission got no answer in time");
        }
    });
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
    use super::{listen_body, should_scrobble, ListenClock};
    use std::time::{Duration, Instant, UNIX_EPOCH};

    #[test]
    fn faster_playback_still_counts() {
        let mut clock = ListenClock::default();
        let start = Instant::now();
        clock.restart(start, 0);
        // Ten seconds on the clock are 15 s of track at 1.5x.
        clock.sample_at(start + Duration::from_secs(10), 15_000, 1.5);
        assert_eq!(clock.take().listened_ms, 15_000);
        // The same step at 1x is a seek.
        clock.restart(start, 0);
        clock.sample(start + Duration::from_secs(10), 15_000);
        assert_eq!(clock.take().listened_ms, 0);
    }

    #[test]
    fn scrobble_rules() {
        assert!(should_scrobble(100_000, 180_000), "half of 3 minutes");
        assert!(!should_scrobble(80_000, 180_000), "under half");
        assert!(should_scrobble(240_000, 600_000), "4 minute rule");
        assert!(!should_scrobble(200_000, 600_000), "long track, not enough");
    }

    #[test]
    fn a_short_track_heard_half_counts() {
        // ListenBrainz knows no minimum length: an interlude heard through is
        // a listen like any other.
        assert!(should_scrobble(20_000, 25_000));
        assert!(should_scrobble(5_000, 10_000));
        assert!(!should_scrobble(4_000, 10_000), "under half");
        assert!(!should_scrobble(0, 10), "nothing heard");
    }

    #[test]
    fn a_track_of_unknown_length_counts_by_the_four_minutes() {
        assert!(should_scrobble(240_000, 0));
        assert!(should_scrobble(360_000, 0));
        assert!(!should_scrobble(239_999, 0));
        assert!(!should_scrobble(0, 0));
    }

    #[test]
    fn a_listen_is_dated_by_when_it_was_heard() {
        let mut now = Instant::now();
        let start = now;
        let mut position = 0;
        let mut clock = ListenClock::default();
        clock.restart(now, 0);
        play(&mut clock, &mut now, &mut position, 120_000);
        let paused_at = now;
        // Paused overnight, then reported stopped the next morning.
        now += Duration::from_secs(8 * 3_600);
        clock.sample(now, position);
        let listen = clock.take();
        assert_eq!(listen.listened_ms, 120_000);
        let (started, ended) = (listen.started_at.unwrap(), listen.ended_at.unwrap());
        assert_eq!(
            ended.duration_since(started).unwrap(),
            paused_at - start,
            "the night does not count"
        );
        assert_eq!(clock.take(), Default::default(), "take starts over");
    }

    #[test]
    fn a_pause_between_two_ticks_ends_the_listen_where_the_audio_stopped() {
        for speed in [1.0, 1.5, 0.5] {
            let mut now = Instant::now();
            let start = now;
            let mut position = 0;
            let mut clock = ListenClock::default();
            clock.restart(now, 0);
            play(&mut clock, &mut now, &mut position, 120_000);
            // Paused 250 ms after the last tick, plus the 170 ms ramp: the
            // sink played on for 420 ms that no sample saw.
            let ran_on = Duration::from_millis(420);
            let paused_at = now + ran_on;
            position += (420.0 * speed) as u64;
            // The next sample is the report, the next morning.
            now = paused_at + Duration::from_secs(8 * 3_600);
            clock.sample_at(now, position, speed as f32);
            let listen = clock.take();
            assert_eq!(listen.listened_ms, 120_000 + (420.0 * speed) as u64);
            let (started, ended) = (listen.started_at.unwrap(), listen.ended_at.unwrap());
            assert_eq!(
                ended.duration_since(started).unwrap(),
                paused_at - start,
                "at {speed}x the night does not count"
            );
        }
    }

    #[test]
    fn nothing_heard_has_no_time() {
        let now = Instant::now();
        let mut clock = ListenClock::default();
        clock.restart(now, 0);
        // A seek is no listening.
        clock.sample(now + Duration::from_millis(400), 90_000);
        let listen = clock.take();
        assert_eq!(
            (listen.listened_ms, listen.started_at, listen.ended_at),
            (0, None, None)
        );
    }

    #[test]
    fn listenbrainz_gets_the_start_of_the_listen() {
        let track: crate::player::QueueTrack = serde_json::from_value(serde_json::json!({
            "itemId": "a", "name": "A", "artist": "B", "album": "C",
            "durationMs": 1, "streamUrl": "",
        }))
        .unwrap();
        let started = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let body = listen_body(&track, Some(started));
        assert_eq!(body["payload"][0]["listened_at"], 1_700_000_000u64);
        assert_eq!(body["payload"][0]["track_metadata"]["track_name"], "A");
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
        assert_eq!(clock.take().listened_ms, 100_000);
        assert_eq!(clock.take().listened_ms, 0, "take starts over");
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

        assert_eq!(clock.take().listened_ms, 16_000);
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
        let listened = clock.take().listened_ms;
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
        let listened = clock.take().listened_ms;
        assert_eq!(listened, duration);
        assert!(should_scrobble(listened, duration));
    }
}
