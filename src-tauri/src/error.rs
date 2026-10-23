use serde::{Serialize, Serializer};
use std::borrow::Cow;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("network error: {0}")]
    Network(reqwest::Error),
    #[error("server returned {status}: {message}")]
    Server { status: u16, message: String },
    #[error("not connected to a server")]
    NotConnected,
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("credential store error: {0}")]
    Keyring(#[from] keyring::Error),
    #[error("audio error: {0}")]
    Audio(String),
    #[error("{0}")]
    Other(String),
}

impl AppError {
    /// The same error once more, for callers that share one failed attempt.
    /// A network or database error cannot be cloned; its text is kept, so the
    /// UI shows the same message (and [`AppError::report`] finds the same
    /// code in it).
    pub fn replay(&self) -> AppError {
        match self {
            AppError::Server { status, message } => AppError::Server {
                status: *status,
                message: message.clone(),
            },
            AppError::NotConnected => AppError::NotConnected,
            AppError::Audio(message) => AppError::Audio(message.clone()),
            other => AppError::Other(other.to_string()),
        }
    }

    /// The error as the UI receives it: a code it words in the user's
    /// language, and the English it was raised with for everything else.
    pub fn report(&self) -> ErrorReport {
        let (base, status, reason) = match self {
            AppError::Network(error) => ("network", None, Some(network_reason(error))),
            // The server's own words, for the status the UI says.
            AppError::Server { status, message } => {
                ("server", Some(*status), Some(message.clone()))
            }
            AppError::NotConnected => ("notConnected", None, None),
            AppError::Db(error) => ("database", None, Some(error.to_string())),
            AppError::Keyring(error) => ("credentialStore", None, Some(error.to_string())),
            AppError::Audio(_) => ("audio", None, None),
            AppError::Other(_) => ("other", None, None),
        };
        let found = match self {
            AppError::Server { status, message } => from_server(*status, message),
            AppError::Audio(message) => known(message),
            AppError::Other(message) => marker(message).or_else(|| known(message)),
            _ => None,
        };
        let (code, arg, reason) = match found {
            Some(found) => (found.code, found.arg, found.rest),
            None => (Cow::Borrowed(base), None, reason.filter(|r| !r.is_empty())),
        };
        ErrorReport {
            code,
            status,
            arg,
            reason,
            detail: self.to_string(),
        }
    }
}

/// An error on its way to the UI (`src/lib/appErrors.ts` words it): every
/// command's rejection, and the error fields of results that report a
/// failure without failing (a Discover row, the removal half of a move).
///
/// `code` is one of the base codes of [`AppError`]'s variants (`network`,
/// `server`, `notConnected`, `database`, `credentialStore`, `audio`,
/// `other`), a code from [`KNOWN`] for a message the UI has words for, or a
/// marker an area already parses itself (`update:…`, `sonic:…`,
/// `preset:…`). The UI shows `detail` for a code it does not know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorReport {
    pub code: Cow<'static, str>,
    /// The HTTP status of a `server` error, and of codes found in one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// The one value the sentence names, for the codes that have one (the
    /// address a server redirects to, a sample rate).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arg: Option<String>,
    /// Why, in the words of whatever failed (English): the server's message
    /// of a `server` error, a network error's cause, the database's or the
    /// credential store's error, the I/O error after a known prefix. The UI
    /// adds it to the sentences where it is the reason (an export that was
    /// refused, a sign-in that could not connect) and leaves it out where it
    /// is noise (cpal's text after an output that did not open).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The error in English, as the log has it (no URLs, see
    /// `From<reqwest::Error>`), and as every build before the codes sent it.
    pub detail: String,
}

impl From<&AppError> for ErrorReport {
    fn from(error: &AppError) -> Self {
        error.report()
    }
}

/// How a message the UI has words for is recognised.
enum Shape {
    /// Exactly this.
    Is(&'static str),
    /// This, then a detail (a cpal or I/O error), sent as the `reason`.
    Starts(&'static str),
    /// A detail, then this.
    Ends(&'static str),
    /// The value the sentence names, between these two.
    Names(&'static str, &'static str),
}

/// The messages the UI words by a code of their own. They are matched where
/// the error leaves for the UI, not where it is raised, so the code that
/// raises them stays as it is; `every_known_message_is_still_raised` keeps the
/// two from drifting apart. More specific shapes come first.
const KNOWN: &[(&str, Shape)] = &[
    // Player and audio output (`player/mod.rs`, `player/errors.rs`).
    ("playerGone", Shape::Is("player thread is gone")),
    ("decoderCrashed", Shape::Is("the decoder crashed")),
    ("noOutputDevice", Shape::Is("no default output device")),
    (
        "outputDeviceMissing",
        Shape::Is("the selected output device is not connected"),
    ),
    (
        "outputDeviceUnusable",
        Shape::Starts("cannot use output device: "),
    ),
    (
        "defaultOutputFailed",
        Shape::Starts("cannot open default output: "),
    ),
    (
        "outputOpenFailed",
        Shape::Starts("cannot open audio output: "),
    ),
    (
        "outputListFailed",
        Shape::Starts("cannot list output devices: "),
    ),
    ("threadFailed", Shape::Is("cannot start opener thread")),
    (
        "threadFailed",
        Shape::Starts("cannot start the decoder thread: "),
    ),
    // Opening a track (`player/source.rs`, `player/source/*`, `opus.rs`).
    (
        "sampleRate",
        Shape::Names("cannot decode the stream: a sample rate of ", " Hz"),
    ),
    ("undecodable", Shape::Starts("cannot decode the stream: ")),
    ("undecodable", Shape::Starts("decode failed")),
    ("streamBuffer", Shape::Starts("stream buffer failed: ")),
    ("openCancelled", Shape::Is("stream open cancelled")),
    (
        "foreignQueueEntry",
        Shape::Is("queue entry belongs to a different server"),
    ),
    ("streamRequest", Shape::Is("invalid auth header")),
    ("streamRequest", Shape::Starts("bad stream url: ")),
    // What a play command found (`commands*.rs`).
    ("albumEmpty", Shape::Is("album has no tracks")),
    ("trackMissing", Shape::Is("the track is not on the server")),
    ("mixEmpty", Shape::Is("the server returned an empty mix")),
    (
        "collectionEmpty",
        Shape::Is("collection has no music tracks"),
    ),
    ("playlistEmpty", Shape::Is("playlist has no tracks")),
    ("selectionEmpty", Shape::Is("the selection has no tracks")),
    (
        "smartViewEmpty",
        Shape::Is("the smart view has no tracks to materialize"),
    ),
    (
        "sonicSameTrack",
        Shape::Is("a path needs two different tracks"),
    ),
    // Signing in and certificates (`api/mod.rs`, `commands.rs`, `store.rs`).
    ("wrongCredentials", Shape::Is("wrong username or password")),
    (
        "serverRedirect",
        Shape::Names("the server redirects to ", " — sign in with that address"),
    ),
    (
        "serverAddress",
        Shape::Is("enter the server address with http:// or https://"),
    ),
    (
        "invalidFingerprint",
        Shape::Is("not a SHA-256 certificate fingerprint"),
    ),
    (
        "otherAccount",
        Shape::Is("the server answered for another account"),
    ),
    (
        "certificateChanged",
        Shape::Is("the server presents a different certificate now"),
    ),
    ("unreachable", Shape::Is("the server cannot be reached")),
    ("noServerId", Shape::Is("the server does not say its id")),
    ("tlsSetup", Shape::Starts("tls setup failed: ")),
    // What the server sent (`api/mod.rs`).
    ("badResponse", Shape::Starts("unreadable server response")),
    ("badResponse", Shape::Is("server item is not a JSON object")),
    ("responseTooLarge", Shape::Ends(" exceeds the size limit")),
    (
        "playlistEntryMissing",
        Shape::Starts("playlist entry not found: "),
    ),
    ("noMediaSource", Shape::Starts("no media source for ")),
    // Editing (`api/mod.rs`, `api/types.rs`, `commands_*.rs`).
    ("titleEmpty", Shape::Is("title must not be empty")),
    ("yearRange", Shape::Is("year must be between 1 and 9999")),
    (
        "playlistNameEmpty",
        Shape::Is("playlist name must not be empty"),
    ),
    ("nothingSelected", Shape::Is("no playlist entries selected")),
    (
        "samePlaylist",
        Shape::Is("source and target playlists must differ"),
    ),
    // Settings (`commands.rs`, `desktop.rs`, `audio_profiles.rs`, `store.rs`).
    (
        "settingsUnsaved",
        Shape::Starts("cannot serialize settings: "),
    ),
    (
        "settingsUnsaved",
        Shape::Starts("cannot serialize desktop settings: "),
    ),
    (
        "settingsUnsaved",
        Shape::Starts("cannot serialize audio profiles: "),
    ),
    ("settingsExport", Shape::Starts("export failed: ")),
    (
        "notSettingsExport",
        Shape::Starts("not a settings export: "),
    ),
    // Files and folders (`commands.rs`, `download.rs`, `cache.rs`).
    ("downloadsFolder", Shape::Starts("no downloads folder: ")),
    (
        "downloadsFolder",
        Shape::Starts("cannot create downloads folder: "),
    ),
    (
        "downloadsFolder",
        Shape::Starts("cannot open downloads folder: "),
    ),
    (
        "outsideDownloads",
        Shape::Is("path is outside the downloads folder"),
    ),
    ("fileMissing", Shape::Is("file not found")),
    ("fileBrowser", Shape::Starts("cannot open file browser: ")),
    ("downloadMissing", Shape::Is("download not found")),
    (
        "downloadWrite",
        Shape::Starts("cannot create partial download: "),
    ),
    ("downloadWrite", Shape::Starts("download write failed: ")),
    ("downloadWrite", Shape::Starts("download flush failed: ")),
    ("downloadWrite", Shape::Starts("download sync failed: ")),
    ("downloadWrite", Shape::Starts("download finalize failed: ")),
    (
        "downloadTooLarge",
        Shape::Is("download exceeded the expected size"),
    ),
    (
        "downloadNameTaken",
        Shape::Is("too many files with the same download name"),
    ),
    (
        "diagnosticsFailed",
        Shape::Starts("diagnostic export failed: "),
    ),
    (
        "diagnosticsFailed",
        Shape::Starts("diagnostic write failed: "),
    ),
    ("coverCache", Shape::Starts("cannot create cover cache: ")),
    ("coverCache", Shape::Starts("cannot inspect cover cache: ")),
    ("coverCache", Shape::Starts("cannot read cover cache: ")),
    // A task the command waited on fell over (a panic, a shutdown).
    (
        "taskFailed",
        Shape::Starts("playback settings task failed: "),
    ),
    ("taskFailed", Shape::Starts("output switch task failed: ")),
    ("taskFailed", Shape::Starts("cover cache task failed: ")),
    (
        "taskFailed",
        Shape::Starts("diagnostic writer task failed: "),
    ),
    ("taskFailed", Shape::Starts("reveal task failed: ")),
    ("taskFailed", Shape::Starts("update install task failed: ")),
    // A replayed error (`AppError::replay`) keeps its variant's code.
    ("network", Shape::Starts("network error: ")),
    ("database", Shape::Starts("database error: ")),
    ("credentialStore", Shape::Starts("credential store error: ")),
];

/// A message recognised: its code, the value its sentence names, and what
/// the message says beyond the shape (the I/O error after a `Starts` prefix).
struct Found {
    code: Cow<'static, str>,
    arg: Option<String>,
    rest: Option<String>,
}

fn known(message: &str) -> Option<Found> {
    known_where(message, |_| true)
}

/// The messages of a `Server` error that come from this app, not from the
/// server: the rest of them is the server's own text, which must not pass
/// for a local error ("file not found" in a 404 body is not a missing file).
fn from_server(status: u16, message: &str) -> Option<Found> {
    known_where(message, |code| match code {
        // `JellyfinClient::authenticate` says so for a 401 only.
        "wrongCredentials" => status == 401,
        "serverRedirect" => true,
        _ => false,
    })
}

fn known_where(message: &str, allowed: impl Fn(&str) -> bool) -> Option<Found> {
    let rest = |text: &str| {
        Some(text.trim())
            .filter(|t| !t.is_empty())
            .map(String::from)
    };
    KNOWN
        .iter()
        .filter(|(code, _)| allowed(code))
        .find_map(|(code, shape)| {
            let (arg, remainder) = match *shape {
                Shape::Is(text) => (message == text).then_some((None, None))?,
                Shape::Starts(prefix) => (None, rest(message.strip_prefix(prefix)?)),
                Shape::Ends(suffix) => (None, rest(message.strip_suffix(suffix)?)),
                Shape::Names(prefix, suffix) => {
                    let value = message.strip_prefix(prefix)?.strip_suffix(suffix)?;
                    if value.is_empty() {
                        return None;
                    }
                    (Some(value.to_string()), None)
                }
            };
            Some(Found {
                code: Cow::Borrowed(*code),
                arg,
                rest: remainder,
            })
        })
}

/// A network error with what caused it: reqwest's own text ("error sending
/// request") says nothing about a refused connection, a name that did not
/// resolve or a certificate that was not trusted — its sources do. No URL in
/// it: the error was stripped of it (`From<reqwest::Error>`).
fn network_reason(error: &reqwest::Error) -> String {
    let mut reason = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        let text = inner.to_string();
        if !reason.contains(&text) {
            reason.push_str(": ");
            reason.push_str(&text);
        }
        source = inner.source();
    }
    reason
}

/// Areas whose errors already are codes, `<area>:<name>[:<value>]`, parsed
/// by their own UI (`updateErrors.ts`, `visualizer/presets.ts`,
/// `state/sonic.svelte.ts`): the code is `<area>:<name>`, the value `arg`.
const MARKER_AREAS: [&str; 3] = ["update", "sonic", "preset"];

fn marker(message: &str) -> Option<Found> {
    let (area, rest) = message.split_once(':')?;
    if !MARKER_AREAS.contains(&area) {
        return None;
    }
    let (name, value) = match rest.split_once(':') {
        Some((name, value)) => (name, Some(value.to_string())),
        None => (rest, None),
    };
    let word = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'-');
    word(name).then(|| Found {
        code: Cow::Owned(format!("{area}:{name}")),
        arg: value,
        rest: None,
    })
}

impl From<reqwest::Error> for AppError {
    /// Strip the URL before the error can reach a log or the UI banner.
    ///
    /// `reqwest`'s Display includes the request URL, and our URLs carry values
    /// that must not be shown or logged — the Quick Connect secret, `api_key`,
    /// `userId`, `deviceId`. The host is already known to the user, so nothing
    /// diagnostic is lost.
    fn from(err: reqwest::Error) -> Self {
        AppError::Network(err.without_url())
    }
}

// Commands return Result<T, AppError>; the frontend receives the report
// (`{ code, status?, arg?, reason?, detail }`), which `src/lib/appErrors.ts`
// words.
impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let report = self.report();
        tracing::debug!(code = %report.code, "command failed: {}", report.detail);
        report.serialize(serializer)
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wire(error: AppError) -> serde_json::Value {
        serde_json::to_value(&error).unwrap()
    }

    #[test]
    fn a_command_error_arrives_as_code_and_detail() {
        // `src/lib/appErrors.ts` reads exactly these shapes; a code that
        // changes here falls back to the detail there.
        assert_eq!(
            wire(AppError::Other("album has no tracks".into())),
            json!({ "code": "albumEmpty", "detail": "album has no tracks" })
        );
        assert_eq!(
            wire(AppError::Server {
                status: 503,
                message: "Service Unavailable".into(),
            }),
            json!({
                "code": "server",
                "status": 503,
                "reason": "Service Unavailable",
                "detail": "server returned 503: Service Unavailable",
            })
        );
        assert_eq!(
            wire(AppError::NotConnected),
            json!({ "code": "notConnected", "detail": "not connected to a server" })
        );
        assert_eq!(
            wire(AppError::Audio("player thread is gone".into())),
            json!({ "code": "playerGone", "detail": "audio error: player thread is gone" })
        );
        // What the UI has no words for keeps its variant's code.
        assert_eq!(
            wire(AppError::Audio("something odd".into())),
            json!({ "code": "audio", "detail": "audio error: something odd" })
        );
        assert_eq!(
            wire(AppError::Other("invalid search category".into())),
            json!({ "code": "other", "detail": "invalid search category" })
        );
    }

    #[test]
    fn a_code_carries_the_value_its_sentence_names() {
        let redirect = AppError::Server {
            status: 308,
            message: "the server redirects to https://music.example — sign in with that address"
                .into(),
        };
        let report = redirect.report();
        assert_eq!(
            (report.code.as_ref(), report.status, report.arg.as_deref()),
            ("serverRedirect", Some(308), Some("https://music.example"))
        );
        let wrong = AppError::Server {
            status: 401,
            message: "wrong username or password".into(),
        };
        assert_eq!(
            (wrong.report().code.as_ref(), wrong.report().status),
            ("wrongCredentials", Some(401))
        );
        let rate = AppError::Audio("cannot decode the stream: a sample rate of 3 Hz".into());
        assert_eq!(
            (rate.report().code.as_ref(), rate.report().arg.as_deref()),
            ("sampleRate", Some("3"))
        );
        // What follows a known prefix is the reason, not the value.
        let cpal = AppError::Audio("cannot open default output: device busy".into());
        assert_eq!(
            (
                cpal.report().code.as_ref(),
                cpal.report().arg,
                cpal.report().reason.as_deref()
            ),
            ("defaultOutputFailed", None, Some("device busy"))
        );
        // A message of the app's own says no more than its code.
        assert_eq!(rate.report().reason, None);
        assert_eq!(wrong.report().reason, None);
        let opus = AppError::Audio("cannot decode the stream: OpusHead too short".into());
        assert_eq!(opus.report().code, "undecodable");
        let large = AppError::Other("cover image exceeds the size limit".into());
        assert_eq!(large.report().code, "responseTooLarge");
    }

    #[test]
    fn the_reason_is_what_failed_in_its_own_words() {
        let export = AppError::Other("export failed: Access is denied. (os error 5)".into());
        assert_eq!(
            serde_json::to_value(export.report()).unwrap(),
            json!({
                "code": "settingsExport",
                "reason": "Access is denied. (os error 5)",
                "detail": "export failed: Access is denied. (os error 5)",
            })
        );
        let refused = AppError::Server {
            status: 400,
            message: "Name is too long".into(),
        };
        assert_eq!(refused.report().reason.as_deref(), Some("Name is too long"));
        let db = AppError::Db(rusqlite::Error::InvalidQuery);
        assert_eq!(
            db.report().reason,
            Some(rusqlite::Error::InvalidQuery.to_string())
        );
        // A network error says what lay underneath reqwest's own words.
        let builder = reqwest::Client::new().get("no url").build().unwrap_err();
        let shown = builder.to_string();
        let reason = AppError::from(builder).report().reason.unwrap();
        assert!(reason.starts_with(&shown), "{reason}");
        assert!(reason.len() > shown.len(), "no cause in {reason:?}");
        // Nothing to say: no reason at all.
        let empty = AppError::Other("export failed: ".into());
        assert_eq!(empty.report().reason, None);
        assert_eq!(AppError::NotConnected.report().reason, None);
    }

    #[test]
    fn a_server_message_picks_no_local_code() {
        // A body the server wrote is the server's word, whatever it says.
        for message in [
            "file not found",
            "download not found",
            "decode failed: bad data",
            "the response exceeds the size limit",
            "album has no tracks",
        ] {
            let error = AppError::Server {
                status: 404,
                message: message.into(),
            };
            assert_eq!(error.report().code, "server", "{message}");
            assert_eq!(error.report().reason.as_deref(), Some(message));
        }
        // Wrong credentials are said for a 401 only.
        let forbidden = AppError::Server {
            status: 403,
            message: "wrong username or password".into(),
        };
        assert_eq!(forbidden.report().code, "server");
    }

    #[test]
    fn an_area_code_is_passed_on() {
        let report = AppError::Other("update:no-release".into()).report();
        assert_eq!(
            (report.code.as_ref(), report.arg),
            ("update:no-release", None)
        );
        let report = AppError::Other("preset:http:404".into()).report();
        assert_eq!(
            (
                report.code.as_ref(),
                report.arg.as_deref(),
                report.detail.as_str()
            ),
            ("preset:http", Some("404"), "preset:http:404")
        );
        let report = AppError::Other("preset:network:operation timed out".into()).report();
        assert_eq!(
            (report.code.as_ref(), report.arg.as_deref()),
            ("preset:network", Some("operation timed out"))
        );
        assert_eq!(
            AppError::Other("sonic:too-few".into()).report().code,
            "sonic:too-few"
        );
        // Only from an area that has them, and only from `Other`.
        assert_eq!(
            AppError::Other("history: boom".into()).report().code,
            "other"
        );
        assert_eq!(AppError::Other("Update:Now".into()).report().code, "other");
        assert_eq!(AppError::Audio("update:none".into()).report().code, "audio");
    }

    #[test]
    fn a_replayed_error_keeps_its_code() {
        let network = reqwest::Client::new().get("no url").build().unwrap_err();
        for (error, code) in [
            (AppError::from(network), "network"),
            (AppError::Db(rusqlite::Error::InvalidQuery), "database"),
            (
                AppError::Keyring(keyring::Error::NoEntry),
                "credentialStore",
            ),
        ] {
            assert_eq!(error.report().code, code);
            // Replayed as `Other`, with the same text and so the same code
            // (a network error's sources are gone with the error).
            let replayed = error.replay();
            assert!(matches!(replayed, AppError::Other(_)));
            assert_eq!(
                (replayed.report().code, replayed.report().detail),
                (error.report().code, error.report().detail)
            );
        }
        for error in [
            AppError::NotConnected,
            AppError::Server {
                status: 404,
                message: "album a not found".into(),
            },
            AppError::Audio("the decoder crashed".into()),
            AppError::Other("album has no tracks".into()),
        ] {
            assert_eq!(error.replay().report(), error.report());
        }
    }

    #[test]
    fn every_known_message_is_still_raised() {
        // The modules that raise the messages `KNOWN` recognises. A message
        // reworded there would quietly lose its code: the UI would show the
        // English again.
        const SOURCES: &[&str] = &[
            include_str!("api/mod.rs"),
            include_str!("api/types.rs"),
            include_str!("audio_profiles.rs"),
            include_str!("cache.rs"),
            include_str!("commands.rs"),
            include_str!("commands_discover.rs"),
            include_str!("commands_library.rs"),
            include_str!("commands_metadata.rs"),
            include_str!("commands_sonic.rs"),
            include_str!("desktop.rs"),
            include_str!("download.rs"),
            include_str!("player/errors.rs"),
            include_str!("player/mod.rs"),
            include_str!("player/source.rs"),
            include_str!("player/source/feed.rs"),
            include_str!("player/source/stream.rs"),
            include_str!("store.rs"),
            include_str!("tls.rs"),
            include_str!("updater.rs"),
        ];
        // A message raised only in a test proves nothing.
        let sources: Vec<String> = SOURCES.iter().map(|s| without_tests(s)).collect();
        for (code, shape) in KNOWN {
            // A replayed error's text is its variant's `#[error]`, which
            // `a_replayed_error_keeps_its_code` checks.
            if matches!(*code, "network" | "database" | "credentialStore") {
                continue;
            }
            // A message is a string literal: what it is exactly, starts with
            // or ends with is quoted in the source that raises it.
            let pieces = match *shape {
                Shape::Is(text) => vec![format!("\"{text}\"")],
                Shape::Starts(prefix) => vec![format!("\"{prefix}")],
                Shape::Ends(suffix) => vec![format!("{suffix}\"")],
                Shape::Names(prefix, suffix) => {
                    vec![format!("\"{prefix}"), format!("{suffix}\"")]
                }
            };
            for piece in pieces {
                let in_tests = SOURCES.iter().any(|source| source.contains(&piece));
                assert!(
                    sources.iter().any(|source| source.contains(&piece)),
                    "{code}: nothing raises {piece} any more{}",
                    if in_tests { " (only a test does)" } else { "" }
                );
            }
        }
    }

    /// `source` without its `#[cfg(test)]` items: each is cut from the
    /// attribute to the end of the item — its `;`, or the brace that closes
    /// its body, skipping strings, characters and comments on the way.
    fn without_tests(source: &str) -> String {
        const MARK: &str = "#[cfg(test)]";
        let mut out = String::new();
        let mut rest = source;
        while let Some(at) = rest.find(MARK) {
            out.push_str(&rest[..at]);
            rest = &rest[at + MARK.len()..];
            rest = &rest[item_end(rest)..];
        }
        out.push_str(rest);
        out
    }

    /// Where the item at the start of `text` ends (just past it).
    fn item_end(text: &str) -> usize {
        let bytes = text.as_bytes();
        let mut depth = 0usize;
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    i += text[i..].find('\n').unwrap_or(text.len() - i);
                    continue;
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    i += text[i..].find("*/").map_or(text.len() - i, |end| end + 2);
                    continue;
                }
                // A raw string: `r"…"`, `r#"…"#`.
                b'r' if matches!(bytes.get(i + 1), Some(b'"' | b'#'))
                    && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()) =>
                {
                    let hashes = bytes[i + 1..].iter().take_while(|&&b| b == b'#').count();
                    let open = i + 1 + hashes;
                    if bytes.get(open) == Some(&b'"') {
                        let close = format!("\"{}", "#".repeat(hashes));
                        let end = text[open + 1..]
                            .find(&close)
                            .expect("an unclosed raw string");
                        i = open + 1 + end + close.len();
                        continue;
                    }
                }
                b'"' => {
                    i += 1;
                    while bytes[i] != b'"' {
                        i += if bytes[i] == b'\\' { 2 } else { 1 };
                    }
                }
                // A character, not a lifetime: `'\…'`, or one char between
                // quotes.
                b'\'' if bytes.get(i + 1) == Some(&b'\\') => {
                    // Past the escaped character, so `'\''` ends at its
                    // last quote.
                    i += 3 + text[i + 3..].find('\'').expect("an unclosed char");
                }
                b'\'' => {
                    let mut chars = text[i + 1..].chars();
                    if let (Some(c), Some('\'')) = (chars.next(), chars.next()) {
                        i += 1 + c.len_utf8();
                    }
                }
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i + 1;
                    }
                }
                b';' if depth == 0 => return i + 1,
                _ => {}
            }
            i += 1;
        }
        text.len()
    }

    #[test]
    fn test_items_are_cut_out() {
        let source = concat!(
            "a\n#[cfg(test)]\nmod tests {\n",
            "    fn f() { let s = \"}\\\"\"; let r = r#\"}\"#; let c = '}'; let q = '\\''; }\n",
            "    // }\n    /* } */\n}\n",
            "b\n#[cfg(test)]\nmod more;\n",
            "c\n#[cfg(test)]\nfn g<'a>(x: &'a str) -> &'a str { x }\n",
            "d",
        );
        assert_eq!(without_tests(source), "a\n\nb\n\nc\n\nd");
    }
}
