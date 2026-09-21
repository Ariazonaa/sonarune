use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};
use tracing_subscriber::fmt::MakeWriter;

const MAX_LOG_LINES: usize = 500;
const MAX_LINE_BYTES: usize = 2_000;

static LOGS: OnceLock<Arc<Mutex<VecDeque<String>>>> = OnceLock::new();

fn logs() -> &'static Arc<Mutex<VecDeque<String>>> {
    LOGS.get_or_init(|| Arc::new(Mutex::new(VecDeque::with_capacity(MAX_LOG_LINES))))
}

#[derive(Clone, Copy)]
pub struct DiagnosticMakeWriter;

pub fn writer() -> DiagnosticMakeWriter {
    let _ = logs();
    DiagnosticMakeWriter
}

pub fn recent_logs() -> Vec<String> {
    logs().lock().unwrap().iter().cloned().collect()
}

impl<'a> MakeWriter<'a> for DiagnosticMakeWriter {
    type Writer = DiagnosticWriter;

    fn make_writer(&'a self) -> Self::Writer {
        DiagnosticWriter { buffer: Vec::new() }
    }
}

pub struct DiagnosticWriter {
    buffer: Vec<u8>,
}

impl Write for DiagnosticWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(bytes);
        let remaining = MAX_LINE_BYTES.saturating_sub(self.buffer.len());
        self.buffer
            .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

impl Drop for DiagnosticWriter {
    fn drop(&mut self) {
        let line = String::from_utf8_lossy(&self.buffer);
        for line in line.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let mut logs = logs().lock().unwrap();
            if logs.len() == MAX_LOG_LINES {
                logs.pop_front();
            }
            logs.push_back(redact_log_line(line));
        }
    }
}

/// Logs are sanitized before they enter the in-memory ring. Entries containing
/// a URL, credential marker, or absolute path are replaced wholesale so a
/// partially parsed value can never leak into a diagnostic export.
fn redact_log_line(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    let sensitive = [
        "http://",
        "https://",
        "authorization",
        "password",
        "token",
        "client_secret",
        "secret=",
        "secret:",
        "credential",
        "api_key",
        "apikey",
        "access_token",
        "\\\\",
        "/home/",
        "/users/",
        "/mnt/",
        "/media/",
        "/tmp/",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || contains_windows_path(line)
        || contains_long_hex_identifier(line);
    if sensitive {
        return "[redacted sensitive log entry]".to_string();
    }

    redact_quoted_values(line)
}

/// Dynamic values in log lines (device names, server messages, parse errors)
/// are quoted. Keep the diagnostic context while replacing each quoted value
/// with `[value]`.
///
/// Quotes cannot be paired naively: values carry apostrophes ("John's
/// AirPods", "Guns N' Roses") and so does the prose around them ("can't
/// open"). The rules, all erring towards redacting too much rather than
/// leaking a tail:
/// - A quote *opens* a value at a word boundary (line start, whitespace, or
///   one of `( [ { : = ,`). A `"` inside a word opens one too — double quotes
///   are not prose. A `'` inside a word is an apostrophe and stays text.
/// - A quote can *close* the value only when it is not escaped (`\"`) and is
///   followed by a boundary (line end, whitespace, closing bracket, or
///   punctuation). Of those candidates the first is taken whose remainder does
///   not run into a stray closer — a quote right after a word and before a
///   boundary, i.e. the shape of the value's real end — before the next
///   opening quote: `'Guns N' Roses'` does not end after `N'`.
/// - A value that never closes is redacted to the end of the line.
///
/// Escaped backslashes need no handling: a line containing `\\` is already
/// dropped wholesale as a possible path.
fn redact_quoted_values(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();

    // clean[i]: the text from `i` on (read outside any value) reaches an
    // opening quote or the end before it reaches a stray closer.
    let mut clean = vec![true; len + 1];
    for i in (0..len).rev() {
        clean[i] = match quote_role(&chars, i) {
            Some(QuoteRole::Open) => true,
            Some(QuoteRole::StrayClose) => false,
            Some(QuoteRole::Apostrophe) | None => clean[i + 1],
        };
    }

    let mut output = String::with_capacity(line.len());
    let mut i = 0;
    while i < len {
        if quote_role(&chars, i) != Some(QuoteRole::Open) {
            output.push(chars[i]);
            i += 1;
            continue;
        }
        output.push_str("[value]");
        let quote = chars[i];
        let close = (i + 1..len).find(|&j| {
            chars[j] == quote
                && chars[j - 1] != '\\'
                && is_boundary_after(chars.get(j + 1).copied())
                && clean[j + 1]
        });
        match close {
            Some(j) => i = j + 1,
            // Unbalanced: nothing after the opening quote can be trusted.
            None => return output,
        }
    }
    output
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QuoteRole {
    Open,
    StrayClose,
    Apostrophe,
}

/// What the character at `i` is when met outside a quoted value; `None` for
/// anything but a quote.
fn quote_role(chars: &[char], i: usize) -> Option<QuoteRole> {
    let ch = chars[i];
    if ch != '\'' && ch != '"' {
        return None;
    }
    let before = i.checked_sub(1).map(|p| chars[p]);
    let after = chars.get(i + 1).copied();
    Some(if is_boundary_before(before) {
        QuoteRole::Open
    } else if is_boundary_after(after) {
        QuoteRole::StrayClose
    } else if ch == '"' {
        QuoteRole::Open
    } else {
        QuoteRole::Apostrophe
    })
}

fn is_boundary_before(ch: Option<char>) -> bool {
    ch.is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | '{' | ':' | '=' | ','))
}

fn is_boundary_after(ch: Option<char>) -> bool {
    ch.is_none_or(|c| {
        c.is_whitespace() || matches!(c, ')' | ']' | '}' | ':' | ';' | ',' | '.' | '!' | '?')
    })
}

fn contains_windows_path(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes
        .windows(3)
        .any(|window| window[0].is_ascii_alphabetic() && window[1] == b':' && window[2] == b'\\')
}

fn contains_long_hex_identifier(line: &str) -> bool {
    let mut run = 0usize;
    for byte in line.bytes() {
        if byte.is_ascii_hexdigit() {
            run += 1;
            if run >= 24 {
                return true;
            }
        } else if byte != b'-' || run < 8 {
            run = 0;
        }
    }
    false
}

pub fn os_version() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

        // `GetVersionEx` is capped by the compatibility shim: without a
        // manifest declaring the supportedOS GUIDs it reports 6.2.9200 on
        // every Windows since 8.1, and the manifest tauri-build embeds carries
        // no <compatibility> block. A diagnostics export exists to be accurate,
        // so ask ntdll instead — `RtlGetVersion` is not shimmed.
        #[link(name = "ntdll")]
        extern "system" {
            fn RtlGetVersion(info: *mut OSVERSIONINFOW) -> i32;
        }

        let mut info = OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        // SAFETY: `info` is initialized, correctly sized, and exclusively
        // borrowed for the duration of the system call.
        if unsafe { RtlGetVersion(&mut info) } == 0 {
            return Some(format!(
                "{}.{}.{}",
                info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
            ));
        }
    }

    #[cfg(target_os = "linux")]
    {
        let release = std::fs::read_to_string("/etc/os-release").ok()?;
        return release.lines().find_map(|line| {
            line.strip_prefix("PRETTY_NAME=")
                .map(|value| value.trim_matches('"').to_string())
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::redact_log_line;

    #[test]
    fn redacts_urls_credentials_and_paths_wholesale() {
        for line in [
            "request failed https://server/Audio/1?api_key=secret",
            "Authorization Token=secret",
            r"write failed C:\Music\Artist\song.flac",
            r"open failed \\nas\music\song.flac",
            "open failed /home/user/Music/song.flac",
            "cover failed for 0123456789abcdef0123456789abcdef",
        ] {
            assert_eq!(redact_log_line(line), "[redacted sensitive log entry]");
        }
    }

    #[test]
    fn redacts_quoted_dynamic_values_but_keeps_context() {
        assert_eq!(
            redact_log_line("failed to start 'Secret Song': decoder error"),
            "failed to start [value]: decoder error"
        );
    }

    #[test]
    fn apostrophes_inside_quoted_values_do_not_leak_the_tail() {
        for (line, expected) in [
            (
                "configured output device failed (audio error: output device 'John's AirPods' not found); using default",
                "configured output device failed (audio error: output device [value] not found); using default",
            ),
            ("failed for 'John's AirPods'", "failed for [value]"),
            (
                "invalid type: string \"Don't Stop\", expected u32",
                "invalid type: string [value], expected u32",
            ),
            ("cannot play 'Don't Stop': gone", "cannot play [value]: gone"),
            // An apostrophe before a space looks like a closing quote.
            ("seed 'Guns N' Roses' failed", "seed [value] failed"),
            ("seed 'Rock 'n' Roll' failed", "seed [value] failed"),
            // Nested and escaped double quotes.
            ("device \"My \"Best\" Speaker\" lost", "device [value] lost"),
            ("field \"John \\\"JJ\\\" Smith\" missing", "field [value] missing"),
            // A plural possessive after a value is ambiguous: redact more, never less.
            ("'Mine' for the users' devices", "[value] devices"),
        ] {
            assert_eq!(redact_log_line(line), expected, "line: {line}");
        }
    }

    #[test]
    fn several_quoted_values_keep_the_context_between_them() {
        assert_eq!(
            redact_log_line("device 'A' failed, fallback \"B\" used"),
            "device [value] failed, fallback [value] used"
        );
        assert_eq!(
            redact_log_line("server returned 400: {\"title\":\"Bad Request\",\"status\":400}"),
            "server returned 400: {[value]:[value],[value]:400}"
        );
    }

    #[test]
    fn unbalanced_quotes_redact_the_rest_of_the_line() {
        for (line, expected) in [
            (
                "output device 'John's AirPods not found",
                "output device [value]",
            ),
            ("cannot play \"Don't Stop", "cannot play [value]"),
            ("start 'A' then '90s mix", "start [value] then [value]"),
            ("empty '' value", "empty [value] value"),
        ] {
            assert_eq!(redact_log_line(line), expected, "line: {line}");
        }
    }

    #[test]
    fn apostrophes_in_prose_do_not_swallow_the_message() {
        for line in [
            "can't open the output, it isn't available",
            "decoder doesn't support this stream",
            "the users' devices weren't listed",
        ] {
            assert_eq!(redact_log_line(line), line);
        }
        assert_eq!(
            redact_log_line("can't open 'Kitchen Speaker': it isn't there"),
            "can't open [value]: it isn't there"
        );
    }

    /// The shimmed `GetVersionEx` reports 6.2.9200 on every Windows since 8.1
    /// unless the binary carries a supportedOS manifest — which tauri-build's
    /// default manifest does not. This asserts we are on the unshimmed path.
    /// (Any machine that can run a Tauri 2 / WebView2 app is Windows 10+.)
    #[cfg(target_os = "windows")]
    #[test]
    fn os_version_reports_the_real_build_not_the_compatibility_shim() {
        let version = super::os_version().expect("windows always reports a version");
        let major: u32 = version
            .split('.')
            .next()
            .and_then(|m| m.parse().ok())
            .unwrap_or(0);
        assert!(
            major >= 10,
            "got {version} — this looks like the shimmed GetVersionEx value"
        );
    }
}
