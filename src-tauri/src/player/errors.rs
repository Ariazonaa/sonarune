//! `player:error` — what the error banner says about playback. The player
//! sends a code and the data that goes with it (the track, the device, the
//! cause), and the UI words it in the user's language
//! (`src/lib/playerErrors.ts`, `messages/*.json`), the way
//! `player:stream-broke-off` already was. The log keeps the English.

use crate::error::AppError;
use serde::Serialize;
use std::borrow::Cow;

/// The error an output open gives when Windows has no output device at all
/// (no default to open). The banner has words of its own for it.
pub(super) const NO_OUTPUT_DEVICE: &str = "no default output device";
/// The error an output open gives when the configured device is not listed.
pub(super) const OUTPUT_DEVICE_MISSING: &str = "the selected output device is not connected";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlayerErrorCode {
    /// A track did not start (`Worker::fail_start`).
    CannotPlay,
    /// No output opened at startup; the player keeps trying.
    OutputUnavailable,
    /// An output did not open: a device picked in Settings, "Switch back",
    /// or the output opened again for playback (`Worker::reopen_output`).
    OutputFailed,
    /// The output died, and no other device opened.
    NoFallbackOutput,
}

/// What an error was about, as far as the banner can word it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CauseKind {
    /// No session (signed out): `AppError::NotConnected`.
    NotConnected,
    Network,
    /// The server answered with an error status (`ErrorCause::status`).
    Server,
    /// Windows has no output device to open.
    NoOutputDevice,
    /// The configured output device is not connected.
    OutputDeviceMissing,
    /// Anything else: the banner shows `ErrorCause::detail`.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorCause {
    pub kind: CauseKind,
    /// The error's code, as a command error carries it
    /// (`crate::error::ErrorReport`): the banner words the cause by it, the
    /// way `src/lib/appErrors.ts` words a command error.
    pub code: Cow<'static, str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// The value the code's sentence names, if it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arg: Option<String>,
    /// The error as the log has it (English, no URLs: see `AppError`). What
    /// the banner shows for a cause it has no words of its own for.
    pub detail: String,
}

impl ErrorCause {
    pub fn of(error: &AppError) -> Self {
        let report = error.report();
        let (kind, status, detail) = match error {
            AppError::NotConnected => (CauseKind::NotConnected, None, error.to_string()),
            AppError::Network(_) => (CauseKind::Network, None, error.to_string()),
            AppError::Server { status, .. } => {
                (CauseKind::Server, Some(*status), error.to_string())
            }
            // Without the "audio error:" in front: the banner says what
            // failed already.
            AppError::Audio(detail) => {
                let kind = match detail.as_str() {
                    NO_OUTPUT_DEVICE => CauseKind::NoOutputDevice,
                    OUTPUT_DEVICE_MISSING => CauseKind::OutputDeviceMissing,
                    _ => CauseKind::Other,
                };
                (kind, None, detail.clone())
            }
            _ => (CauseKind::Other, None, error.to_string()),
        };
        Self {
            kind,
            code: report.code,
            status,
            arg: report.arg,
            detail,
        }
    }
}

/// The payload of `player:error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerErrorEvent {
    pub code: PlayerErrorCode,
    /// The title of the track that did not play.
    pub track: Option<String>,
    /// The name of the output device it is about; None for the system
    /// default.
    pub device: Option<String>,
    pub cause: Option<ErrorCause>,
}

impl PlayerErrorEvent {
    fn new(code: PlayerErrorCode, error: &AppError) -> Self {
        Self {
            code,
            track: None,
            device: None,
            cause: Some(ErrorCause::of(error)),
        }
    }

    pub fn cannot_play(track: &str, error: &AppError) -> Self {
        Self {
            track: Some(track.to_string()),
            ..Self::new(PlayerErrorCode::CannotPlay, error)
        }
    }

    pub fn output_unavailable(error: &AppError) -> Self {
        Self::new(PlayerErrorCode::OutputUnavailable, error)
    }

    pub fn output_failed(device: Option<String>, error: &AppError) -> Self {
        Self {
            device,
            ..Self::new(PlayerErrorCode::OutputFailed, error)
        }
    }

    pub fn no_fallback_output(device: Option<String>, error: &AppError) -> Self {
        Self {
            device,
            ..Self::new(PlayerErrorCode::NoFallbackOutput, error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wire(event: PlayerErrorEvent) -> serde_json::Value {
        serde_json::to_value(event).unwrap()
    }

    #[test]
    fn the_banner_codes_are_stable() {
        // `src/lib/playerErrors.ts` reads exactly these shapes; a code that
        // changes here falls through to the generic sentence there.
        let decoder = AppError::Audio("decode failed: unsupported codec".into());
        assert_eq!(
            wire(PlayerErrorEvent::cannot_play("Intro", &decoder)),
            json!({
                "code": "cannotPlay",
                "track": "Intro",
                "device": null,
                "cause": {
                    "kind": "other",
                    "code": "undecodable",
                    "detail": "decode failed: unsupported codec",
                },
            })
        );
        let missing = AppError::Audio(OUTPUT_DEVICE_MISSING.into());
        assert_eq!(
            wire(PlayerErrorEvent::output_failed(
                Some("USB DAC".into()),
                &missing
            )),
            json!({
                "code": "outputFailed",
                "track": null,
                "device": "USB DAC",
                "cause": {
                    "kind": "outputDeviceMissing",
                    "code": "outputDeviceMissing",
                    "detail": OUTPUT_DEVICE_MISSING,
                },
            })
        );
        let none = AppError::Audio(NO_OUTPUT_DEVICE.into());
        assert_eq!(
            wire(PlayerErrorEvent::output_unavailable(&none))["code"],
            "outputUnavailable"
        );
        assert_eq!(
            wire(PlayerErrorEvent::output_unavailable(&none))["cause"]["kind"],
            "noOutputDevice"
        );
        assert_eq!(
            wire(PlayerErrorEvent::no_fallback_output(None, &none))["code"],
            "noFallbackOutput"
        );
    }

    #[test]
    fn a_cause_says_what_the_banner_can_word() {
        assert_eq!(
            ErrorCause::of(&AppError::NotConnected).kind,
            CauseKind::NotConnected
        );
        let server = ErrorCause::of(&AppError::Server {
            status: 503,
            message: "Service Unavailable".into(),
        });
        assert_eq!((server.kind, server.status), (CauseKind::Server, Some(503)));
        assert_eq!(
            serde_json::to_value(&server).unwrap()["kind"],
            json!("server")
        );
        // A detail the banner shows as it is keeps the English of the log,
        // without the "audio error:" the banner's own words replace.
        let other = ErrorCause::of(&AppError::Audio("the decoder crashed".into()));
        assert_eq!(
            (other.kind, other.detail.as_str()),
            (CauseKind::Other, "the decoder crashed")
        );
        assert_eq!(other.code, "decoderCrashed");
        let other = ErrorCause::of(&AppError::Other("something odd".into()));
        assert_eq!(
            (other.kind, other.code.as_ref(), other.detail.as_str()),
            (CauseKind::Other, "other", "something odd")
        );
        // The code of an output that did not open, which the banner words.
        let cpal = ErrorCause::of(&AppError::Audio(
            "cannot open audio output: The requested device is no longer available".into(),
        ));
        assert_eq!(
            (cpal.kind, cpal.code.as_ref(), cpal.arg),
            (CauseKind::Other, "outputOpenFailed", None)
        );
        let rate = ErrorCause::of(&AppError::Audio(
            "cannot decode the stream: a sample rate of 1 Hz".into(),
        ));
        assert_eq!(
            serde_json::to_value(&rate).unwrap(),
            json!({
                "kind": "other",
                "code": "sampleRate",
                "arg": "1",
                "detail": "cannot decode the stream: a sample rate of 1 Hz",
            })
        );
    }
}
