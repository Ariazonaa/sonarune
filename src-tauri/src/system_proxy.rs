//! Which proxy the server clients go through.
//!
//! reqwest takes the Windows proxy settings from the registry (through
//! hyper-util), but reads their bypass list differently from Windows itself:
//! `<local>` and wildcards in addresses (`10.*`, `172.16.*`, `127.*`) never
//! match there, and loopback is not exempt. A server on the LAN or on this
//! machine — the usual Jellyfin setup — is then asked for through the proxy,
//! which a remote proxy cannot reach, and for an `http://` server the token
//! and, at sign-in, the password travel to it in the clear. So on Windows the
//! list is also read the way WinINet (and with it the browser) reads it:
//! loopback always goes direct unless the list says `<-loopback>`, `<local>`
//! covers host names without a dot, and `*` matches anywhere in an entry.
//!
//! Only the bypass changes, and it only grows. A request goes through the
//! proxy only when reqwest's own reading of the same settings — hyper-util's
//! matcher, built here from the same values — sends it there too, so nothing
//! that went around the proxy before is sent through it now: networks such as
//! `10.0.0.0/8`, names with a leading dot, a name's subdomains. The proxy is
//! the one reqwest would pick: the registry's `ProxyServer` for both schemes,
//! when it names a single HTTP proxy. Everything else — proxy environment
//! variables, a per-scheme `ProxyServer`, SOCKS, a value reqwest cannot read,
//! PAC scripts, the other platforms — stays with reqwest's own handling.

// Only Windows reads its settings; elsewhere the parsing is left to the tests.
#![cfg_attr(not(windows), allow(dead_code))]

use std::ffi::OsString;

use hyper_util::client::proxy::matcher::Matcher;
use reqwest::Url;
use tauri::http::Uri;

/// Route `builder`'s requests by the Windows proxy settings, where they apply.
pub fn apply(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    match configured() {
        Some(policy) => builder.proxy(policy.into_proxy()),
        None => builder,
    }
}

#[cfg(windows)]
fn configured() -> Option<Policy> {
    if !registry_applies(|name| std::env::var_os(name)) {
        return None;
    }
    let settings = windows_registry::CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .ok()?;
    if settings.get_u32("ProxyEnable").unwrap_or(0) == 0 {
        return None;
    }
    Policy::new(
        &settings.get_string("ProxyServer").ok()?,
        &settings.get_string("ProxyOverride").unwrap_or_default(),
    )
}

#[cfg(not(windows))]
fn configured() -> Option<Policy> {
    None
}

/// Whether reqwest takes its proxy from the registry, given the process
/// environment `var`. Proxy environment variables win over the registry in
/// reqwest's handling, which they keep; and in a CGI environment
/// (`REQUEST_METHOD` set, whatever its value) reqwest uses no proxy at all.
fn registry_applies(var: impl Fn(&str) -> Option<OsString>) -> bool {
    const ENV: [&str; 8] = [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ];
    var("REQUEST_METHOD").is_none()
        && !ENV
            .iter()
            .any(|name| var(name).is_some_and(|value| !value.is_empty()))
}

/// A proxy and the destinations that go around it.
struct Policy {
    proxy: Url,
    /// The bypass list as Windows reads it.
    bypass: Bypass,
    /// reqwest's own reading of the same settings. What it sends direct goes
    /// direct.
    stock: Matcher,
}

impl Policy {
    /// The policy for a `ProxyServer` and a `ProxyOverride` value; `None`
    /// when the server is not one HTTP proxy reqwest would use.
    fn new(server: &str, list: &str) -> Option<Self> {
        Some(Policy {
            proxy: proxy_server(server)?,
            bypass: Bypass::parse(list),
            stock: Matcher::builder()
                .http(server)
                .https(server)
                .no(stock_bypass(list))
                .build(),
        })
    }

    /// Where a request for `url` goes; `None` is direct.
    fn route(&self, url: &Url) -> Option<Url> {
        let uri: Uri = url.as_str().parse().ok()?;
        self.stock.intercept(&uri)?;
        (!self.bypass.matches(url)).then(|| self.proxy.clone())
    }

    fn into_proxy(self) -> reqwest::Proxy {
        reqwest::Proxy::custom(move |url| self.route(url))
    }
}

/// `ProxyOverride` as hyper-util passes it to its matcher on reqwest's
/// behalf (its `win::with_system`): comma-separated, every `*.` dropped.
fn stock_bypass(list: &str) -> String {
    list.split(';')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(",")
        .replace("*.", "")
}

/// The proxy a `ProxyServer` value names when it is one HTTP proxy for every
/// scheme — `host:port` or `http://host:port`, read the way reqwest reads it:
/// as a URI, exactly as it stands, a missing scheme meaning http. `None` for
/// the per-scheme form (`http=…;https=…`), for SOCKS and for what reqwest
/// cannot read, which all keep reqwest's handling.
fn proxy_server(value: &str) -> Option<Url> {
    if value.contains(['=', ';']) {
        return None;
    }
    let uri: Uri = value.parse().ok()?;
    let scheme = match uri.scheme_str() {
        None => "http",
        Some(scheme @ ("http" | "https")) => scheme,
        Some(_) => return None,
    };
    Url::parse(&format!("{scheme}://{}", uri.authority()?)).ok()
}

/// The `ProxyOverride` list.
#[derive(Debug, Default, PartialEq)]
struct Bypass {
    /// `localhost`, 127.0.0.0/8 and `::1` go direct unless `<-loopback>`.
    loopback: bool,
    /// `<local>`: host names without a dot go direct.
    local: bool,
    entries: Vec<Entry>,
}

/// One entry of the list: `[scheme://]host[:port]`, where the host may hold
/// `*` wildcards.
#[derive(Debug, PartialEq)]
struct Entry {
    scheme: Option<String>,
    /// Lowercase, IPv6 addresses without their brackets.
    host: String,
    port: Option<u16>,
}

impl Bypass {
    fn parse(list: &str) -> Self {
        let mut bypass = Bypass {
            loopback: true,
            ..Bypass::default()
        };
        for raw in list.split(|c: char| c == ';' || c == ',' || c.is_whitespace()) {
            match raw.to_ascii_lowercase().as_str() {
                "" => {}
                "<local>" => bypass.local = true,
                "<-loopback>" => bypass.loopback = false,
                entry => bypass.entries.extend(Entry::parse(entry)),
            }
        }
        bypass
    }

    fn matches(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase();
        let ip = host.parse::<std::net::IpAddr>().ok();
        if self.loopback && (host == "localhost" || ip.is_some_and(|ip| ip.is_loopback())) {
            return true;
        }
        if self.local && ip.is_none() && !host.contains('.') {
            return true;
        }
        let port = url.port_or_known_default();
        self.entries
            .iter()
            .any(|entry| entry.matches(url.scheme(), &host, port))
    }
}

impl Entry {
    /// `raw` is lowercase. `None` for what cannot be an entry (a port that
    /// is not a number, an unclosed bracket).
    fn parse(raw: &str) -> Option<Self> {
        let (scheme, rest) = match raw.split_once("://") {
            Some((scheme, rest)) => (Some(scheme.to_string()), rest),
            None => (None, raw),
        };
        let rest = rest.trim_end_matches('/');
        let (host, port) = if let Some(bracketed) = rest.strip_prefix('[') {
            let (host, tail) = bracketed.split_once(']')?;
            let port = match tail {
                "" => None,
                tail => Some(tail.strip_prefix(':')?.parse().ok()?),
            };
            (host, port)
        } else if rest.matches(':').count() == 1 {
            let (host, port) = rest.split_once(':')?;
            (host, Some(port.parse().ok()?))
        } else {
            // A bare host, or an IPv6 address without brackets.
            (rest, None)
        };
        (!host.is_empty()).then(|| Entry {
            scheme,
            host: host.to_string(),
            port,
        })
    }

    fn matches(&self, scheme: &str, host: &str, port: Option<u16>) -> bool {
        if self.scheme.as_deref().is_some_and(|s| s != scheme)
            || self.port.is_some_and(|p| Some(p) != port)
        {
            return false;
        }
        glob(&self.host, host)
    }
}

/// `*` matches any run of characters, none included; everything else only
/// itself.
fn glob(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(rest) = text.strip_prefix(parts.next().unwrap_or("")) else {
        return false;
    };
    let mut middle: Vec<&str> = parts.collect();
    let Some(last) = middle.pop() else {
        // No wildcard: the whole text.
        return rest.is_empty();
    };
    let mut rest = rest;
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::{glob, proxy_server, registry_applies, Bypass, Policy};
    use reqwest::Url;
    use std::ffi::OsString;

    /// The bypass list as Windows reads it.
    fn bypassed(list: &str, url: &str) -> bool {
        Bypass::parse(list).matches(&Url::parse(url).unwrap())
    }

    /// Whether the whole policy sends `url` around a proxy with this list.
    fn direct(list: &str, url: &str) -> bool {
        Policy::new("proxy.corp:8080", list)
            .unwrap()
            .route(&Url::parse(url).unwrap())
            .is_none()
    }

    fn applies(env: &[(&str, &str)]) -> bool {
        registry_applies(|name| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        })
    }

    #[test]
    fn a_single_proxy_server_is_used_for_both_schemes() {
        assert_eq!(
            proxy_server("proxy.corp:8080").map(String::from),
            Some("http://proxy.corp:8080/".to_string())
        );
        assert_eq!(
            proxy_server("http://127.0.0.1:7890").map(String::from),
            Some("http://127.0.0.1:7890/".to_string())
        );
        assert!(proxy_server("https://proxy.corp:443").is_some());
        // Forms reqwest makes nothing of keep its handling.
        assert_eq!(proxy_server("http=proxy:80;https=proxy:443"), None);
        assert_eq!(proxy_server("socks=proxy:1080"), None);
        assert_eq!(proxy_server("socks5://proxy:1080"), None);
        assert_eq!(proxy_server(""), None);
        // reqwest reads the value as it stands, and with blanks around it or
        // a path after a bare host it uses no proxy at all; neither do we.
        assert_eq!(proxy_server(" http://127.0.0.1:7890 "), None);
        assert_eq!(proxy_server("proxy.corp:8080/"), None);
        assert!(Policy::new(" proxy.corp:8080", "").is_none());
    }

    #[test]
    fn the_registry_counts_only_where_reqwest_reads_it() {
        assert!(applies(&[]));
        assert!(applies(&[("HTTPS_PROXY", "")]));
        assert!(!applies(&[("https_proxy", "http://proxy.corp:8080")]));
        assert!(!applies(&[("NO_PROXY", "localhost")]));
        // In a CGI environment reqwest uses no proxy, whatever the value.
        assert!(!applies(&[("REQUEST_METHOD", "")]));
        assert!(!applies(&[("REQUEST_METHOD", "GET")]));
    }

    #[test]
    fn loopback_goes_direct_unless_the_list_says_otherwise() {
        for url in [
            "http://127.0.0.1:8096",
            "http://127.1.2.3:8096",
            "http://localhost:8096",
            "http://[::1]:8096",
        ] {
            assert!(bypassed("", url), "{url}");
            assert!(!bypassed("<-loopback>", url), "{url}");
        }
        assert!(!bypassed("", "http://10.0.0.5:8096"));
        assert!(!bypassed("", "http://nas:8096"));
    }

    #[test]
    fn local_covers_names_without_a_dot() {
        assert!(bypassed("<local>", "http://nas:8096"));
        assert!(bypassed("<LOCAL>", "http://NAS:8096"));
        assert!(!bypassed("<local>", "http://nas.local:8096"));
        assert!(!bypassed("<local>", "http://10.0.0.5:8096"));
        assert!(!bypassed("<local>", "http://[fe80::1]:8096"));
    }

    #[test]
    fn wildcards_match_addresses_as_windows_does() {
        // The kind of list Windows proxy tools write by default.
        let list = "localhost;127.*;10.*;172.16.*;169.254.*;<local>";
        assert!(bypassed(list, "http://10.0.0.5:8096"));
        assert!(bypassed(list, "http://172.16.4.2:8096"));
        assert!(bypassed(list, "http://169.254.7.1:8096"));
        assert!(bypassed(list, "http://nas:8096"));
        assert!(!bypassed(list, "http://172.20.1.1:8096"));
        assert!(!bypassed(list, "https://music.example.com"));
        assert!(!bypassed(list, "http://100.10.1.1:8096"));
    }

    #[test]
    fn host_entries_never_send_more_through_the_proxy_than_before() {
        // reqwest's own reading lets a name cover its subdomains, and
        // `*.name` the name itself. Windows does not, but what went direct
        // stays direct.
        assert!(direct("*.example.com", "https://music.example.com"));
        assert!(direct("*.example.com", "https://example.com"));
        assert!(direct("example.com", "https://www.example.com"));
        assert!(direct("Example.COM", "https://example.com"));
        assert!(!direct("example.com", "https://notexample.com"));
        assert!(!direct("*.example.com", "https://example.org"));
    }

    #[test]
    fn whatever_reqwest_sent_direct_still_goes_direct() {
        // Networks.
        assert!(direct("10.0.0.0/8;<local>", "http://10.1.2.3:8096"));
        assert!(direct("010.0.0.0/8", "http://10.1.2.3:8096"));
        assert!(direct("fd00::/8", "http://[fd12::5]:8096"));
        // Addresses compare as addresses, however they are written.
        assert!(direct("fd12:0:0:0:0:0:0:5", "http://[fd12::5]:8096"));
        // A leading dot: the name and everything below it.
        assert!(direct(".home.example", "http://music.home.example:8096"));
        assert!(direct(".home.example", "http://home.example:8096"));
        // `*.` is dropped wherever it stands.
        assert!(direct("foo.*.example.com", "http://foo.example.com"));
        assert!(direct("foo.*.example.com", "http://x.foo.example.com"));
        assert!(direct("*.*", "http://nas:8096"));
        // On top of it, the Windows reading.
        assert!(direct("", "http://127.0.0.1:8096"));
        assert!(direct("<local>", "http://nas:8096"));
        assert!(direct("10.*", "http://10.0.0.5:8096"));
        // What neither reading exempts still goes through the proxy.
        let list = "10.0.0.0/8;fd00::/8;.home.example";
        for url in [
            "http://11.1.2.3:8096",
            "http://[2001:db8::5]:8096",
            "https://home.example.org",
            "https://music.example.com",
        ] {
            assert_eq!(
                Policy::new("proxy.corp:8080", list)
                    .unwrap()
                    .route(&Url::parse(url).unwrap())
                    .map(String::from),
                Some("http://proxy.corp:8080/".to_string()),
                "{url}"
            );
        }
    }

    #[test]
    fn scheme_and_port_narrow_an_entry() {
        assert!(bypassed("https://secure.home", "https://secure.home"));
        assert!(!bypassed("https://secure.home", "http://secure.home"));
        assert!(bypassed("nas.home:8096", "http://nas.home:8096"));
        assert!(!bypassed("nas.home:8096", "http://nas.home:8920"));
        assert!(bypassed("[fe80::1]", "http://[fe80::1]:8096"));
        assert!(bypassed("[fe80::1]:8096", "http://[fe80::1]:8096"));
        // Nothing that cannot be an entry turns into one.
        assert_eq!(Bypass::parse("nas.home:port").entries, Vec::new());
        assert_eq!(Bypass::parse("[fe80::1").entries, Vec::new());
    }

    #[test]
    fn glob_matches_runs_of_any_length() {
        assert!(glob("172.16.*", "172.16.1.10"));
        assert!(glob("*", "anything"));
        assert!(glob("a*b*c", "abc"));
        assert!(glob("a*b*c", "a-b-b-c"));
        assert!(!glob("a*b*c", "a-c"));
        assert!(!glob("a*bc", "abc-"));
        assert!(glob("exact", "exact"));
        assert!(!glob("exact", "exactly"));
    }

    #[test]
    fn a_client_builds_with_the_policy() {
        let policy = Policy::new("proxy.corp:8080", "<local>").unwrap();
        let client = reqwest::Client::builder()
            .proxy(policy.into_proxy())
            .build();
        assert!(client.is_ok());
    }
}
