//! Removing the saved credentials when the app is uninstalled.
//!
//! The access token, the password and the ListenBrainz token live in the
//! Windows Credential Manager, outside the two data folders the uninstaller's
//! "delete app data" box removes. With that box ticked, and never during an
//! update, `installer/hooks.nsh` runs `sonarune.exe --purge-credentials` before
//! the program files go. `run()` in `lib.rs` handles the switch before the data
//! migration, the single-instance plugin (which would hand it over to a running
//! copy) and any window, so it works whether or not the app is running, and
//! the process ends with one of the `EXIT_*` codes.
//!
//! keyring names a credential `<user>.<service>`, with the entry names from
//! `commands.rs` as the user: `<server>::<username>` (the token),
//! `<server>::<username>::password` and `listenbrainz`. The installed app's
//! service is `sonarune`, and a Jellysic install may have left entries under
//! `jellysic`; a demo or test build has a service of its own
//! (`commands::keyring_service_for`) and removes only that one. Nothing else
//! in the user's Credential Manager is touched: only generic credentials whose
//! whole name has one of these shapes, with an `http(s)` server address in
//! front, are deleted, and only their names are looked at. Every such name
//! starts with `http` or `listenbrainz.`, so only those are listed at all
//! (`LISTING_FILTERS`): what Windows returns with a listing includes the
//! secrets, and the others never need to enter the process.

// Elsewhere only the switch itself is compiled in.
#![cfg_attr(not(windows), allow(dead_code))]

use std::ffi::OsString;

/// The switch the uninstaller passes.
pub const FLAG: &str = "--purge-credentials";

/// Every entry of this build is gone, or there was none.
pub const EXIT_OK: i32 = 0;
/// The Credential Manager could not be listed, or an entry not deleted.
pub const EXIT_FAILED: i32 = 1;
/// Not on Windows: the switch exists for the NSIS uninstaller only.
#[cfg(not(windows))]
pub const EXIT_UNSUPPORTED: i32 = 2;

/// CredEnumerateW filters (a name prefix and `*`; Windows compares them
/// without regard to case) that between them list every name
/// [`is_own_target`] accepts: the server address in front, or the
/// ListenBrainz entry and its separator.
const LISTING_FILTERS: [&str; 2] = ["http*", "listenbrainz.*"];

/// Whether the process was started to purge (the switch as first argument).
pub fn requested(args: impl IntoIterator<Item = OsString>) -> bool {
    args.into_iter().nth(1).is_some_and(|arg| arg == FLAG)
}

/// Delete this build's saved credentials; returns the process exit code.
pub fn run(identifier: &str) -> i32 {
    #[cfg(windows)]
    {
        let services = services(identifier);
        let mut targets = Vec::new();
        for filter in LISTING_FILTERS {
            match imp::generic_targets(filter) {
                Ok(listed) => targets.extend(listed),
                Err(code) => {
                    tracing::warn!("could not list the saved credentials: Windows error {code}");
                    return EXIT_FAILED;
                }
            }
        }
        let outcome = purge(&targets, &services, |target| {
            imp::delete_generic(target)
                .map_err(|code| {
                    tracing::warn!("could not delete a saved credential: Windows error {code}")
                })
                .is_ok()
        });
        tracing::info!(
            "removed {} of {} saved credentials",
            outcome.deleted,
            outcome.matched
        );
        if outcome.deleted == outcome.matched {
            EXIT_OK
        } else {
            EXIT_FAILED
        }
    }
    #[cfg(not(windows))]
    {
        let _ = identifier;
        tracing::warn!("{FLAG} is only supported on Windows");
        EXIT_UNSUPPORTED
    }
}

/// The keyring services whose entries belong to a build with `identifier`.
/// Only the real app has a claim on what the old name left behind, as in
/// `legacy.rs`.
fn services(identifier: &str) -> Vec<&str> {
    let mut services = vec![crate::commands::keyring_service_for(identifier)];
    if identifier == crate::legacy::IDENTIFIER {
        services.push(crate::legacy::OLD_KEYRING_SERVICE);
    }
    services
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    matched: usize,
    deleted: usize,
}

/// Hand every target (NUL-terminated UTF-16, as Windows returned it) that
/// names one of the app's entries to `delete`, unchanged, which reports
/// whether it is gone. A name that is not valid UTF-16 was not written by the
/// app.
fn purge(
    targets: &[Vec<u16>],
    services: &[&str],
    mut delete: impl FnMut(&[u16]) -> bool,
) -> Outcome {
    let mut outcome = Outcome {
        matched: 0,
        deleted: 0,
    };
    for target in targets {
        let Some(name) = target.strip_suffix(&[0]) else {
            continue;
        };
        let Ok(name) = String::from_utf16(name) else {
            continue;
        };
        if !is_own_target(&name, services) {
            continue;
        }
        outcome.matched += 1;
        if delete(target) {
            outcome.deleted += 1;
        }
    }
    outcome
}

/// Whether `target` is the name keyring gives an entry the app writes under
/// one of `services`.
fn is_own_target(target: &str, services: &[&str]) -> bool {
    services.iter().any(|service| {
        target
            .strip_suffix(service)
            .and_then(|rest| rest.strip_suffix('.'))
            .is_some_and(is_own_entry)
    })
}

/// The ListenBrainz entry, or `<server>::<account>`: a server address the
/// way the app stores it, then a non-empty user name, which may carry the
/// `::password` suffix. An IPv6 host has `::` of its own, so every `::` is
/// tried as the separator.
fn is_own_entry(entry: &str) -> bool {
    entry == crate::commands::LISTENBRAINZ_ENTRY_NAME
        || entry.match_indices("::").any(|(at, sep)| {
            let (server, account) = (&entry[..at], &entry[at + sep.len()..]);
            !account.is_empty() && is_server_address(server)
        })
}

/// The test `commands::server_address` applies before anything is stored, on
/// an address it has trimmed and then keeps as typed: the URL parser also
/// takes `http:nas.local:8096` or `https:\\nas`, and up to 0.6.0 the address
/// was stored trimmed without any check. The `http*` listing relies only on
/// the scheme coming first, which trimming ensures (the parser alone would
/// also skip a leading control character or a tab inside the scheme).
fn is_server_address(server: &str) -> bool {
    server
        .get(..4)
        .is_some_and(|start| start.eq_ignore_ascii_case("http"))
        && reqwest::Url::parse(server)
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.has_host())
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND};
    use windows_sys::Win32::Security::Credentials::{
        CredDeleteW, CredEnumerateW, CredFree, CREDENTIALW, CRED_TYPE_GENERIC,
    };

    /// The names of the user's generic credentials that start with `filter`
    /// (CredEnumerateW's: a name prefix followed by `*`), NUL-terminated,
    /// exactly as Windows returned them.
    pub fn generic_targets(filter: &str) -> Result<Vec<Vec<u16>>, u32> {
        debug_assert!(filter.ends_with('*') && filter.len() > 1);
        let filter: Vec<u16> = filter.encode_utf16().chain(Some(0)).collect();
        let mut count = 0u32;
        let mut creds: *mut *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: the filter is a NUL-terminated string that outlives the
        // call; both out-pointers are valid for writes.
        if unsafe { CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut creds) } == 0 {
            // SAFETY: no preconditions; read right after the failed call.
            let error = unsafe { GetLastError() };
            return if error == ERROR_NOT_FOUND {
                Ok(Vec::new())
            } else {
                Err(error)
            };
        }
        let mut targets = Vec::new();
        // SAFETY: on success `creds` points to `count` valid credentials, all
        // in one buffer that stays alive until the CredFree below; their
        // strings are NUL-terminated.
        unsafe {
            for &cred in std::slice::from_raw_parts(creds, count as usize) {
                // Only the name is read. The secrets come along with the
                // listing, so they are cleared before the buffer goes back to
                // the heap.
                let blob = (*cred).CredentialBlob;
                if !blob.is_null() {
                    std::ptr::write_bytes(blob, 0, (*cred).CredentialBlobSize as usize);
                }
                let name = (*cred).TargetName;
                if (*cred).Type == CRED_TYPE_GENERIC && !name.is_null() {
                    let len = (0..).take_while(|&i| *name.add(i) != 0).count();
                    targets.push(std::slice::from_raw_parts(name, len + 1).to_vec());
                }
            }
            CredFree(creds.cast());
        }
        Ok(targets)
    }

    /// Delete one generic credential by its NUL-terminated name. One that is
    /// already gone counts as deleted.
    pub fn delete_generic(target: &[u16]) -> Result<(), u32> {
        debug_assert_eq!(target.last(), Some(&0));
        // SAFETY: `target` is a NUL-terminated UTF-16 string that outlives
        // the call.
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0 {
            return Ok(());
        }
        // SAFETY: no preconditions; read right after the failed call.
        match unsafe { GetLastError() } {
            ERROR_NOT_FOUND => Ok(()),
            error => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_own_target, purge, requested, services, Outcome, FLAG, LISTING_FILTERS};
    use std::ffi::OsString;

    const APP: &[&str] = &["sonarune", "jellysic"];

    fn wide(name: &str) -> Vec<u16> {
        name.encode_utf16().chain(Some(0)).collect()
    }

    /// Whether one of the listings returns `name`: CredEnumerateW gives the
    /// names that start with a filter's prefix, without regard to case (the
    /// `credential_manager` test checks that against Windows).
    fn listed(name: &str) -> bool {
        LISTING_FILTERS.iter().any(|filter| {
            let prefix = filter.strip_suffix('*').unwrap();
            name.get(..prefix.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
        })
    }

    #[test]
    fn the_switch_counts_only_as_the_first_argument() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(requested(args(&["sonarune.exe", FLAG])));
        assert!(!requested(args(&["sonarune.exe"])));
        assert!(!requested(args(&["sonarune.exe", "--minimized", FLAG])));
        assert!(!requested(args(&["sonarune.exe", "--purge-credentials=1"])));
        assert!(!requested(args(&[FLAG])));
    }

    #[test]
    fn the_installed_app_also_removes_what_jellysic_left() {
        assert_eq!(services(crate::legacy::IDENTIFIER), APP);
    }

    #[test]
    fn test_and_demo_builds_remove_only_their_own_service() {
        let e2e: serde_json::Value =
            serde_json::from_str(include_str!("../../e2e/tauri.e2e.conf.json")).unwrap();
        let e2e = e2e["identifier"].as_str().unwrap();
        for other in [e2e, "io.sonarune.demo"] {
            assert_eq!(services(other), [other]);
        }
    }

    #[test]
    fn the_apps_entries_are_recognised() {
        for target in [
            "https://jellyfin.example.org::alice.sonarune",
            "https://jellyfin.example.org::alice::password.sonarune",
            "listenbrainz.sonarune",
            // A server under a path, a port, an upper-case scheme as typed.
            "https://example.org/jellyfin::alice.sonarune",
            "http://203.0.113.20:8096::Bob Smith::password.sonarune",
            "HTTP://nas.local:8096::bob.sonarune",
            // An IPv6 host has its own `::`.
            "http://[::1]:8096::alice.sonarune",
            "http://[fe80::1]:8096::alice::password.sonarune",
            // User names may contain dots and colons.
            "https://h.example::first.last.sonarune",
            "https://h.example::a::b.sonarune",
            // What a Jellysic install left behind.
            "https://jellyfin.example.org::alice.jellysic",
            "https://jellyfin.example.org::alice::password.jellysic",
            "listenbrainz.jellysic",
            // Addresses the URL parser takes without `//`, which up to 0.6.0
            // were stored as typed, only trimmed.
            "http:nas.local:8096/jellyfin::alice.jellysic",
            "HTTP:/nas:8096::alice::password.sonarune",
            "https:\\\\nas\\jellyfin::bob.sonarune",
        ] {
            assert!(is_own_target(target, APP), "{target}");
            assert!(listed(target), "{target}");
        }
    }

    /// The names built the way `commands.rs` builds them when it saves (and
    /// `migrate_legacy_credentials` when it moves the Jellysic ones), from
    /// addresses as `server_address` stores them: a change there that the
    /// purge would not follow fails here.
    #[test]
    fn the_names_the_app_writes_are_recognised_and_listed() {
        use crate::commands::{
            keyring_service_for, password_entry_name, server_address, token_entry_name,
            LISTENBRAINZ_ENTRY_NAME,
        };
        let servers = [
            "https://jellyfin.example.org",
            "https://example.org/jellyfin/",
            " http://203.0.113.20:8096 ",
            "HTTP://nas.local:8096",
            "http://[::1]:8096",
            "https://[2001:db8::7]:8920/jf",
            // The URL parser takes these without `//`, and they are kept as
            // typed.
            "http:nas.local:8096",
            "HTTP:/nas:8096",
            "https:\\\\nas",
        ];
        // Up to 0.6.0 (and as Jellysic) the address was only trimmed, never
        // checked: `server_url.trim().trim_end_matches('/')`.
        let unchecked = [
            " https://nas.local:8920/jellyfin/web/ ",
            "http:nas.local:8096/jellyfin/",
        ]
        .map(|raw| raw.trim().trim_end_matches('/').to_owned());
        let users = ["alice", "Bob Smith", "first.last", "a::b", "Zo\u{eb}"];
        let mut names = vec![LISTENBRAINZ_ENTRY_NAME.to_owned()];
        let stored = servers
            .iter()
            .map(|server| server_address(server).unwrap())
            .chain(unchecked);
        for server in stored {
            for user in users {
                names.push(token_entry_name(&server, user));
                names.push(password_entry_name(&server, user));
            }
        }
        let app = services(crate::legacy::IDENTIFIER);
        let e2e = [keyring_service_for("io.sonarune.e2e")];
        for name in &names {
            // keyring's `<user>.<service>`, checked by `credential_manager`.
            for service in &app {
                let target = format!("{name}.{service}");
                assert!(is_own_target(&target, &app), "{target}");
                assert!(listed(&target), "{target}");
                assert!(!is_own_target(&target, &e2e), "{target}");
            }
            let test_build = format!("{name}.{}", e2e[0]);
            assert!(is_own_target(&test_build, &e2e), "{test_build}");
            assert!(!is_own_target(&test_build, &app), "{test_build}");
        }

        // Neither a bare address nor one without a user name is an entry.
        let server = server_address("https://jellyfin.example.org").unwrap();
        for name in [server.clone(), token_entry_name(&server, "")] {
            assert!(!is_own_target(&format!("{name}.sonarune"), &app), "{name}");
        }
    }

    #[test]
    fn nothing_else_in_the_credential_manager_is_touched() {
        for target in [
            // Other programs' generic credentials.
            "git:https://github.com",
            "MicrosoftAccount:user=someone@example.com",
            "https://github.com::bob",
            "LegacyGeneric:target=sonarune",
            // Close, but not a name the app writes.
            "sonarune",
            ".sonarune",
            "listenbrainz",
            "listenbrainz.sonarune.bak",
            "xlistenbrainz.sonarune",
            "listenbrainz..sonarune",
            "alice.sonarune",
            "jellyfin.example.org::alice.sonarune",
            "https://jellyfin.example.org.sonarune",
            "https://jellyfin.example.org::.sonarune",
            "ftp://jellyfin.example.org::alice.sonarune",
            "https://::alice.sonarune",
            "https://jellyfin.example.org::alice.sonarunex",
            "https://jellyfin.example.org::alice.notsonarune",
            "https://jellyfin.example.org::alice.sonarune2",
            // `server_address` stores the address trimmed, scheme first.
            " https://jellyfin.example.org::alice.sonarune",
            "\u{1}https://jellyfin.example.org::alice.sonarune",
            "h\tttps://jellyfin.example.org::alice.sonarune",
            // The demo and e2e builds' own entries.
            "https://jellyfin.example.org::alice.io.sonarune.e2e",
            "https://jellyfin.example.org::alice::password.io.sonarune.demo",
            "listenbrainz.io.sonarune.e2e",
        ] {
            assert!(!is_own_target(target, APP), "{target}");
        }
    }

    #[test]
    fn a_test_build_leaves_the_installed_apps_entries_alone() {
        let e2e = ["io.sonarune.e2e"];
        assert!(is_own_target(
            "https://jellyfin.example.org::alice.io.sonarune.e2e",
            &e2e
        ));
        for target in [
            "https://jellyfin.example.org::alice.sonarune",
            "https://jellyfin.example.org::alice::password.sonarune",
            "listenbrainz.sonarune",
            "listenbrainz.jellysic",
        ] {
            assert!(!is_own_target(target, &e2e), "{target}");
        }
    }

    #[test]
    fn only_matching_names_are_deleted_exactly_as_listed() {
        let ours = wide("https://jellyfin.example.org::alice.sonarune");
        let password = wide("https://jellyfin.example.org::alice::password.sonarune");
        let foreign = wide("git:https://github.com");
        // Not valid UTF-16 (a lone surrogate), so not one of ours.
        let broken = {
            let mut name = wide("https://jellyfin.example.org::alice.sonarune");
            name.insert(0, 0xD800);
            name
        };
        // CredDeleteW needs the terminator.
        let unterminated: Vec<u16> = "https://jellyfin.example.org::bob.sonarune"
            .encode_utf16()
            .collect();
        let targets = vec![
            ours.clone(),
            foreign,
            broken,
            unterminated,
            password.clone(),
        ];
        let mut asked = Vec::new();
        let outcome = purge(&targets, APP, |target| {
            asked.push(target.to_vec());
            true
        });
        assert_eq!(asked, [ours, password]);
        assert_eq!(
            outcome,
            Outcome {
                matched: 2,
                deleted: 2
            }
        );
    }

    #[test]
    fn a_failed_deletion_is_counted() {
        let targets = vec![
            wide("listenbrainz.sonarune"),
            wide("https://h.example::alice.sonarune"),
        ];
        let outcome = purge(&targets, APP, |target| {
            target != wide("listenbrainz.sonarune")
        });
        assert_eq!(
            outcome,
            Outcome {
                matched: 2,
                deleted: 1
            }
        );
    }
}

/// Against the real Credential Manager, with entries of its own only: a
/// unique service, and names under unique prefixes that every listing is
/// filtered by, so no other credential is listed, read or deleted. Also
/// checks what `LISTING_FILTERS` relies on: a filter is a prefix compared
/// without regard to case. Run by hand:
/// `cargo test --lib credential_purge::credential_manager -- --ignored`.
#[cfg(all(test, windows))]
mod credential_manager {
    use super::{imp, purge, Outcome};

    /// Deletes the test's entries however the test ends.
    struct Cleanup(Vec<(String, String)>);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            for (service, user) in &self.0 {
                if let Ok(entry) = keyring::Entry::new(service, user) {
                    let _ = entry.delete_credential();
                }
            }
        }
    }

    #[test]
    #[ignore = "writes test entries to the Windows Credential Manager"]
    fn removes_its_own_entries_and_keeps_the_rest() {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let service = format!("sonarune-purge-test-{id}");
        let other_service = format!("{service}-other");
        let server = format!("https://sonarune-purge-test-{id}.invalid");
        let token = (service.clone(), format!("{server}::alice"));
        let password = (service.clone(), format!("{server}::alice::password"));
        // The same prefix, but without an account part, and another service.
        let no_account = (service.clone(), server.clone());
        let elsewhere = (other_service, format!("{server}::alice"));
        // An address typed in capitals, and the ListenBrainz entry.
        let shouted = (service.clone(), format!("{}::carol", server.to_uppercase()));
        let listenbrainz = (
            service.clone(),
            crate::commands::LISTENBRAINZ_ENTRY_NAME.to_owned(),
        );
        let cleanup = Cleanup(vec![
            token.clone(),
            password.clone(),
            no_account.clone(),
            elsewhere.clone(),
            shouted.clone(),
            listenbrainz.clone(),
        ]);
        let entry =
            |(service, user): &(String, String)| keyring::Entry::new(service, user).unwrap();
        for pair in &cleanup.0 {
            entry(pair).set_password("purge-test").unwrap();
        }

        let text = |targets: &[Vec<u16>]| -> Vec<String> {
            targets
                .iter()
                .map(|t| String::from_utf16(t.strip_suffix(&[0]).unwrap()).unwrap())
                .collect()
        };
        // In lower case, so the capitalised address is found only if the
        // filter ignores case.
        let listed = || imp::generic_targets(&format!("{server}*")).unwrap();
        let mut targets = listed();
        let names = text(&targets);
        assert_eq!(names.len(), 5, "the listing holds this test's entries only");
        assert!(names
            .iter()
            .all(|name| name.to_lowercase().starts_with(&server)));
        // keyring names an entry `<user>.<service>`, which the matching
        // relies on.
        let token_target = format!("{}.{}", token.1, token.0);
        assert!(names.contains(&token_target));
        assert!(names.contains(&format!("{}.{}", shouted.1, shouted.0)));
        // And the other way round, in capitals for a lower-case name.
        let filter = format!("{}.{}*", listenbrainz.1, listenbrainz.0).to_uppercase();
        let listenbrainz_targets = imp::generic_targets(&filter).unwrap();
        assert_eq!(
            text(&listenbrainz_targets),
            [format!("{}.{}", listenbrainz.1, listenbrainz.0)]
        );
        targets.extend(listenbrainz_targets);

        let outcome = purge(&targets, &[service.as_str()], |target| {
            imp::delete_generic(target).is_ok()
        });
        assert_eq!(
            outcome,
            Outcome {
                matched: 4,
                deleted: 4
            }
        );
        assert!(matches!(
            entry(&token).get_password(),
            Err(keyring::Error::NoEntry)
        ));
        assert!(matches!(
            entry(&password).get_password(),
            Err(keyring::Error::NoEntry)
        ));
        assert!(matches!(
            entry(&shouted).get_password(),
            Err(keyring::Error::NoEntry)
        ));
        assert!(matches!(
            entry(&listenbrainz).get_password(),
            Err(keyring::Error::NoEntry)
        ));
        assert!(entry(&no_account).get_password().is_ok());
        assert!(entry(&elsewhere).get_password().is_ok());
        assert_eq!(listed().len(), 2);
        // Deleting one that is already gone is no failure.
        let token_target: Vec<u16> = token_target.encode_utf16().chain(Some(0)).collect();
        assert_eq!(imp::delete_generic(&token_target), Ok(()));
    }
}
