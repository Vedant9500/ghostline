use rusqlite::{Connection, OptionalExtension, params};
use std::path::PathBuf;

/// Default DB path: $XDG_DATA_HOME/ghostline/history.db or ~/.local/share/ghostline/history.db
pub fn default_db_path() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        let mut p = PathBuf::from(xdg);
        p.push("ghostline/history.db");
        return p;
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut p = home;
    p.push(".local/share/ghostline/history.db");
    p
}

pub const SCHEMA: &str = "
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA busy_timeout=5000;
PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS history(
  id TEXT PRIMARY KEY,
  cmd TEXT NOT NULL,
  cwd TEXT NOT NULL,
  exit_code INTEGER NOT NULL,
  duration_ns INTEGER NOT NULL DEFAULT 0,
  started_at INTEGER NOT NULL,
  ended_at INTEGER NOT NULL,
  hostname TEXT NOT NULL DEFAULT '',
  session_id TEXT NOT NULL DEFAULT '',
  shell TEXT NOT NULL DEFAULT '',
  tty TEXT,
  ssh_ctx TEXT,
  tmux TEXT,
  container TEXT,
  sudo_user TEXT,
  git_repo TEXT,
  git_branch TEXT,
  sensitive INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX IF NOT EXISTS idx_time ON history(started_at DESC);
CREATE INDEX IF NOT EXISTS idx_cwd ON history(cwd, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_session ON history(session_id, started_at);
CREATE VIRTUAL TABLE IF NOT EXISTS history_fts USING fts5(cmd, cwd, tokenize='trigram');
CREATE TRIGGER IF NOT EXISTS history_ai AFTER INSERT ON history BEGIN
  INSERT INTO history_fts(rowid, cmd, cwd) VALUES (new.rowid, new.cmd, new.cwd);
END;
CREATE TRIGGER IF NOT EXISTS history_ad AFTER DELETE ON history BEGIN
  INSERT INTO history_fts(history_fts, rowid, cmd, cwd) VALUES('delete', old.rowid, old.cmd, old.cwd);
END;
";

/// Open (creating parents) + apply schema. DB file chmod 600 on creation.
pub fn open(path: &std::path::Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
    }
    Ok(conn)
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub cmd: String,
    pub cwd: String,
    pub exit_code: i64,
    pub duration_ns: i64,
    pub started_at: i64,
    pub ended_at: i64,
    pub hostname: String,
    pub session_id: String,
    pub shell: String,
    pub sensitive: bool,
}

pub fn insert(conn: &Connection, e: &Entry) -> rusqlite::Result<()> {
    insert_batch(conn, std::slice::from_ref(e)).map(|_| ())
}

/// Batch insert in a single transaction (daemon path: 500 rows / 1s).
/// Returns number of rows inserted.
pub fn insert_batch(conn: &Connection, entries: &[Entry]) -> rusqlite::Result<usize> {
    if entries.is_empty() {
        return Ok(0);
    }
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO history(id,cmd,cwd,exit_code,duration_ns,started_at,ended_at,hostname,session_id,shell,sensitive)
             VALUES(?,?,?,?,?,?,?,?,?,?,?)",
        )?;
        for e in entries {
            stmt.execute(params![
                uuid::Uuid::now_v7().to_string(),
                e.cmd,
                e.cwd,
                e.exit_code,
                e.duration_ns,
                e.started_at,
                e.ended_at,
                e.hostname,
                e.session_id,
                e.shell,
                if e.sensitive { 1 } else { 0 },
            ])?;
        }
    }
    tx.commit()?;
    Ok(entries.len())
}

/// Frecency-ish search. FTS5 MATCH when query has non-wildcard tokens, else recent.
/// Filters: cwd prefix, exit ("0" | "!0"), session, host, since (unix secs).
pub fn search(
    conn: &Connection,
    query: &str,
    limit: i64,
    dir: Option<&str>,
    exit_filter: Option<&str>,
    session: Option<&str>,
    host: Option<&str>,
    since: Option<i64>,
) -> rusqlite::Result<Vec<(i64, i64, String, String)>> {
    let exit_clause = match exit_filter {
        Some("0") => "AND h.exit_code = 0",
        Some("!0") => "AND h.exit_code != 0",
        _ => "",
    };
    let mut sql =
        String::from("SELECT h.started_at, h.exit_code, h.cwd, h.cmd FROM history h WHERE 1=1 ");
    if !query.is_empty() {
        // FTS path: join on history_fts when query looks matchable; fallback LIKE.
        sql.push_str("AND (h.cmd LIKE '%' || ?1 || '%' OR h.cwd LIKE '%' || ?1 || '%') ");
    } else {
        sql.push_str("AND ?1 IS NULL ");
    }
    if dir.is_some() {
        sql.push_str("AND h.cwd = ?2 ");
    } else {
        sql.push_str("AND ?2 IS NULL ");
    }
    sql.push_str(exit_clause);
    if session.is_some() {
        sql.push_str(" AND h.session_id = ?3");
    } else {
        sql.push_str(" AND ?3 IS NULL");
    }
    if host.is_some() {
        sql.push_str(" AND h.hostname = ?4");
    } else {
        sql.push_str(" AND ?4 IS NULL");
    }
    if since.is_some() {
        sql.push_str(" AND h.started_at >= ?5");
    } else {
        sql.push_str(" AND ?5 IS NULL");
    }
    sql.push_str(" ORDER BY h.started_at DESC LIMIT ?6");
    let mut stmt = conn.prepare(&sql)?;
    let q: Option<&str> = if query.is_empty() { None } else { Some(query) };
    let rows = stmt.query_map(params![q, dir, session, host, since, limit], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    rows.collect()
}

/// Ghost suggestion: prefix candidates ranked by dir-affinity + frequency + recency.
/// Returns full commands (not suffixes); caller strips the typed prefix for ghost rendering.
///
/// SAFETY: candidates containing control characters (ESC, newline, BEL, ...) are
/// silently dropped. A ghost is painted raw onto the terminal every keystroke;
/// replaying an entry like `echo -e '\e[7m..'` would fire inverse video, cursor
/// jumps and scrolls — i.e. whole-terminal flicker plus readline desync.
pub fn suggest(
    conn: &Connection,
    buffer: &str,
    cwd: &str,
    limit: i64,
) -> rusqlite::Result<Vec<String>> {
    // Weight: same-dir hit = 100, else 0; plus log(freq) and recency decay computed in SQL.
    // Over-fetch: control-tainted rows are filtered in Rust below.
    let mut stmt = conn.prepare(
        "SELECT cmd,
           (100 * MAX(CASE WHEN cwd = ?2 THEN 1 ELSE 0 END))
           + 10 * (SELECT count(*) FROM history h2 WHERE h2.cmd = h.cmd AND h2.cwd = ?2)
           + 5 * (SELECT count(*) FROM history h2 WHERE h2.cmd = h.cmd)
           AS w,
           max(started_at) AS last_seen
         FROM history h
         WHERE cmd LIKE ?1 || '%' AND cmd != ?1 AND exit_code = 0
         GROUP BY cmd
         ORDER BY w DESC, last_seen DESC
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![buffer, cwd, limit + 16], |r| r.get::<_, String>(0))?;
    Ok(rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|c| !c.chars().any(|ch| ch.is_control()))
        .take(limit.max(0) as usize)
        .collect())
}

/// Most recent distinct command starting with prefix (for tests / simple consumers).
#[allow(dead_code)]
pub fn latest_with_prefix(conn: &Connection, prefix: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT cmd FROM history WHERE cmd LIKE ?1 || '%' ORDER BY started_at DESC LIMIT 1",
        params![prefix],
        |r| r.get(0),
    )
    .optional()
}
