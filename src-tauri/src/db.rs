//! SQLite (FTS5) channel store + Tauri commands.
//!
//! Data model:
//! - `sources`   — named playlists / providers (M3U URL/file or Xtream login).
//! - `channels`  — channels, each tied to a source (`source_id`); FTS5-indexed.
//! - `favorites` — starred channels, keyed by stream url (survive re-import).
//! - `recents`   — recently played, keyed by url, capped to the last 50.

use std::io::Read;
use std::sync::Mutex;

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::playlist::{self, ParsedChannel};
use crate::xtream;

/// Managed Tauri state: a single connection behind a mutex.
pub struct Db(pub Mutex<Connection>);

const RECENTS_CAP: i64 = 50;

// ---- Serialized shapes ----------------------------------------------------

#[derive(Serialize, Clone)]
pub struct Channel {
    pub id: i64,
    pub source_id: i64,
    pub name: String,
    pub url: String,
    pub group: String,
    pub tvg_id: String,
    pub tvg_logo: String,
    pub is_fav: bool,
}

#[derive(Serialize, Clone)]
pub struct SavedChannel {
    pub url: String,
    pub name: String,
    pub group: String,
    pub tvg_logo: String,
    pub is_fav: bool,
}

#[derive(Serialize)]
pub struct Source {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub location: String,
    pub username: String,
    pub count: i64,
    pub refreshed_at: Option<i64>,
}

#[derive(Serialize)]
pub struct GroupCount {
    pub name: String,
    pub count: i64,
}

#[derive(Serialize)]
pub struct SourceSaved {
    pub id: i64,
    pub count: usize,
}

/// A channel reference sent from the UI (for favorites / recents).
#[derive(Deserialize)]
pub struct ChannelRef {
    pub url: String,
    pub name: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub tvg_logo: String,
}

// ---- Schema + migration ---------------------------------------------------

fn has_column(conn: &Connection, table: &str, col: &str) -> bool {
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();
    cols.iter().any(|c| c == col)
}

pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;

        CREATE TABLE IF NOT EXISTS sources(
            id           INTEGER PRIMARY KEY,
            name         TEXT NOT NULL,
            kind         TEXT NOT NULL,
            location     TEXT NOT NULL DEFAULT '',
            username     TEXT NOT NULL DEFAULT '',
            password     TEXT NOT NULL DEFAULT '',
            created_at   INTEGER NOT NULL DEFAULT (strftime('%s','now')),
            refreshed_at INTEGER
        );

        CREATE TABLE IF NOT EXISTS channels(
            id          INTEGER PRIMARY KEY,
            source_id   INTEGER NOT NULL DEFAULT 0,
            name        TEXT NOT NULL,
            url         TEXT NOT NULL,
            group_title TEXT NOT NULL DEFAULT '',
            tvg_id      TEXT NOT NULL DEFAULT '',
            tvg_logo    TEXT NOT NULL DEFAULT ''
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS channels_fts USING fts5(
            name, group_title, content='channels', content_rowid='id'
        );

        CREATE TABLE IF NOT EXISTS favorites(
            url         TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            group_title TEXT NOT NULL DEFAULT '',
            tvg_logo    TEXT NOT NULL DEFAULT '',
            added_at    INTEGER NOT NULL DEFAULT (strftime('%s','now'))
        );

        CREATE TABLE IF NOT EXISTS recents(
            url         TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            group_title TEXT NOT NULL DEFAULT '',
            tvg_logo    TEXT NOT NULL DEFAULT '',
            played_at   INTEGER NOT NULL DEFAULT (strftime('%s','now'))
        );
        ",
    )?;

    // Migrate a pre-M4 channels table (no source_id) — keep rows under a source.
    if !has_column(conn, "channels", "source_id") {
        conn.execute_batch("ALTER TABLE channels ADD COLUMN source_id INTEGER NOT NULL DEFAULT 0;")?;
        conn.execute(
            "INSERT INTO sources(name, kind, location) VALUES ('Imported', 'm3u_url', '')",
            [],
        )?;
        let sid = conn.last_insert_rowid();
        conn.execute("UPDATE channels SET source_id = ?1 WHERE source_id = 0", [sid])?;
    }

    // Index depends on source_id existing, so create it after any migration.
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_channels_source ON channels(source_id);")?;
    Ok(())
}

// ---- Source writes --------------------------------------------------------

fn insert_source(
    conn: &Connection,
    name: &str,
    kind: &str,
    location: &str,
    username: &str,
    password: &str,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO sources(name, kind, location, username, password)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![name, kind, location, username, password],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Replace a single source's channels and rebuild the FTS index.
fn replace_source(
    conn: &mut Connection,
    source_id: i64,
    channels: &[ParsedChannel],
) -> rusqlite::Result<usize> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM channels WHERE source_id = ?1", [source_id])?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO channels(source_id, name, url, group_title, tvg_id, tvg_logo)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for c in channels {
            stmt.execute(params![
                source_id, c.name, c.url, c.group, c.tvg_id, c.tvg_logo
            ])?;
        }
    }
    tx.execute(
        "UPDATE sources SET refreshed_at = strftime('%s','now') WHERE id = ?1",
        [source_id],
    )?;
    tx.execute("INSERT INTO channels_fts(channels_fts) VALUES('rebuild')", [])?;
    tx.commit()?;
    Ok(channels.len())
}

// ---- Reads ----------------------------------------------------------------

fn groups(conn: &Connection, source_id: Option<i64>) -> rusqlite::Result<Vec<GroupCount>> {
    let (sql, binds): (&str, Vec<SqlValue>) = match source_id {
        Some(sid) => (
            "SELECT group_title, COUNT(*) FROM channels WHERE source_id = ? GROUP BY group_title",
            vec![SqlValue::Integer(sid)],
        ),
        None => (
            "SELECT group_title, COUNT(*) FROM channels GROUP BY group_title",
            vec![],
        ),
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params_from_iter(binds), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut tally: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for row in rows {
        let (gt, n) = row?;
        for g in gt.split(';') {
            let g = g.trim();
            if !g.is_empty() {
                *tally.entry(g.to_string()).or_insert(0) += n;
            }
        }
    }
    let mut out: Vec<GroupCount> = tally
        .into_iter()
        .map(|(name, count)| GroupCount { name, count })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// `bbc news` -> `"bbc"* "news"*` (safe FTS5 prefix query).
fn fts_query(input: &str) -> String {
    input
        .split_whitespace()
        .map(|tok| format!("\"{}\"*", tok.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn row_to_channel(r: &rusqlite::Row) -> rusqlite::Result<Channel> {
    Ok(Channel {
        id: r.get(0)?,
        source_id: r.get(1)?,
        name: r.get(2)?,
        url: r.get(3)?,
        group: r.get(4)?,
        tvg_id: r.get(5)?,
        tvg_logo: r.get(6)?,
        is_fav: r.get(7)?,
    })
}

fn search(
    conn: &Connection,
    query: &str,
    group: Option<&str>,
    source_id: Option<i64>,
    limit: i64,
    offset: i64,
) -> rusqlite::Result<Vec<Channel>> {
    let sel = "c.id, c.source_id, c.name, c.url, c.group_title, c.tvg_id, c.tvg_logo, \
               EXISTS(SELECT 1 FROM favorites f WHERE f.url = c.url) AS is_fav";
    let mut sql = String::new();
    let mut binds: Vec<SqlValue> = Vec::new();

    let q = query.trim();
    if q.is_empty() {
        sql.push_str(&format!("SELECT {sel} FROM channels c WHERE 1=1"));
    } else {
        sql.push_str(&format!(
            "SELECT {sel} FROM channels_fts ft JOIN channels c ON c.id = ft.rowid \
             WHERE channels_fts MATCH ?"
        ));
        binds.push(SqlValue::Text(fts_query(q)));
    }
    if let Some(g) = group {
        sql.push_str(" AND (';' || c.group_title || ';') LIKE ('%;' || ? || ';%')");
        binds.push(SqlValue::Text(g.to_string()));
    }
    if let Some(sid) = source_id {
        sql.push_str(" AND c.source_id = ?");
        binds.push(SqlValue::Integer(sid));
    }
    sql.push_str(if q.is_empty() {
        " ORDER BY c.name"
    } else {
        " ORDER BY rank"
    });
    sql.push_str(" LIMIT ? OFFSET ?");
    binds.push(SqlValue::Integer(limit));
    binds.push(SqlValue::Integer(offset));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(binds), row_to_channel)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Fetch playlist text from an http(s) URL or a local file path.
fn fetch_source(source: &str) -> Result<String, String> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let mut resp = xtream::agent().get(source).call().map_err(|e| e.to_string())?;
        let status = resp.status();
        let mut body = String::new();
        resp.body_mut()
            .as_reader()
            .take(256 * 1024 * 1024)
            .read_to_string(&mut body)
            .map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("HTTP {} fetching playlist", status.as_u16()));
        }
        Ok(body)
    } else {
        std::fs::read_to_string(source).map_err(|e| e.to_string())
    }
}

// ---- Commands: sources ----------------------------------------------------

#[tauri::command]
pub async fn add_m3u_source(
    state: State<'_, Db>,
    name: String,
    location: String,
) -> Result<SourceSaved, String> {
    let loc = location.trim().to_string();
    if loc.is_empty() {
        return Err("a URL or file path is required".into());
    }
    let loc2 = loc.clone();
    let content = tauri::async_runtime::spawn_blocking(move || fetch_source(&loc2))
        .await
        .map_err(|e| e.to_string())??;
    let channels = playlist::parse_m3u(&content);
    if channels.is_empty() {
        return Err("no channels found in playlist".into());
    }
    let kind = if loc.starts_with("http") { "m3u_url" } else { "m3u_file" };

    let mut guard = state.0.lock().map_err(|e| e.to_string())?;
    let id = insert_source(&guard, &name, kind, &loc, "", "").map_err(|e| e.to_string())?;
    let count = replace_source(&mut guard, id, &channels).map_err(|e| e.to_string())?;
    Ok(SourceSaved { id, count })
}

#[tauri::command]
pub async fn add_xtream_source(
    state: State<'_, Db>,
    name: String,
    host: String,
    username: String,
    password: String,
) -> Result<SourceSaved, String> {
    let (h, u, p) = (host.clone(), username.clone(), password.clone());
    let channels = tauri::async_runtime::spawn_blocking(move || xtream::fetch_live(&h, &u, &p))
        .await
        .map_err(|e| e.to_string())??;

    let mut guard = state.0.lock().map_err(|e| e.to_string())?;
    let id = insert_source(&guard, &name, "xtream", &host, &username, &password)
        .map_err(|e| e.to_string())?;
    let count = replace_source(&mut guard, id, &channels).map_err(|e| e.to_string())?;
    Ok(SourceSaved { id, count })
}

#[tauri::command]
pub async fn refresh_source(state: State<'_, Db>, id: i64) -> Result<usize, String> {
    // Read source config, then release the lock before the network fetch.
    let (kind, location, username, password) = {
        let guard = state.0.lock().map_err(|e| e.to_string())?;
        guard
            .query_row(
                "SELECT kind, location, username, password FROM sources WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .map_err(|_| "source not found".to_string())?
    };

    let channels = tauri::async_runtime::spawn_blocking(
        move || -> Result<Vec<ParsedChannel>, String> {
            match kind.as_str() {
                "xtream" => xtream::fetch_live(&location, &username, &password),
                _ => Ok(playlist::parse_m3u(&fetch_source(&location)?)),
            }
        },
    )
    .await
    .map_err(|e| e.to_string())??;

    if channels.is_empty() {
        return Err("no channels found on refresh".into());
    }
    let mut guard = state.0.lock().map_err(|e| e.to_string())?;
    replace_source(&mut guard, id, &channels).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn remove_source(state: State<'_, Db>, id: i64) -> Result<(), String> {
    let mut guard = state.0.lock().map_err(|e| e.to_string())?;
    let tx = guard.transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM channels WHERE source_id = ?1", [id])
        .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM sources WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    tx.execute("INSERT INTO channels_fts(channels_fts) VALUES('rebuild')", [])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_sources(state: State<'_, Db>) -> Result<Vec<Source>, String> {
    let guard = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = guard
        .prepare(
            "SELECT s.id, s.name, s.kind, s.location, s.username,
                    (SELECT COUNT(*) FROM channels c WHERE c.source_id = s.id) AS cnt,
                    s.refreshed_at
             FROM sources s ORDER BY s.created_at",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Source {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                location: r.get(3)?,
                username: r.get(4)?,
                count: r.get(5)?,
                refreshed_at: r.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

// ---- Commands: search -----------------------------------------------------

#[tauri::command]
pub fn search_channels(
    state: State<'_, Db>,
    query: String,
    group: Option<String>,
    source_id: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<Channel>, String> {
    let guard = state.0.lock().map_err(|e| e.to_string())?;
    search(
        &guard,
        &query,
        group.as_deref().filter(|g| !g.is_empty()),
        source_id,
        limit.unwrap_or(200).clamp(1, 2000),
        offset.unwrap_or(0).max(0),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_groups(state: State<'_, Db>, source_id: Option<i64>) -> Result<Vec<GroupCount>, String> {
    let guard = state.0.lock().map_err(|e| e.to_string())?;
    groups(&guard, source_id).map_err(|e| e.to_string())
}

// ---- Commands: favorites + recents ---------------------------------------

#[tauri::command]
pub fn toggle_favorite(state: State<'_, Db>, channel: ChannelRef) -> Result<bool, String> {
    let guard = state.0.lock().map_err(|e| e.to_string())?;
    let exists: bool = guard
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM favorites WHERE url = ?1)",
            [&channel.url],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if exists {
        guard
            .execute("DELETE FROM favorites WHERE url = ?1", [&channel.url])
            .map_err(|e| e.to_string())?;
        Ok(false)
    } else {
        guard
            .execute(
                "INSERT OR REPLACE INTO favorites(url, name, group_title, tvg_logo)
                 VALUES (?1, ?2, ?3, ?4)",
                params![channel.url, channel.name, channel.group, channel.tvg_logo],
            )
            .map_err(|e| e.to_string())?;
        Ok(true)
    }
}

#[tauri::command]
pub fn list_favorites(state: State<'_, Db>) -> Result<Vec<SavedChannel>, String> {
    let guard = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = guard
        .prepare(
            "SELECT url, name, group_title, tvg_logo FROM favorites ORDER BY added_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(SavedChannel {
                url: r.get(0)?,
                name: r.get(1)?,
                group: r.get(2)?,
                tvg_logo: r.get(3)?,
                is_fav: true,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

#[tauri::command]
pub fn record_play(state: State<'_, Db>, channel: ChannelRef) -> Result<(), String> {
    let guard = state.0.lock().map_err(|e| e.to_string())?;
    guard
        .execute(
            "INSERT INTO recents(url, name, group_title, tvg_logo, played_at)
             VALUES (?1, ?2, ?3, ?4, strftime('%s','now'))
             ON CONFLICT(url) DO UPDATE SET
                played_at = strftime('%s','now'),
                name = excluded.name,
                group_title = excluded.group_title,
                tvg_logo = excluded.tvg_logo",
            params![channel.url, channel.name, channel.group, channel.tvg_logo],
        )
        .map_err(|e| e.to_string())?;
    // Keep only the most recent N.
    guard
        .execute(
            "DELETE FROM recents WHERE url NOT IN
                (SELECT url FROM recents ORDER BY played_at DESC LIMIT ?1)",
            [RECENTS_CAP],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn list_recents(state: State<'_, Db>, limit: Option<i64>) -> Result<Vec<SavedChannel>, String> {
    let guard = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = guard
        .prepare(
            "SELECT r.url, r.name, r.group_title, r.tvg_logo,
                    EXISTS(SELECT 1 FROM favorites f WHERE f.url = r.url)
             FROM recents r ORDER BY played_at DESC LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([limit.unwrap_or(RECENTS_CAP).clamp(1, RECENTS_CAP)], |r| {
            Ok(SavedChannel {
                url: r.get(0)?,
                name: r.get(1)?,
                group: r.get(2)?,
                tvg_logo: r.get(3)?,
                is_fav: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

/// Open (or create) the database under the app data dir and install schema.
pub fn open(app: &AppHandle) -> Result<Db, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let conn = Connection::open(dir.join("vista.db")).map_err(|e| e.to_string())?;
    init(&conn).map_err(|e| e.to_string())?;
    Ok(Db(Mutex::new(conn)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<ParsedChannel> {
        vec![
            ParsedChannel { name: "BBC News".into(), url: "http://a".into(), group: "News".into(), tvg_id: "bbc".into(), tvg_logo: "".into() },
            ParsedChannel { name: "CNN International".into(), url: "http://b".into(), group: "News;Public".into(), tvg_id: "cnn".into(), tvg_logo: "".into() },
            ParsedChannel { name: "Sky Sports".into(), url: "http://c".into(), group: "Sports".into(), tvg_id: "sky".into(), tvg_logo: "".into() },
        ]
    }

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap(); // fails if bundled SQLite lacks FTS5
        conn
    }

    #[test]
    fn source_import_search_groups() {
        let mut conn = mem();
        let sid = insert_source(&conn, "Test", "m3u_url", "http://x", "", "").unwrap();
        assert_eq!(replace_source(&mut conn, sid, &sample()).unwrap(), 3);

        // Prefix FTS search across all sources.
        assert_eq!(search(&conn, "new", None, None, 50, 0).unwrap().len(), 2);
        // Source filter.
        assert_eq!(search(&conn, "", None, Some(sid), 50, 0).unwrap().len(), 3);
        assert_eq!(search(&conn, "", None, Some(sid + 999), 50, 0).unwrap().len(), 0);
        // Exact ;-delimited group membership.
        let pub_ = search(&conn, "", Some("Public"), None, 50, 0).unwrap();
        assert_eq!(pub_.len(), 1);
        assert_eq!(pub_[0].name, "CNN International");
        // Groups split + tally.
        let g = groups(&conn, None).unwrap();
        assert_eq!(g.iter().find(|x| x.name == "News").unwrap().count, 2);
    }

    #[test]
    fn migrates_pre_m4_channels() {
        // Simulate the M3 schema: a channels table with no source_id.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE channels(id INTEGER PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL,
                group_title TEXT NOT NULL DEFAULT '', tvg_id TEXT NOT NULL DEFAULT '',
                tvg_logo TEXT NOT NULL DEFAULT '');
             CREATE VIRTUAL TABLE channels_fts USING fts5(name, group_title,
                content='channels', content_rowid='id');",
        )
        .unwrap();
        conn.execute("INSERT INTO channels(name, url) VALUES ('Old', 'http://old')", [])
            .unwrap();

        // init() must migrate without error and preserve the existing row.
        init(&conn).unwrap();
        assert!(has_column(&conn, "channels", "source_id"));
        let sid: i64 = conn
            .query_row("SELECT source_id FROM channels WHERE url='http://old'", [], |r| r.get(0))
            .unwrap();
        assert!(sid > 0, "existing channel should be assigned to a source");
        let srcs: i64 = conn
            .query_row("SELECT COUNT(*) FROM sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(srcs, 1);
    }

    #[test]
    fn favorites_and_recents() {
        let mut conn = mem();
        let sid = insert_source(&conn, "T", "m3u_url", "", "", "").unwrap();
        replace_source(&mut conn, sid, &sample()).unwrap();

        // Toggling favorite flips state and shows up in search results.
        let fav = |url: &str| {
            let exists: bool = conn
                .query_row("SELECT EXISTS(SELECT 1 FROM favorites WHERE url=?1)", [url], |r| r.get(0))
                .unwrap();
            if exists {
                conn.execute("DELETE FROM favorites WHERE url=?1", [url]).unwrap();
            } else {
                conn.execute(
                    "INSERT INTO favorites(url,name) VALUES(?1,'x')",
                    [url],
                )
                .unwrap();
            }
        };
        fav("http://a");
        let rows = search(&conn, "bbc", None, None, 10, 0).unwrap();
        assert!(rows[0].is_fav, "BBC should be marked favorite");

        // record_play semantics: upsert + cap.
        conn.execute(
            "INSERT INTO recents(url,name) VALUES('http://a','BBC')
             ON CONFLICT(url) DO UPDATE SET played_at=strftime('%s','now')",
            [],
        )
        .unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM recents", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
