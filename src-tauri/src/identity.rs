//! Whose stored data is whose.
//!
//! Sound analyses belong to a Jellyfin server; the play history and the scan
//! progress to an account on one. A server is told apart by the `Id` it
//! reports (`/System/Info/Public`), learned whenever a session is installed
//! (`commands::install_session`): the address typed at sign-in is only how it
//! is reached, and the same server behind a proxy, on https or at another IP
//! is the same server. An account is its Jellyfin user id together with that
//! server — two servers whose accounts share user ids (one set up as a copy of
//! the other) keep apart as long as each has an id of its own.
//!
//! The store remembers which id answered at which address
//! (`Store::record_server`), so a session started while the server cannot be
//! reached still knows whose it is, and rows stored under an address before
//! ids were kept can be given to the right server ([`settle`]). A server whose
//! id is not known — it could not be asked, and none was ever seen at its
//! address — is told apart by its address, as before ids were kept.

use crate::api::JellyfinClient;
use crate::error::AppResult;
use crate::store::{keys, Store};

/// A Jellyfin server as stored data names it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Server {
    /// The base URL it is reached at.
    pub address: String,
    /// Its Jellyfin id; `None` when it is not known, and then the address is
    /// the server.
    pub id: Option<String>,
}

impl Server {
    pub fn of(client: &JellyfinClient) -> Self {
        Self {
            address: client.base_url().to_string(),
            id: client.server_id().map(str::to_string),
        }
    }

    /// The server signed in now, as far as the store knows it: the stored
    /// address, and the id last seen there.
    pub fn current(store: &Store) -> Option<Self> {
        let address = store.get(keys::SERVER_URL)?;
        let id = store.server_id_at(&address);
        Some(Self { address, id })
    }
}

/// An account: whose play history and whose library listing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Owner {
    pub server: Server,
    /// The Jellyfin user id.
    pub user: String,
}

impl Owner {
    /// The account `client` is signed in as; `None` without one.
    pub fn of(client: &JellyfinClient) -> Option<Self> {
        Some(Self {
            server: Server::of(client),
            user: client.user_id().ok()?.to_string(),
        })
    }

    /// The account signed in now, by the store; `None` while signed out.
    pub fn current(store: &Store) -> Option<Self> {
        Some(Self {
            server: Server::current(store)?,
            user: store.get(keys::USER_ID)?,
        })
    }
}

/// Bring what is stored in line with `owner`'s server once a session for it
/// is installed. Idempotent, and through indexes only: run on every sign-in,
/// re-login and restore.
///
/// With the server's id known:
/// - An id it replaced at its address (`Store::replaced_ids`) is its own
///   when the account has plays on it: the same account on a server whose
///   data was restored into a fresh install, which answers with a new id —
///   or, the other way round, the original coming back after such a copy
///   stood in for it. The account's plays and the server's analyses go
///   over. Without such plays it is another server that once stood there,
///   and keeps its own.
/// - Its analyses are tagged with the address it is reached at now, which
///   is what a build from before ids reads.
/// - An analysis stored before ids were kept is its own when the account
///   played under that address before ids were kept — a Jellyfin user id is
///   minted by one server —, or when the server stands at that address now
///   (or did when last seen) and no other account played there before ids.
///   Others stay as they are: by their address they still count for the
///   server standing there, and whichever server the account's history
///   names takes them whenever it signs in — in either order. A complete
///   listing of the server standing there may release them meanwhile, as
///   it releases its own; any listing claims them back within the time
///   released analyses are kept.
/// - The account's plays from before ids, under an address the server
///   stands at, get its id (`history::adopt`).
///
/// Without it: what was analysed under the other addresses the account has
/// played under moves to this one, as before ids were kept.
///
/// Returns how many analyses and plays moved.
pub fn settle(store: &Store, owner: &Owner) -> AppResult<(usize, usize)> {
    use crate::history;
    if owner.user.is_empty() {
        return Ok((0, 0));
    }
    let server = &owner.server;
    let Some(id) = &server.id else {
        let tied = history::other_addresses(store, &owner.user, &server.address)?;
        return Ok((store.adopt_analyses(server, &tied)?, 0));
    };
    let (mut analyses, mut plays) = (0, 0);
    for old in store.replaced_ids(server)? {
        if history::has_plays_on(store, &owner.user, &old)? {
            plays += history::take_over(store, &owner.user, &old, id)?;
            analyses += store.take_over_analyses(&old, server)?;
        }
    }
    let mut addresses = history::addresses_before_ids(store, &owner.user)?;
    let mut its_own = store.addresses_of(id)?;
    its_own.push(server.address.clone());
    for address in its_own {
        if !addresses.contains(&address)
            && !history::others_played_before_ids(store, &owner.user, &address)?
        {
            addresses.push(address);
        }
    }
    analyses += store.adopt_analyses(server, &addresses)?;
    plays += history::adopt(store, owner)?;
    Ok((analyses, plays))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempStore {
        store: Option<Store>,
        dir: std::path::PathBuf,
    }

    impl std::ops::Deref for TempStore {
        type Target = Store;
        fn deref(&self) -> &Store {
            self.store.as_ref().expect("open until drop")
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            self.store.take();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn temp_store() -> TempStore {
        let dir = std::env::temp_dir().join(format!("sonarune-identity-{}", uuid::Uuid::new_v4()));
        TempStore {
            store: Some(Store::open(&dir).expect("open test store")),
            dir,
        }
    }

    /// A play as 0.6.1 recorded it: by address and user id, no server id.
    fn played_before_ids(store: &Store, address: &str, user: &str, item: &str) {
        store
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO play_history
                         (server_url, user_id, played_at, listened_ms, item_id, track)
                     VALUES (?1, ?2, 0, 0, ?3, '{}')",
                    rusqlite::params![address, user, item],
                )
            })
            .unwrap();
    }

    fn at(address: &str) -> Server {
        Server {
            address: address.into(),
            id: None,
        }
    }

    fn tags(store: &Store, item: &str) -> (Option<String>, Option<String>) {
        let row = store.analysis_row(item).expect("stored");
        (row.server_url, row.server_id)
    }

    #[test]
    fn a_sign_in_gives_its_server_what_was_stored_for_it_before_its_id_was_known() {
        let store = temp_store();
        let (lan, proxy, old) = (
            "http://10.0.0.5:8096",
            "https://music.example.org",
            "http://old.box:8096",
        );
        // Stored by 0.6.1 under the addresses of one server, played and
        // analysed, and one of an unrelated server.
        played_before_ids(&store, lan, "u1", "l");
        played_before_ids(&store, old, "u1", "o");
        for (item, address) in [("l", lan), ("o", old), ("x", "https://x.home")] {
            store
                .put_analysis_from(Some(&at(address)), item, "{}")
                .unwrap();
        }
        // The server answered at its LAN address before, and now through
        // the proxy.
        store.record_server(lan, "s1").unwrap();
        store.record_server(proxy, "s1").unwrap();
        let me = Owner {
            server: Server {
                address: proxy.into(),
                id: Some("s1".into()),
            },
            user: "u1".into(),
        };
        assert_eq!(settle(&store, &me).unwrap(), (2, 1));
        assert_eq!(tags(&store, "l"), (Some(proxy.into()), Some("s1".into())));
        // Played under an address no server was ever seen at: the user id
        // ties it to this server, as before ids were kept.
        assert_eq!(tags(&store, "o"), (Some(proxy.into()), Some("s1".into())));
        assert_eq!(tags(&store, "x"), (Some("https://x.home".into()), None));
        // Its play stays the account's on any server, as in 0.6.1: given to
        // this one, it would be gone from a copy of the server that the
        // account may have played on there. Only an address the server was
        // seen at gives a play its id.
        let play_id = |item: &str| -> Option<String> {
            store
                .with_conn(|conn| {
                    conn.query_row(
                        "SELECT server_id FROM play_history WHERE item_id = ?1",
                        [item],
                        |row| row.get(0),
                    )
                })
                .unwrap()
        };
        assert_eq!(play_id("l").as_deref(), Some("s1"));
        assert_eq!(play_id("o"), None);
        assert_eq!(settle(&store, &me).unwrap(), (0, 0), "once");
        // Nobody signed in: nothing to settle.
        let nobody = Owner {
            user: String::new(),
            ..me
        };
        assert_eq!(settle(&store, &nobody).unwrap(), (0, 0));
    }

    /// A play of `user` on the server `id` (none: before ids were kept).
    fn played_on(store: &Store, address: &str, user: &str, item: &str, id: Option<&str>) {
        store
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO play_history
                         (server_url, user_id, played_at, listened_ms, item_id, track, server_id)
                     VALUES (?1, ?2, 0, 0, ?3, ?4, ?5)",
                    rusqlite::params![
                        address,
                        user,
                        item,
                        serde_json::json!({
                            "itemId": item, "name": item, "artist": "A", "album": "B",
                        })
                        .to_string(),
                        id
                    ],
                )
            })
            .unwrap();
    }

    fn play_tag(store: &Store, item: &str) -> Option<String> {
        store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT server_id FROM play_history WHERE item_id = ?1",
                    [item],
                    |row| row.get(0),
                )
            })
            .unwrap()
    }

    fn owner(address: &str, id: &str, user: &str) -> Owner {
        Owner {
            server: Server {
                address: address.into(),
                id: Some(id.into()),
            },
            user: user.into(),
        }
    }

    type Tags = Vec<(String, Option<String>, Option<String>)>;

    /// The account u1 of server X played at A and at B before ids were
    /// kept; X has moved to B since, and a new server Y, with an account of
    /// its own, stands at A. `x_first`: which signs in first after the
    /// update. Returns where the analyses and u1's plays end up.
    fn a_new_server_at_an_old_address(x_first: bool) -> Tags {
        let store = temp_store();
        let (a, b) = ("http://10.0.0.5:8096", "https://music.example.org");
        played_before_ids(&store, a, "u1", "pa");
        played_before_ids(&store, b, "u1", "pb");
        store.put_analysis_from(Some(&at(a)), "a1", "{}").unwrap();
        store.put_analysis_from(Some(&at(b)), "b1", "{}").unwrap();
        let x = |store: &Store| {
            store.record_server(b, "x").unwrap();
            settle(store, &owner(b, "x", "u1")).unwrap();
        };
        let y = |store: &Store| {
            store.record_server(a, "y").unwrap();
            settle(store, &owner(a, "y", "u2")).unwrap();
        };
        if x_first {
            x(&store);
            y(&store);
        } else {
            y(&store);
            // Until X signs in, the old analysis counts for whoever stands
            // at its address — and is not given away.
            assert_eq!(tags(&store, "a1"), (Some(a.into()), None));
            x(&store);
        }
        let mut seen: Tags = ["a1", "b1"]
            .into_iter()
            .map(|item| {
                let (url, id) = tags(&store, item);
                (item.to_string(), url, id)
            })
            .collect();
        seen.extend(
            ["pa", "pb"]
                .into_iter()
                .map(|item| (item.to_string(), None, play_tag(&store, item))),
        );
        seen
    }

    #[test]
    fn a_new_server_at_an_old_address_takes_nothing_of_the_old_one_in_either_order() {
        let b = Some("https://music.example.org".to_string());
        let x = Some("x".to_string());
        let expected = vec![
            ("a1".to_string(), b.clone(), x.clone()),
            ("b1".to_string(), b, x.clone()),
            // Heard at A: still the account's on any server, X included.
            ("pa".to_string(), None, None),
            ("pb".to_string(), None, x),
        ];
        assert_eq!(a_new_server_at_an_old_address(true), expected);
        assert_eq!(a_new_server_at_an_old_address(false), expected);
    }

    #[test]
    fn a_server_restored_under_a_new_id_keeps_the_accounts_plays() {
        // Its data restored into a fresh install: the same address and user
        // ids, a new server id.
        let store = temp_store();
        let address = "https://music.home";
        store.record_server(address, "old").unwrap();
        played_on(&store, address, "u1", "p1", Some("old"));
        played_on(&store, address, "kid", "k1", Some("old"));
        let old = Server {
            address: address.into(),
            id: Some("old".into()),
        };
        store.put_analysis_from(Some(&old), "t1", "{}").unwrap();
        store.record_server(address, "new").unwrap();
        // An account the old server never had says nothing about it.
        assert_eq!(
            settle(&store, &owner(address, "new", "stranger")).unwrap(),
            (0, 0)
        );
        assert_eq!(play_tag(&store, "p1").as_deref(), Some("old"));
        assert_eq!(
            settle(&store, &owner(address, "new", "u1")).unwrap(),
            (1, 1)
        );
        assert_eq!(play_tag(&store, "p1").as_deref(), Some("new"));
        assert_eq!(
            tags(&store, "t1"),
            (Some(address.into()), Some("new".into()))
        );
        // Each account's plays when it signs in.
        assert_eq!(play_tag(&store, "k1").as_deref(), Some("old"));
        assert_eq!(
            settle(&store, &owner(address, "new", "kid")).unwrap(),
            (0, 1)
        );
        assert_eq!(play_tag(&store, "k1").as_deref(), Some("new"));
        // And the history shows them on the new one.
        store.set(keys::SERVER_URL, address).unwrap();
        store.set(keys::USER_ID, "u1").unwrap();
        assert_eq!(crate::history::page(&store, None, 50).unwrap().len(), 1);
    }

    #[test]
    fn a_server_that_answers_elsewhere_was_not_replaced() {
        // It moved away, and another server took its address — all within
        // one millisecond, and again with a clock that has gone back a day
        // since the first sightings were recorded.
        for clock_went_back in [false, true] {
            let store = temp_store();
            let (here, there) = ("https://music.home", "https://moved.home");
            store.record_server(here, "old").unwrap();
            played_on(&store, here, "u1", "p1", Some("old"));
            store.record_server(here, "new").unwrap();
            if clock_went_back {
                store
                    .with_conn(|conn| {
                        conn.execute(
                            "UPDATE server_address SET first_seen = first_seen + 86400000,
                                                       last_seen = last_seen + 86400000",
                            [],
                        )
                    })
                    .unwrap();
            }
            store.record_server(there, "old").unwrap();
            assert_eq!(
                settle(&store, &owner(here, "new", "u1")).unwrap(),
                (0, 0),
                "clock went back: {clock_went_back}"
            );
            assert_eq!(play_tag(&store, "p1").as_deref(), Some("old"));
        }
    }

    /// The plays of `user` the history shows on the server `id` at
    /// `address`, by item.
    fn shown(store: &Store, address: &str, id: &str, user: &str) -> Vec<String> {
        store.set(keys::SERVER_URL, address).unwrap();
        store.set(keys::USER_ID, user).unwrap();
        assert_eq!(store.server_id_at(address).as_deref(), Some(id));
        let mut items: Vec<String> = crate::history::page(store, None, 50)
            .unwrap()
            .into_iter()
            .map(|entry| entry.track.item_id)
            .collect();
        items.sort();
        items
    }

    /// A restored copy X2 stands in at the address of X for a while, then
    /// X comes back. Two accounts play on each; `parent_first`: who signs
    /// in first at every change. Returns what the history shows on X, then
    /// on X2, for each account.
    fn a_round_trip(parent_first: bool) -> [Vec<String>; 4] {
        let store = temp_store();
        let address = "https://music.home";
        let users = if parent_first {
            ["parent", "kid"]
        } else {
            ["kid", "parent"]
        };
        let visit = |id: &str, items: &[&str]| {
            store.record_server(address, id).unwrap();
            for user in users {
                settle(&store, &owner(address, id, user)).unwrap();
                for item in items {
                    let item = format!("{user}-{item}");
                    played_on(&store, address, user, &item, Some(id));
                }
            }
        };
        visit("x", &["x0", "x1"]);
        visit("x2", &["x2"]);
        visit("x", &["x3"]);
        [
            shown(&store, address, "x", "parent"),
            shown(&store, address, "x", "kid"),
            {
                // Looked at as X2, which answered there before.
                store.record_server(address, "x2").unwrap();
                shown(&store, address, "x2", "parent")
            },
            shown(&store, address, "x2", "kid"),
        ]
    }

    #[test]
    fn a_server_that_comes_back_takes_its_plays_back_from_the_copy() {
        let all = |user: &str| -> Vec<String> {
            ["x0", "x1", "x2", "x3"]
                .iter()
                .map(|item| format!("{user}-{item}"))
                .collect()
        };
        for parent_first in [true, false] {
            let [parent_on_x, kid_on_x, parent_on_x2, kid_on_x2] = a_round_trip(parent_first);
            assert_eq!(parent_on_x, all("parent"), "parent first: {parent_first}");
            assert_eq!(kid_on_x, all("kid"), "parent first: {parent_first}");
            assert!(parent_on_x2.is_empty() && kid_on_x2.is_empty());
        }
    }

    #[test]
    fn an_account_with_no_history_does_not_take_what_another_one_played() {
        // A shared server: the parent played at its address before ids, the
        // kid has history off and signs in first after the update.
        let store = temp_store();
        let address = "https://music.home";
        played_before_ids(&store, address, "parent", "p");
        store
            .put_analysis_from(Some(&at(address)), "t", "{}")
            .unwrap();
        store.record_server(address, "s1").unwrap();
        let kid = owner(address, "s1", "kid");
        assert_eq!(settle(&store, &kid).unwrap(), (0, 0));
        // By its address it is in the kid's index all the same.
        assert_eq!(store.analyses_for(&kid.server).unwrap().len(), 1);
        assert_eq!(
            settle(&store, &owner(address, "s1", "parent")).unwrap(),
            (1, 1)
        );
        assert_eq!(tags(&store, "t"), (Some(address.into()), Some("s1".into())));
        // Nobody else played there: a server standing there takes it.
        let store = temp_store();
        store
            .put_analysis_from(Some(&at(address)), "t", "{}")
            .unwrap();
        store.record_server(address, "s1").unwrap();
        assert_eq!(settle(&store, &kid).unwrap(), (1, 0));
    }

    #[test]
    fn the_signed_in_server_is_the_one_seen_last_at_its_address() {
        let store = temp_store();
        assert_eq!(Server::current(&store), None);
        store.set(keys::SERVER_URL, "https://music.home").unwrap();
        assert_eq!(Server::current(&store), Some(at("https://music.home")));
        store.record_server("https://music.home", "s1").unwrap();
        assert_eq!(
            Server::current(&store).and_then(|server| server.id),
            Some("s1".into())
        );
        assert_eq!(Owner::current(&store), None, "no account");
        store.set(keys::USER_ID, "u1").unwrap();
        assert_eq!(Owner::current(&store).unwrap().user, "u1");
    }
}
