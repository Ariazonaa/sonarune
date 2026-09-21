//! Online "Butterchurn Weekly" visualizer presets, fetched and verified in Rust.
//!
//! Butterchurn compiles a preset's equation strings with `new Function`, so a
//! preset file is code, running in a window that can invoke every command. The
//! WebView therefore never downloads one itself: it asks `fetch_weekly_preset`
//! for a file name, and this module
//! - accepts only `<32 hex>.json` names pinned in `weekly_presets.json`, the
//!   SHA-256 manifest `scripts/pin-weekly-presets.mjs` generates from the
//!   installed `butterchurn-presets-weekly` package (the 32-hex names are not
//!   content hashes, so the manifest is the only integrity anchor),
//! - builds the URL from a fixed origin and the manifest's own key, and fetches
//!   it over public-CA TLS without redirects, with timeouts and size caps,
//! - hands the body over only when its decoded content hashes to the pin.
//!
//! Whether online presets may load at all stays the frontend's decision (the
//! persisted opt-in, gated in `controller.ts` `#loadEntry`), and so does
//! caching verified presets (`presets.ts`).
//!
//! Errors reach the frontend as `preset:<code>[:<detail>]` strings (see
//! [`PresetError::code`]); `presets.ts` sorts them into permanent failures
//! (the entry is quarantined) and transient ones (it is kept).

use crate::error::{AppError, AppResult};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read as _;
use std::sync::OnceLock;
use std::time::Duration;

/// Where every Weekly preset lives. Must match `ORIGIN` in the pin script.
const ORIGIN: &str = "https://s3-us-east-2.amazonaws.com/butterchurn-presets/";
/// Cap for the body, both as transferred and decoded (presets are a few kB).
const MAX_PRESET_BYTES: usize = 2 * 1024 * 1024;
const MANIFEST_JSON: &str = include_str!("weekly_presets.json");

#[derive(Debug, PartialEq, Eq)]
enum PresetError {
    /// Not a `<32 lowercase hex>.json` file name.
    InvalidName,
    /// Well-formed, but not pinned in the manifest.
    NotListed,
    /// The content does not hash to its pin: it changed on the server.
    Changed,
    /// Any status other than 200 (redirects included; none are followed).
    Http(u16),
    TooLarge,
    /// Unsupported or corrupt `Content-Encoding`, or content that is not UTF-8.
    Undecodable,
    /// Connection, TLS, timeout or body-read failure.
    Network(String),
}

impl PresetError {
    /// The wire form `presets.ts` parses. Keep both sides in sync.
    fn code(&self) -> String {
        match self {
            PresetError::InvalidName => "preset:invalid-name".into(),
            PresetError::NotListed => "preset:not-listed".into(),
            PresetError::Changed => "preset:changed".into(),
            PresetError::Http(status) => format!("preset:http:{status}"),
            PresetError::TooLarge => "preset:too-large".into(),
            PresetError::Undecodable => "preset:undecodable".into(),
            PresetError::Network(message) => format!("preset:network:{message}"),
        }
    }
}

impl From<PresetError> for AppError {
    fn from(error: PresetError) -> Self {
        AppError::Other(error.code())
    }
}

fn network(error: reqwest::Error) -> PresetError {
    let error = error.without_url();
    let mut message = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(inner) = source {
        let _ = write!(message, ": {inner}");
        source = inner.source();
    }
    PresetError::Network(message)
}

/// The compiled-in pins, `"<32hex>.json"` → SHA-256 hex.
fn manifest() -> &'static HashMap<String, String> {
    static MANIFEST: OnceLock<HashMap<String, String>> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        serde_json::from_str(MANIFEST_JSON).unwrap_or_else(|error| {
            // Fails closed (nothing is pinned, nothing is fetched); a unit test
            // keeps the committed manifest parseable.
            tracing::error!("weekly preset manifest is unreadable: {error}");
            HashMap::new()
        })
    })
}

fn is_valid_name(file: &str) -> bool {
    let bytes = file.as_bytes();
    bytes.len() == 37
        && bytes[..32]
            .iter()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && &bytes[32..] == b".json"
}

/// The manifest's own file name and pinned hash for a requested name.
fn pinned(file: &str) -> Result<(&'static str, &'static str), PresetError> {
    if !is_valid_name(file) {
        return Err(PresetError::InvalidName);
    }
    manifest()
        .get_key_value(file)
        .map(|(name, hash)| (name.as_str(), hash.as_str()))
        .ok_or(PresetError::NotListed)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(bytes).iter() {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Accept `content` only when it hashes to `pin` (hex, compared lowercase).
fn verify(pin: &str, content: &[u8]) -> Result<(), PresetError> {
    if sha256_hex(content) == pin.trim().to_ascii_lowercase() {
        Ok(())
    } else {
        Err(PresetError::Changed)
    }
}

/// Undo the transfer encoding. S3 stores these files gzip-compressed and
/// serves them with `Content-Encoding: gzip` whatever the request says; the
/// pin covers the decoded bytes, which is what butterchurn gets to run.
fn decode(encoding: Option<&str>, raw: Vec<u8>) -> Result<Vec<u8>, PresetError> {
    match encoding
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("") | Some("identity") => Ok(raw),
        Some("gzip") | Some("x-gzip") => {
            let mut decoded = Vec::new();
            // Multi-member aware, like the zlib decoder the pin script's
            // `fetch` uses; bounded so a small body cannot inflate unchecked.
            flate2::read::MultiGzDecoder::new(raw.as_slice())
                .take(MAX_PRESET_BYTES as u64 + 1)
                .read_to_end(&mut decoded)
                .map_err(|_| PresetError::Undecodable)?;
            if decoded.len() > MAX_PRESET_BYTES {
                return Err(PresetError::TooLarge);
            }
            Ok(decoded)
        }
        Some(_) => Err(PresetError::Undecodable),
    }
}

/// Decode, verify against the pin, and only then read the bytes as text.
fn verified_text(pin: &str, encoding: Option<&str>, raw: Vec<u8>) -> Result<String, PresetError> {
    let content = decode(encoding, raw)?;
    verify(pin, &content)?;
    String::from_utf8(content).map_err(|_| PresetError::Undecodable)
}

fn client() -> AppResult<&'static reqwest::Client> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    // No pins: only a certificate the OS trust store accepts gets through. No
    // redirects: the origin is fixed and has no reason to send one.
    let (builder, _observed) = crate::tls::apply(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(30)),
        &[],
    )?;
    let client = builder.build()?;
    Ok(CLIENT.get_or_init(|| client))
}

/// The raw body (size-capped while reading) and its `Content-Encoding`.
async fn download(
    client: &reqwest::Client,
    file: &'static str,
) -> Result<(Option<String>, Vec<u8>), PresetError> {
    let mut response = client
        .get(format!("{ORIGIN}{file}"))
        .send()
        .await
        .map_err(network)?;
    let status = response.status();
    if status != reqwest::StatusCode::OK {
        return Err(PresetError::Http(status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PRESET_BYTES as u64)
    {
        return Err(PresetError::TooLarge);
    }
    let encoding = match response.headers().get(reqwest::header::CONTENT_ENCODING) {
        Some(value) => Some(
            value
                .to_str()
                .map_err(|_| PresetError::Undecodable)?
                .to_owned(),
        ),
        None => None,
    };
    let mut raw = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if raw.len() + chunk.len() > MAX_PRESET_BYTES {
            return Err(PresetError::TooLarge);
        }
        raw.extend_from_slice(&chunk);
    }
    Ok((encoding, raw))
}

/// Fetch one pinned Butterchurn Weekly preset and return its JSON text.
#[tauri::command]
pub async fn fetch_weekly_preset(file: String) -> AppResult<String> {
    let (file, pin) = pinned(&file)?;
    let (encoding, raw) = download(client()?, file).await?;
    let text = verified_text(pin, encoding.as_deref(), raw);
    if text == Err(PresetError::Changed) {
        tracing::warn!("weekly preset {file} changed on the server; refusing it");
    }
    Ok(text?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    const HEX: &str = "0123456789abcdef0123456789abcdef";

    fn listed() -> (&'static str, &'static str) {
        let (file, hash) = manifest().iter().next().expect("manifest has pins");
        (file.as_str(), hash.as_str())
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn manifest_is_well_formed() {
        let parsed: HashMap<String, String> =
            serde_json::from_str(MANIFEST_JSON).expect("weekly_presets.json parses");
        assert!(!parsed.is_empty());
        assert_eq!(manifest().len(), parsed.len());
        for (file, hash) in &parsed {
            assert!(is_valid_name(file), "bad file name {file:?}");
            assert!(
                hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
                "bad pin for {file}: {hash:?}"
            );
        }
    }

    #[test]
    fn looks_up_listed_names() {
        let (file, hash) = listed();
        assert_eq!(pinned(file), Ok((file, hash)));
        // The returned name is the manifest's, not the caller's string.
        let owned = file.to_string();
        assert!(std::ptr::eq(pinned(&owned).unwrap().0, file));

        let unlisted = format!("{}.json", "0".repeat(32));
        assert!(!manifest().contains_key(&unlisted));
        assert_eq!(pinned(&unlisted), Err(PresetError::NotListed));
        assert_eq!(pinned(&format!("{HEX}.json")), Err(PresetError::NotListed));
    }

    #[test]
    fn rejects_malformed_names() {
        let (file, _) = listed();
        let names = [
            String::new(),
            HEX.to_string(),
            format!("{HEX}.JSON"),
            format!("{}.json", HEX.to_uppercase()),
            format!("{}.json", &HEX[1..]),
            format!("{HEX}0.json"),
            format!("{}g.json", &HEX[1..]),
            format!("{}ä.json", &HEX[2..]),
            format!("{HEX}.jso?"),
            format!("../{}.json", &HEX[3..]),
            format!("{ORIGIN}{HEX}.json"),
            format!("/{file}"),
            format!("{file}?x=1"),
            format!(" {file}"),
            format!("{file}\0"),
            file.replace(".json", "/json"),
        ];
        for name in names {
            assert_eq!(pinned(&name), Err(PresetError::InvalidName), "{name:?}");
        }
    }

    #[test]
    fn verifies_content_against_the_pin() {
        // SHA-256("abc")
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256_hex(b"abc"), abc);
        assert_eq!(verify(abc, b"abc"), Ok(()));
        assert_eq!(verify(&abc.to_uppercase(), b"abc"), Ok(()));
        assert_eq!(verify(abc, b"abd"), Err(PresetError::Changed));
        assert_eq!(verify(abc, b"abc "), Err(PresetError::Changed));
        assert_eq!(verify("", b""), Err(PresetError::Changed));
    }

    #[test]
    fn pins_the_decoded_content() {
        let body = br#"{"baseVals":{"decay":0.9}}"#;
        let pin = sha256_hex(body);
        let text = String::from_utf8(body.to_vec()).unwrap();
        let gzipped = gzip(body);

        assert_eq!(
            verified_text(&pin, Some("gzip"), gzipped.clone()),
            Ok(text.clone())
        );
        assert_eq!(
            verified_text(&pin, Some(" GZIP "), gzipped.clone()),
            Ok(text.clone())
        );
        assert_eq!(verified_text(&pin, None, body.to_vec()), Ok(text.clone()));
        assert_eq!(
            verified_text(&pin, Some("identity"), body.to_vec()),
            Ok(text)
        );
        // The transfer bytes are not what is pinned.
        assert_eq!(
            verified_text(&sha256_hex(&gzipped), Some("gzip"), gzipped.clone()),
            Err(PresetError::Changed)
        );
        // Changed content, however it is encoded.
        let other = gzip(br#"{"baseVals":{"decay":0.1}}"#);
        assert_eq!(
            verified_text(&pin, Some("gzip"), other),
            Err(PresetError::Changed)
        );
        assert_eq!(
            verified_text(&pin, Some("br"), body.to_vec()),
            Err(PresetError::Undecodable)
        );
        assert_eq!(
            verified_text(&pin, Some("gzip"), body.to_vec()),
            Err(PresetError::Undecodable)
        );
        let mut truncated = gzipped;
        truncated.truncate(truncated.len() - 4);
        assert_eq!(
            verified_text(&pin, Some("gzip"), truncated),
            Err(PresetError::Undecodable)
        );
    }

    #[test]
    fn caps_the_decoded_size() {
        let limit = vec![b' '; MAX_PRESET_BYTES];
        assert_eq!(
            decode(Some("gzip"), gzip(&limit)).map(|v| v.len()),
            Ok(MAX_PRESET_BYTES)
        );
        let over = vec![b' '; MAX_PRESET_BYTES + 1];
        assert_eq!(
            decode(Some("gzip"), gzip(&over)),
            Err(PresetError::TooLarge)
        );
    }

    #[test]
    fn refuses_pinned_bytes_that_are_not_utf8() {
        let bytes = vec![b'{', 0xff, b'}'];
        let pin = sha256_hex(&bytes);
        assert_eq!(
            verified_text(&pin, None, bytes),
            Err(PresetError::Undecodable)
        );
    }

    #[test]
    fn error_codes_are_stable() {
        // `presets.ts` parses exactly these strings.
        let wire = |error: PresetError| AppError::from(error).to_string();
        assert_eq!(wire(PresetError::InvalidName), "preset:invalid-name");
        assert_eq!(wire(PresetError::NotListed), "preset:not-listed");
        assert_eq!(wire(PresetError::Changed), "preset:changed");
        assert_eq!(wire(PresetError::Http(404)), "preset:http:404");
        assert_eq!(wire(PresetError::TooLarge), "preset:too-large");
        assert_eq!(wire(PresetError::Undecodable), "preset:undecodable");
        assert_eq!(
            wire(PresetError::Network("timed out".into())),
            "preset:network:timed out"
        );
    }
}
