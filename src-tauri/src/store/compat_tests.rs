//! The database across versions: what 0.6.0 and 0.6.1 left behind is
//! carried over when this build opens it, and what this build leaves behind
//! still opens and works in them. Their statements are copied here as they
//! shipped (`store.rs` and `history.rs` of each release).

use super::{keys, Store};
use crate::identity::{Owner, Server};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

/// 0.6.0 and 0.6.1, `Store::open`: the tables before anything else.
const OPEN_060: &str = "PRAGMA journal_mode = WAL;
     CREATE TABLE IF NOT EXISTS kv (
         key   TEXT PRIMARY KEY,
         value TEXT NOT NULL
     );
     CREATE TABLE IF NOT EXISTS track_analysis (
         item_id TEXT PRIMARY KEY,
         json    TEXT NOT NULL
     );";
const KV_GET: &str = "SELECT value FROM kv WHERE key = ?1";
const KV_SET: &str = "INSERT INTO kv (key, value) VALUES (?1, ?2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value";
const GET: &str = "SELECT json FROM track_analysis WHERE item_id = ?1";
/// 0.6.0, `Store::put_analysis`.
const PUT_060: &str = "INSERT INTO track_analysis (item_id, json) VALUES (?1, ?2)
     ON CONFLICT(item_id) DO UPDATE SET json = excluded.json";

/// 0.6.1, `history::SCHEMA`, run by `Store::open` after `OPEN_060`.
const HISTORY_061: &str = "
    CREATE TABLE IF NOT EXISTS play_history (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        server_url  TEXT    NOT NULL,
        user_id     TEXT    NOT NULL,
        played_at   INTEGER NOT NULL,
        listened_ms INTEGER NOT NULL,
        item_id     TEXT    NOT NULL,
        track       TEXT    NOT NULL
    );
    DROP INDEX IF EXISTS play_history_owner;
    CREATE INDEX IF NOT EXISTS play_history_user
        ON play_history (user_id, id);
    CREATE INDEX IF NOT EXISTS play_history_user_time
        ON play_history (user_id, played_at);";
/// 0.6.1, the analyses' server column, added by `Store::open` if missing.
const HAS_SERVER_061: &str =
    "SELECT 1 FROM pragma_table_info('track_analysis') WHERE name = 'server_url'";
const ADD_SERVER_061: &str = "ALTER TABLE track_analysis ADD COLUMN server_url TEXT";
/// 0.6.1, `Store::put_analysis_from`.
const PUT_061: &str = "INSERT INTO track_analysis (item_id, json, server_url) VALUES (?1, ?2, ?3)
     ON CONFLICT(item_id) DO UPDATE SET json = excluded.json,
                                        server_url = excluded.server_url";
/// 0.6.1, `Store::claim_analyses` and `Store::release_analyses`.
const CLAIM_061: &str = "UPDATE track_analysis SET server_url = ?1
     WHERE server_url IS NOT ?2 AND item_id = ?3";
const RELEASE_061: &str = "UPDATE track_analysis SET server_url = ?1
     WHERE server_url = ?2 AND item_id = ?3";
/// 0.6.1, `Store::analyses_for`: the sonic index.
const INDEX_061: &str = "SELECT item_id, json FROM track_analysis
     WHERE server_url = ?1 OR server_url IS NULL";
/// 0.6.1, `history::record_at`, `history::page` and `history::clear`.
const RECORD_061: &str = "INSERT INTO play_history
         (server_url, user_id, played_at, listened_ms, item_id, track)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
const PAGE_061: &str = "SELECT id, played_at, listened_ms, track FROM play_history
     WHERE user_id = ?1 AND id < ?2
     ORDER BY id DESC LIMIT ?3";
const CLEAR_061: &str = "DELETE FROM play_history WHERE user_id = ?1";

const LAN: &str = "http://10.0.0.5:8096";
const PROXY: &str = "https://music.example.org";
/// A tempo/key analysis as 0.6.0 stored it: no sound vector, no server
/// length.
const ANALYSIS_060: &str = r#"{"version":1,"bpm":120.0,"tempoConfidence":0.5,"intro":null,"outro":null,"key":null,"keyCorrelation":0.0,"audibleStartMs":0.0,"audibleEndMs":200000.0,"durationMs":200000.0}"#;
const TRACK: &str = r#"{"itemId":"t","name":"T","artist":"A","album":"B"}"#;

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir() -> TempDir {
    let dir = std::env::temp_dir().join(format!("sonarune-compat-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// The database file the way an older build opens it.
fn old_build(dir: &Path) -> Connection {
    Connection::open(dir.join(super::DB_FILE)).unwrap()
}

/// What 0.6.1 runs when it opens the database.
fn open_061(dir: &Path) -> Connection {
    let conn = old_build(dir);
    conn.execute_batch(OPEN_060).unwrap();
    conn.execute_batch(HISTORY_061).unwrap();
    if !conn.prepare(HAS_SERVER_061).unwrap().exists([]).unwrap() {
        conn.execute_batch(ADD_SERVER_061).unwrap();
    }
    conn
}

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    conn.prepare("SELECT name FROM pragma_table_info(?1)")
        .unwrap()
        .query_map([table], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap()
}

fn ids(rows: Vec<(String, String)>) -> Vec<String> {
    let mut ids: Vec<String> = rows.into_iter().map(|(id, _)| id).collect();
    ids.sort();
    ids
}

fn index_061(conn: &Connection, server: &str) -> Vec<String> {
    let rows = conn
        .prepare(INDEX_061)
        .unwrap()
        .query_map([server], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<(String, String)>>>()
        .unwrap();
    ids(rows)
}

fn page_061(conn: &Connection, user: &str) -> Vec<String> {
    conn.prepare(PAGE_061)
        .unwrap()
        .query_map(params![user, i64::MAX, 50], |row| row.get::<_, i64>(0))
        .unwrap()
        .map(|id| id.unwrap().to_string())
        .collect()
}

fn server(address: &str, id: &str) -> Server {
    Server {
        address: address.into(),
        id: Some(id.into()),
    }
}

#[test]
fn a_0_6_0_database_is_carried_over_and_still_works_in_0_6_0() {
    let dir = temp_dir();
    {
        let conn = old_build(&dir.0);
        conn.execute_batch(OPEN_060).unwrap();
        conn.execute(KV_SET, [keys::SERVER_URL, LAN]).unwrap();
        conn.execute(KV_SET, [keys::USER_ID, "u1"]).unwrap();
        conn.execute(KV_SET, [keys::VOLUME, "0.42"]).unwrap();
        conn.execute(PUT_060, ["t", ANALYSIS_060]).unwrap();
    }

    let store = Store::open(&dir.0).unwrap();
    assert_eq!(store.get(keys::VOLUME).as_deref(), Some("0.42"));
    assert_eq!(store.get_analysis("t").as_deref(), Some(ANALYSIS_060));
    let row = store.analysis_row("t").unwrap();
    assert_eq!((row.server_url, row.server_id), (None, None));
    // Nothing says which library the track is in: no server's index offers
    // it until a listing claims it.
    let me = server(LAN, "s1");
    assert!(store.analyses_for(&me).unwrap().is_empty());
    assert!(store
        .analyses_for(&Server {
            address: LAN.into(),
            id: None,
        })
        .unwrap()
        .is_empty());
    store.record_server(LAN, "s1").unwrap();
    let owner = Owner {
        server: me.clone(),
        user: "u1".into(),
    };
    assert_eq!(crate::identity::settle(&store, &owner).unwrap(), (0, 0));
    assert!(crate::history::page(&store, None, 50).unwrap().is_empty());
    assert_eq!(store.claim_analyses(&me, &["t".into()]).unwrap(), 1);
    assert_eq!(ids(store.analyses_for(&me).unwrap()), ["t"]);
    store.put_analysis_from(Some(&me), "u", "{}").unwrap();
    drop(store);

    // Back to 0.6.0: it opens the file, reads and writes as before.
    {
        let conn = old_build(&dir.0);
        conn.execute_batch(OPEN_060).unwrap();
        let get = |id: &str| -> String { conn.query_row(GET, [id], |row| row.get(0)).unwrap() };
        assert_eq!(get("t"), ANALYSIS_060);
        assert_eq!(get("u"), "{}");
        conn.execute(PUT_060, ["u", ANALYSIS_060]).unwrap();
        conn.execute(PUT_060, ["v", ANALYSIS_060]).unwrap();
        assert_eq!(get("u"), ANALYSIS_060);
        let volume: String = conn
            .query_row(KV_GET, [keys::VOLUME], |row| row.get(0))
            .unwrap();
        assert_eq!(volume, "0.42");
        conn.execute(KV_SET, [keys::VOLUME, "0.5"]).unwrap();
    }

    // And forward again: what 0.6.0 wrote is there, whose it was too.
    let store = Store::open(&dir.0).unwrap();
    assert_eq!(store.get(keys::VOLUME).as_deref(), Some("0.5"));
    assert_eq!(ids(store.analyses_for(&me).unwrap()), ["t", "u"]);
    assert_eq!(store.get_analysis("v").as_deref(), Some(ANALYSIS_060));
}

#[test]
fn a_0_6_1_database_is_carried_over_and_still_works_in_0_6_1() {
    let dir = temp_dir();
    {
        let conn = open_061(&dir.0);
        conn.execute(KV_SET, [keys::SERVER_URL, LAN]).unwrap();
        conn.execute(KV_SET, [keys::USER_ID, "u1"]).unwrap();
        conn.execute(
            KV_SET,
            [
                keys::SONIC_SCAN_PROGRESS,
                r#"[{"server":"http://10.0.0.5:8096","user":"u1","analysed":2,"total":3}]"#,
            ],
        )
        .unwrap();
        conn.execute(PUT_061, params!["a", "{}", LAN]).unwrap();
        conn.execute(PUT_061, params!["b", "{}", LAN]).unwrap();
        conn.execute(PUT_061, params!["r1", "{}", LAN]).unwrap();
        conn.execute(PUT_061, params!["r2", "{}", LAN]).unwrap();
        conn.execute(PUT_061, params!["gone", "{}", ""]).unwrap();
        conn.execute(PUT_060, ["legacy", ANALYSIS_060]).unwrap();
        conn.execute(RECORD_061, params![LAN, "u1", 1_000, 1_000, "a", TRACK])
            .unwrap();
        conn.execute(
            RECORD_061,
            params!["http://old.box", "u1", 2_000, 1_000, "a", TRACK],
        )
        .unwrap();
    }

    let store = Store::open(&dir.0).unwrap();
    store
        .with_conn(|conn| {
            assert!(columns(conn, "track_analysis").contains(&"server_id".to_string()));
            assert!(columns(conn, "track_analysis").contains(&"released_at".to_string()));
            assert!(columns(conn, "play_history").contains(&"server_id".to_string()));
            let released: Option<i64> = conn.query_row(
                "SELECT released_at FROM track_analysis WHERE item_id = 'gone'",
                [],
                |row| row.get(0),
            )?;
            assert!(released.is_some(), "kept from the update on");
            Ok(())
        })
        .unwrap();
    // Before the server said its id: as in 0.6.1.
    assert_eq!(crate::history::page(&store, None, 50).unwrap().len(), 2);
    let lan = Server {
        address: LAN.into(),
        id: None,
    };
    assert_eq!(
        ids(store.analyses_for(&lan).unwrap()),
        ["a", "b", "r1", "r2"]
    );
    // The first sign-in learns it.
    store.record_server(LAN, "s1").unwrap();
    let me = server(LAN, "s1");
    let owner = Owner {
        server: me.clone(),
        user: "u1".into(),
    };
    assert_eq!(crate::identity::settle(&store, &owner).unwrap(), (4, 1));
    assert_eq!(
        ids(store.analyses_for(&me).unwrap()),
        ["a", "b", "r1", "r2"]
    );
    // Gone from the library, as far as this build is concerned.
    store
        .release_analyses(&me, &["r1".into(), "r2".into()])
        .unwrap();
    assert_eq!(crate::history::page(&store, None, 50).unwrap().len(), 2);
    crate::history::record(
        &store,
        &owner,
        &serde_json::from_value(serde_json::json!({
            "itemId": "b", "name": "B", "artist": "A", "album": "B", "albumId": null,
            "durationMs": 200_000, "imageItemId": null, "imageTag": null,
            "streamUrl": format!("{LAN}/Audio/b/universal"),
        }))
        .unwrap(),
        200_000,
        Some(3_000),
    )
    .unwrap();
    drop(store);

    // Back to 0.6.1: everything opens, its index and its history read as
    // they did, and its writes go through.
    {
        let conn = open_061(&dir.0);
        assert_eq!(index_061(&conn, LAN), ["a", "b", "legacy"]);
        assert_eq!(page_061(&conn, "u1").len(), 3);
        conn.execute(RECORD_061, params![LAN, "u1", 4_000, 1_000, "c", TRACK])
            .unwrap();
        conn.execute(PUT_061, params!["c", "{}", LAN]).unwrap();
        // It reaches the server through the proxy, claims one track there
        // and releases another.
        conn.execute(CLAIM_061, params![PROXY, PROXY, "a"]).unwrap();
        conn.execute(RELEASE_061, params!["", LAN, "b"]).unwrap();
        assert_eq!(index_061(&conn, PROXY), ["a", "legacy"]);
        // It lists the two released tracks again: one claimed back, one
        // analysed anew.
        conn.execute(CLAIM_061, params![LAN, LAN, "r1"]).unwrap();
        conn.execute(PUT_061, params!["r2", "{}", LAN]).unwrap();
    }

    // Forward again: what 0.6.1 moved by address goes by its address, until
    // the next sign-in settles it.
    let store = Store::open(&dir.0).unwrap();
    let row = |id: &str| {
        let row = store.analysis_row(id).unwrap();
        (row.server_url, row.server_id)
    };
    let released_at = |id: &str| -> Option<i64> {
        store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT released_at FROM track_analysis WHERE item_id = ?1",
                    [id],
                    |row| row.get(0),
                )
            })
            .unwrap()
    };
    assert_eq!(row("a"), (Some(PROXY.into()), None));
    assert_eq!(row("b"), (Some(String::new()), None));
    assert!(
        released_at("b").is_some(),
        "released by 0.6.1: kept from now"
    );
    // Back in the library: no release date that would delete them.
    assert_eq!((released_at("r1"), released_at("r2")), (None, None));
    assert_eq!(ids(store.analyses_for(&me).unwrap()), ["c", "r1", "r2"]);
    assert_eq!(crate::history::page(&store, None, 50).unwrap().len(), 4);
    store.record_server(PROXY, "s1").unwrap();
    let proxied = Owner {
        server: server(PROXY, "s1"),
        user: "u1".into(),
    };
    crate::identity::settle(&store, &proxied).unwrap();
    assert_eq!(
        ids(store.analyses_for(&proxied.server).unwrap()),
        ["a", "c", "r1", "r2"]
    );
    drop(store);

    // 0.6.1 clears the account's history as it did.
    let conn = open_061(&dir.0);
    assert_eq!(conn.execute(CLEAR_061, ["u1"]).unwrap(), 4);
}

#[test]
fn a_database_that_cannot_be_indexed_still_opens_with_everything_in_it() {
    // Building an index reads a whole table; on a damaged page it fails,
    // and an open that failed would have the database set aside, history
    // and all. Stood in for here by a name the index cannot take.
    let dir = temp_dir();
    {
        let conn = open_061(&dir.0);
        conn.execute(KV_SET, [keys::USER_ID, "u1"]).unwrap();
        conn.execute(KV_SET, [keys::SERVER_URL, LAN]).unwrap();
        conn.execute(RECORD_061, params![LAN, "u1", 1_000, 1_000, "a", TRACK])
            .unwrap();
        conn.execute(PUT_061, params!["a", "{}", LAN]).unwrap();
        conn.execute_batch("CREATE TABLE track_analysis_owner (x)")
            .unwrap();
    }
    let store = Store::open(&dir.0).expect("opens all the same");
    // The other indexes are there.
    let indexes: Vec<String> = store
        .with_conn(|conn| {
            conn.prepare("SELECT name FROM sqlite_master WHERE type = 'index'")?
                .query_map([], |row| row.get(0))?
                .collect()
        })
        .unwrap();
    assert!(indexes.contains(&"track_analysis_released".to_string()));
    assert!(indexes.contains(&"play_history_before_ids".to_string()));
    assert_eq!(crate::history::page(&store, None, 50).unwrap().len(), 1);
    let lan = Server {
        address: LAN.into(),
        id: None,
    };
    assert_eq!(ids(store.analyses_for(&lan).unwrap()), ["a"]);
    store.record_server(LAN, "s1").unwrap();
    let owner = Owner {
        server: server(LAN, "s1"),
        user: "u1".into(),
    };
    assert_eq!(crate::identity::settle(&store, &owner).unwrap(), (1, 1));
}
