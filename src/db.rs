use rusqlite::{Connection, params};
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
-- Covering partial index for the per-keystroke prefix query in suggest():
-- successful, non-sensitive rows only; cmd/cwd/started_at read from the index.
CREATE INDEX IF NOT EXISTS idx_cmd ON history(cmd COLLATE NOCASE, cwd, started_at DESC)
  WHERE exit_code = 0 AND sensitive = 0;
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

/// Trigram FTS5 needs at least one three-character run; shorter queries use LIKE.
const FTS_MIN_CHARS: usize = 3;

fn use_fts(query: &str) -> bool {
    query.chars().count() >= FTS_MIN_CHARS && !query.trim().is_empty()
}

/// Render a user query as an FTS5 string phrase, so MATCH operators and
/// punctuation (`-`, `*`, `%`, ...) are searched literally instead of parsed
/// as query syntax. Embedded double quotes are escaped by doubling.
fn fts_phrase(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 2);
    out.push('"');
    for c in query.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Escape LIKE wildcards so a typed `%`/`_`/`\` stays literal under ESCAPE '\'.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Frecency-ish search. FTS5 trigram phrase when the query is matchable,
/// LIKE fallback for 1-2 character queries (trigrams cannot match those).
/// Filters: cwd, exit ("0" | "!0"), session, host, since (unix nanoseconds;
/// `cmd_search` converts user-facing seconds).
pub fn search_sql(
    query: &str,
    dir: Option<&str>,
    exit_filter: Option<&str>,
    session: Option<&str>,
    host: Option<&str>,
    since: Option<i64>,
) -> String {
    let exit_clause = match exit_filter {
        Some("0") => "AND h.exit_code = 0",
        Some("!0") => "AND h.exit_code != 0",
        _ => "",
    };
    let fts = use_fts(query);
    let mut sql = String::from("SELECT h.started_at, h.exit_code, h.cwd, h.cmd FROM history h ");
    if fts {
        sql.push_str("JOIN history_fts ON history_fts.rowid = h.rowid ");
    }
    sql.push_str("WHERE 1=1 ");
    if fts {
        sql.push_str("AND history_fts MATCH ?1 ");
    } else if !query.is_empty() {
        // Short query: trigram FTS cannot match it, scan with LIKE instead.
        sql.push_str(
            "AND (h.cmd LIKE '%' || ?1 || '%' ESCAPE '\\' \
             OR h.cwd LIKE '%' || ?1 || '%' ESCAPE '\\') ",
        );
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
    sql
}

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
    let sql = search_sql(query, dir, exit_filter, session, host, since);
    let mut stmt = conn.prepare(&sql)?;
    let bind_q: Option<String> = if use_fts(query) {
        Some(fts_phrase(query))
    } else if query.is_empty() {
        None
    } else {
        Some(escape_like(query))
    };
    let rows = stmt.query_map(
        params![bind_q.as_deref(), dir, session, host, since, limit],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    rows.collect()
}

/// Representative query plans for CI (`EXPLAIN QUERY PLAN` in gate output).
/// Catches missing-index/schema regressions; shapes mirror search()/suggest().
pub fn search_plan(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let sql = search_sql("bench", None, None, None, None, None);
    let full = format!("EXPLAIN QUERY PLAN {sql}");
    let mut stmt = conn.prepare(&full)?;
    let bind_q = fts_phrase("bench");
    let none: Option<&str> = None;
    let rows = stmt.query_map(
        params![Some(bind_q.as_str()), none, none, none, None::<i64>, 20i64],
        |r| r.get::<_, String>(3),
    )?;
    rows.collect()
}

pub const SUGGEST_SQL: &str = "SELECT cmd,
           (100 * MAX(CASE WHEN cwd = ?2 THEN 1 ELSE 0 END))
           + 10 * SUM(CASE WHEN cwd = ?2 THEN 1 ELSE 0 END)
           + 5 * COUNT(*)
           AS w,
           max(started_at) AS last_seen
         FROM history h
         WHERE cmd LIKE ?1 ESCAPE '\\' AND cmd != ?4
           AND exit_code = 0 AND sensitive = 0
         GROUP BY cmd COLLATE NOCASE
         ORDER BY w DESC, last_seen DESC
         LIMIT ?3";

/// Ghost suggestion: prefix candidates ranked by dir-affinity + frequency + recency.
/// Returns full commands (not suffixes); caller strips the typed prefix for ghost rendering.
///
/// Weight is computed in a single GROUP BY pass (no correlated subqueries:
/// those made each keystroke O(groups × rows)). Only exit-0 runs count
/// toward frequency — failed runs must not promote a suggestion.
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
    // Weight: same-dir hits dominate, frequency follows, recency breaks ties.
    // Over-fetch: control-tainted rows are filtered in Rust below.
    let limit = limit.clamp(0, 1000);
    let pattern = format!("{}%", escape_like(buffer));
    let mut stmt = conn.prepare(SUGGEST_SQL)?;
    let rows = stmt.query_map(
        params![pattern, cwd, limit.saturating_add(16), buffer],
        |r| r.get::<_, String>(0),
    )?;
    Ok(rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|c| !c.chars().any(|ch| ch.is_control()))
        .take(limit as usize)
        .collect())
}

/// Representative suggest plan for CI gate output (shape mirrors suggest()).
pub fn suggest_plan(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let full = format!("EXPLAIN QUERY PLAN {SUGGEST_SQL}");
    let mut stmt = conn.prepare(&full)?;
    let pattern = format!("{}%", escape_like("bench"));
    let rows = stmt.query_map(params![pattern, "/bench", 21i64, "bench"], |r| {
        r.get::<_, String>(3)
    })?;
    rows.collect()
}

/// Schema objects present (tables + indexes). Gate asserts the budgeted
/// objects exist; plans above show how the queries use them.
pub fn schema_objects(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type IN ('table','index') ORDER BY name",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}
