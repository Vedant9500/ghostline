use rusqlite::{Connection, params, params_from_iter};
use std::path::PathBuf;

/// Treat an exported-but-empty XDG variable as unset (the XDG spec says an
/// empty value means "use the default"), so we never create `./ghostline/...`
/// in the caller's current directory.
pub(crate) fn path_from_env(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Default DB path: $XDG_DATA_HOME/ghostline/history.db or ~/.local/share/ghostline/history.db
pub fn default_db_path() -> PathBuf {
    if let Some(xdg) = path_from_env(std::env::var_os("XDG_DATA_HOME")) {
        let mut p = xdg;
        p.push("ghostline/history.db");
        return p;
    }
    let home = path_from_env(std::env::var_os("HOME")).unwrap_or_else(|| PathBuf::from("."));
    let mut p = home;
    p.push(".local/share/ghostline/history.db");
    p
}

/// Per-connection settings. Safe to re-apply on every open.
///
/// `auto_vacuum` must be set before the first page is written; putting it
/// first lets a brand-new database opt into incremental vacuum. Existing
/// databases keep their mode until a full `VACUUM`.
const PRAGMAS: &str = "
PRAGMA auto_vacuum=INCREMENTAL;
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA busy_timeout=5000;
PRAGMA foreign_keys=ON;
";

/// Read-path timeout: a keystroke must never wait five seconds behind a
/// checkpoint. Writers keep the long timeout from `PRAGMAS`.
const READ_BUSY_TIMEOUT_MS: u64 = 100;

/// Current schema version stored in `PRAGMA user_version`.
///
/// Bump this whenever the schema changes and add the matching step to
/// `migrate()`. The cumulative baseline below is also kept current so fresh
/// databases and tests get the latest shape in one batch.
pub const SCHEMA_VERSION: i64 = 3;

/// Cumulative baseline DDL (version 0 -> latest). Idempotent, so re-baselining
/// an existing pre-migration database is safe. Indexes that depend on a column
/// added by a migration (for example `import_key`) live in that migration, not
/// here, so re-baselining a legacy table without the column cannot fail.
pub const SCHEMA: &str = "
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
  sensitive INTEGER NOT NULL DEFAULT 0,
  import_key TEXT             -- stable key for idempotent HISTFILE imports
) STRICT;
CREATE INDEX IF NOT EXISTS idx_time ON history(started_at DESC);
CREATE INDEX IF NOT EXISTS idx_cwd ON history(cwd, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_session ON history(session_id, started_at);
-- Covering partial index for the per-keystroke prefix query in suggest():
-- successful, non-sensitive rows only; cmd/cwd/started_at read from the index.
CREATE INDEX IF NOT EXISTS idx_cmd ON history(cmd COLLATE NOCASE, cwd, started_at DESC)
  WHERE exit_code = 0 AND sensitive = 0;
-- idx_import_key is created by migration_3, after the column exists. Keeping
-- it out of the baseline lets a pre-versioning database (user_version=0) whose
-- table predates import_key still re-baseline without error.
CREATE VIRTUAL TABLE IF NOT EXISTS history_fts USING fts5(cmd, cwd, tokenize='trigram');
CREATE TRIGGER IF NOT EXISTS history_ai AFTER INSERT ON history BEGIN
  INSERT INTO history_fts(rowid, cmd, cwd) VALUES (new.rowid, new.cmd, new.cwd);
END;
CREATE TRIGGER IF NOT EXISTS history_ad AFTER DELETE ON history BEGIN
  DELETE FROM history_fts WHERE rowid = old.rowid;
END;
CREATE TRIGGER IF NOT EXISTS history_au AFTER UPDATE ON history BEGIN
  DELETE FROM history_fts WHERE rowid = old.rowid;
  INSERT INTO history_fts(rowid, cmd, cwd) VALUES (new.rowid, new.cmd, new.cwd);
END;
";

/// Version 1 -> 2: keep the FTS index in sync on updates, and repair the
/// delete trigger. `history_fts` is a regular FTS5 table, so the special
/// `'delete'` insert used by older databases raises "SQL logic error"; a
/// plain `DELETE ... WHERE rowid` is the correct operation.
const MIGRATION_2: &str = "
DROP TRIGGER IF EXISTS history_ad;
CREATE TRIGGER history_ad AFTER DELETE ON history BEGIN
  DELETE FROM history_fts WHERE rowid = old.rowid;
END;
CREATE TRIGGER IF NOT EXISTS history_au AFTER UPDATE ON history BEGIN
  DELETE FROM history_fts WHERE rowid = old.rowid;
  INSERT INTO history_fts(rowid, cmd, cwd) VALUES (new.rowid, new.cmd, new.cwd);
END;
";

/// Version 2 -> 3: add the import dedupe key. The column and partial unique
/// index make re-imports idempotent without affecting logged rows (NULL keys
/// are ignored by the index). `ALTER TABLE` is guarded by a column check so
/// the step is also safe on a database that already has the column.
fn migration_3(conn: &Connection) -> rusqlite::Result<()> {
    let has_column: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('history') WHERE name = 'import_key'",
        [],
        |r| r.get(0),
    )?;
    if has_column == 0 {
        conn.execute_batch("ALTER TABLE history ADD COLUMN import_key TEXT;")?;
    }
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_import_key ON history(import_key)
           WHERE import_key IS NOT NULL;",
    )
}

/// Create missing parent directories (0700 when we own the leaf) and open the
/// database. The main file and its WAL/SHM sidecars are private (0600) because
/// they contain raw command history.
pub fn open(path: &std::path::Path) -> rusqlite::Result<Connection> {
    ensure_parent(path);
    let conn = Connection::open(path)?;
    conn.execute_batch(PRAGMAS)?;
    migrate(&conn)?;
    secure_db_files(path);
    Ok(conn)
}

/// Open an existing database for the read-only hot path (`search`, `suggest`,
/// `fix`). Returns `None` when the file does not exist so a read never creates
/// an empty database. Current schemas skip DDL entirely, use a read-only
/// connection, and get a short busy timeout; an older schema falls back to the
/// writable `open` path so it can still be migrated.
pub fn open_read(path: &std::path::Path) -> rusqlite::Result<Option<Connection>> {
    use rusqlite::OpenFlags;
    if !path.exists() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(std::time::Duration::from_millis(READ_BUSY_TIMEOUT_MS))?;
    conn.execute_batch("PRAGMA query_only=ON;")?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version == SCHEMA_VERSION {
        return Ok(Some(conn));
    }
    drop(conn);
    open(path).map(Some)
}

/// A write transaction that takes SQLite's RESERVED lock at `BEGIN IMMEDIATE`.
/// Deferred transactions can both read and then fight over the lock upgrade;
/// immediate batches queue from the start and respect `busy_timeout` once.
pub(crate) struct WriteTxn<'a> {
    conn: &'a Connection,
    active: bool,
}

impl std::ops::Deref for WriteTxn<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn
    }
}

impl WriteTxn<'_> {
    pub(crate) fn commit(mut self) -> rusqlite::Result<()> {
        self.conn.execute_batch("COMMIT")?;
        self.active = false;
        Ok(())
    }
}

impl Drop for WriteTxn<'_> {
    fn drop(&mut self) {
        if self.active {
            self.conn.execute_batch("ROLLBACK").ok();
        }
    }
}

/// Start an immediate write transaction. Used by batch inserts and `forget`.
pub(crate) fn begin_write(conn: &Connection) -> rusqlite::Result<WriteTxn<'_>> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    Ok(WriteTxn { conn, active: true })
}

fn ensure_parent(path: &std::path::Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if parent.as_os_str().is_empty() {
        return;
    }
    let existed = parent.exists();
    std::fs::create_dir_all(parent).ok();
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::fs::PermissionsExt;
        // A newly created leaf is ours; an existing "ghostline" directory is
        // the default data dir and should never be group/world readable.
        let is_data_dir = parent.file_name() == Some(OsStr::new("ghostline"));
        if !existed || is_data_dir {
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).ok();
        }
    }
}

/// chmod the database and its WAL/SHM sidecars. SQLite creates sidecars with
/// the database's mode at creation time; a later chmod only covers the main
/// file, so re-apply after writes as well.
fn secure_db_files(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = path.as_os_str().to_owned();
            sidecar.push(suffix);
            std::fs::set_permissions(
                std::path::PathBuf::from(sidecar),
                std::fs::Permissions::from_mode(0o600),
            )
            .ok();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Apply pending schema migrations. Version 0 is the pre-`user_version`
/// baseline; applying the idempotent DDL brings it to version 1.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let mut version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!(
                "database schema version {version} is newer than supported version {SCHEMA_VERSION}"
            )),
        ));
    }
    // One block per version step. Keep each step idempotent: databases created
    // before versioning report 0 even though the baseline objects exist.
    if version < 1 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(SCHEMA)?;
        tx.pragma_update(None, "user_version", 1)?;
        tx.commit()?;
        version = 1;
    }
    if version < 2 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_2)?;
        tx.pragma_update(None, "user_version", 2)?;
        tx.commit()?;
        version = 2;
    }
    if version < 3 {
        let tx = conn.unchecked_transaction()?;
        migration_3(&tx)?;
        tx.pragma_update(None, "user_version", 3)?;
        tx.commit()?;
        version = 3;
    }
    debug_assert_eq!(version, SCHEMA_VERSION, "missing migration step");
    Ok(())
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
    /// Stable key for idempotent imports; `None` for commands logged normally.
    pub import_key: Option<String>,
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
    // Imported entries carry import_key; a re-import must skip conflicts
    // rather than abort the batch. Normal log/daemon batches stay strict.
    let dedupe = entries.iter().any(|e| e.import_key.is_some());
    let sql = if dedupe {
        "INSERT INTO history(id,cmd,cwd,exit_code,duration_ns,started_at,ended_at,hostname,session_id,shell,sensitive,import_key)
         VALUES(?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT DO NOTHING"
    } else {
        "INSERT INTO history(id,cmd,cwd,exit_code,duration_ns,started_at,ended_at,hostname,session_id,shell,sensitive,import_key)
         VALUES(?,?,?,?,?,?,?,?,?,?,?,?)"
    };
    let tx = begin_write(conn)?;
    let mut inserted = 0usize;
    {
        let mut stmt = tx.prepare_cached(sql)?;
        for e in entries {
            inserted += stmt.execute(params![
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
                e.import_key,
            ])?;
        }
    }
    tx.commit()?;
    // The -wal/-shm sidecars are (re)created lazily by the first write.
    if let Some(path) = conn.path().filter(|p| !p.is_empty()) {
        secure_db_files(std::path::Path::new(path));
    }
    Ok(inserted)
}

/// Trigram FTS5 needs at least one three-character run; shorter queries use LIKE.
const FTS_MIN_CHARS: usize = 3;

/// Hard cap for search results. SQLite treats a negative LIMIT as "no limit",
/// so clamp before binding; 10k is far above any interactive use.
const SEARCH_MAX_LIMIT: i64 = 10_000;

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
pub(crate) fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Normalize a `--dir` value: strip trailing slashes, keep `/` as root, and
/// treat an empty value as no filter.
fn normalize_dir(dir: &str) -> &str {
    let trimmed = dir.trim_end_matches('/');
    if trimmed.is_empty() && dir.starts_with('/') {
        "/"
    } else {
        trimmed
    }
}

/// Bounds for the cwd subtree of `dir`: `[dir + "/", dir + "0")`, plus the
/// exact `dir` equality at the call site. Comparing raw strings avoids LIKE
/// wildcard escaping and still lets SQLite use `idx_cwd` via a multi-index OR.
fn dir_bounds(dir: &str) -> (String, String) {
    let base = normalize_dir(dir);
    if base == "/" {
        ("/".to_string(), "0".to_string())
    } else {
        (format!("{base}/"), format!("{base}0"))
    }
}

/// Parse an exit filter: `N` means exit code N, `!N` means anything but N.
pub fn parse_exit_filter(raw: &str) -> Option<(bool, i64)> {
    let (negated, digits) = match raw.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, raw),
    };
    digits.parse::<i64>().ok().map(|code| (negated, code))
}

/// Frecency-ish search. FTS5 trigram phrase when the query is matchable,
/// LIKE fallback for 1-2 character queries (trigrams cannot match those).
/// Filters: cwd subtree (`--dir` includes descendants), exit (`N` or `!N`),
/// session, host, since (unix nanoseconds; `cmd_search` converts seconds).
pub fn search_sql(
    query: &str,
    dir: Option<&str>,
    exit_filter: Option<&str>,
    session: Option<&str>,
    host: Option<&str>,
    since: Option<i64>,
) -> String {
    let dir = dir.map(normalize_dir).filter(|d| !d.is_empty());
    // Only parsed integers are interpolated, so this has no injection surface.
    let exit_clause = exit_filter
        .and_then(parse_exit_filter)
        .map(|(negated, code)| {
            if negated {
                format!("AND h.exit_code != {code}")
            } else {
                format!("AND h.exit_code = {code}")
            }
        });
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
        sql.push_str("AND (h.cwd = ?2 OR (h.cwd >= ?9 AND h.cwd < ?10)) ");
    } else {
        sql.push_str("AND ?2 IS NULL ");
    }
    if let Some(clause) = exit_clause {
        sql.push_str(&clause);
        sql.push(' ');
    }
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
    // Rank exact matches first, then prefixes, then exact-cwd affinity, and
    // finally recency. bm25 is deliberately omitted: on a 500k-row database it
    // added ~40ms to a common-token search, while this ordering keeps the
    // result useful and the cost bounded. `?7` is the raw query for equality;
    // `?8` is wildcard-escaped for the prefix LIKE so `%`/`_` stay literal.
    sql.push_str(
        " ORDER BY CASE WHEN ?7 = '' THEN 2 \
                         WHEN h.cmd = ?7 THEN 0 \
                         WHEN h.cmd LIKE ?8 || '%' ESCAPE '\\' THEN 1 \
                         ELSE 2 END, ",
    );
    sql.push_str(
        "CASE WHEN ?2 IS NOT NULL AND h.cwd = ?2 THEN 0 ELSE 1 END, \
         h.started_at DESC, h.id DESC LIMIT ?6",
    );
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
    let dir = dir.map(normalize_dir).filter(|d| !d.is_empty());
    let limit = limit.clamp(0, SEARCH_MAX_LIMIT);
    let sql = search_sql(query, dir, exit_filter, session, host, since);
    let bind_q: Option<String> = if use_fts(query) {
        Some(fts_phrase(query))
    } else if query.is_empty() {
        None
    } else {
        Some(escape_like(query))
    };
    let bounds = dir.map(dir_bounds);
    let raw_query = query.to_string();
    let prefix_query = escape_like(query);
    let mut stmt = conn.prepare(&sql)?;
    let mut binds: Vec<&dyn rusqlite::ToSql> = vec![
        &bind_q,
        &dir,
        &session,
        &host,
        &since,
        &limit,
        &raw_query,
        &prefix_query,
    ];
    if let Some((ref lower, ref upper)) = bounds {
        binds.push(lower);
        binds.push(upper);
    }
    let rows = stmt.query_map(params_from_iter(binds), |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
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
    let raw_query = "bench";
    let prefix_query = escape_like(raw_query);
    let rows = stmt.query_map(
        params![
            Some(bind_q.as_str()),
            none,
            none,
            none,
            None::<i64>,
            20i64,
            raw_query,
            prefix_query
        ],
        |r| r.get::<_, String>(3),
    )?;
    rows.collect()
}

/// Frecency weight by age: a run in the last four days counts as a full use;
/// older runs decay (4d/14d/90d buckets), so 100 runs from last year no longer
/// drown out 10 runs from this week.
pub const SUGGEST_SQL: &str = "SELECT cmd,
           (100.0 * MAX(CASE WHEN cwd = ?2 THEN 1 ELSE 0 END))
           + (10.0 * SUM(CASE WHEN cwd = ?2 THEN 1 ELSE 0 END))
           + SUM(CASE
               WHEN ?5 - started_at <= 345600000000000 THEN 4.0
               WHEN ?5 - started_at <= 1209600000000000 THEN 2.0
               WHEN ?5 - started_at <= 7776000000000000 THEN 1.0
               ELSE 0.25
             END)
           AS w,
           max(started_at) AS last_seen
         FROM history h
         WHERE cmd LIKE ?1 ESCAPE '\\' AND cmd != ?4
           AND exit_code = 0 AND sensitive = 0
         GROUP BY cmd COLLATE NOCASE
         ORDER BY w DESC, last_seen DESC, cmd COLLATE NOCASE
         LIMIT ?3";

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Ghost suggestion: prefix candidates ranked by dir-affinity + decayed
/// frecency. Returns full commands (not suffixes); caller strips the typed
/// prefix for ghost rendering.
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
    suggest_at(conn, buffer, cwd, limit, now_ns())
}

/// `suggest` with an explicit clock for deterministic frecency tests.
pub fn suggest_at(
    conn: &Connection,
    buffer: &str,
    cwd: &str,
    limit: i64,
    now: i64,
) -> rusqlite::Result<Vec<String>> {
    // Weight: same-dir hits dominate; frequency is age-decayed (recent runs
    // count more), and last_seen breaks remaining ties.
    // Control-tainted rows are filtered in Rust; if they crowd out clean
    // candidates, widen the over-fetch until enough clean rows are seen or the
    // prefix is exhausted. Write-time sanitization keeps new rows clean, so the
    // common case still costs one query.
    let limit = limit.clamp(0, 1000);
    if limit == 0 {
        return Ok(Vec::new());
    }
    let pattern = format!("{}%", escape_like(buffer));
    let mut stmt = conn.prepare(SUGGEST_SQL)?;
    let mut fetch = limit.saturating_add(16);
    loop {
        let raw: Vec<String> = stmt
            .query_map(params![pattern, cwd, fetch, buffer, now], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let exhausted = (raw.len() as i64) < fetch;
        let clean: Vec<String> = raw
            .into_iter()
            .filter(|c| !c.chars().any(|ch| ch.is_control()))
            .take(limit as usize)
            .collect();
        if clean.len() as i64 >= limit || exhausted {
            return Ok(clean);
        }
        fetch = fetch.saturating_mul(2);
    }
}

/// Representative suggest plan for CI gate output (shape mirrors suggest()).
pub fn suggest_plan(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let full = format!("EXPLAIN QUERY PLAN {SUGGEST_SQL}");
    let mut stmt = conn.prepare(&full)?;
    let pattern = format!("{}%", escape_like("bench"));
    let rows = stmt.query_map(params![pattern, "/bench", 21i64, "bench", 0i64], |r| {
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

/// Refresh planner statistics after a bulk load. `ANALYZE` is a full pass over
/// the indexed tables, so only the import/bench paths call it.
pub fn analyze(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("ANALYZE;")
}

/// Cheap periodic maintenance for long-lived writers. SQLite decides whether
/// any table needs `ANALYZE`; safe to call after a daemon batch.
pub fn optimize(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("PRAGMA optimize;")
}

/// What `forget` should remove. Selectors are intentionally explicit: a
/// bare pattern, a time cutoff, the newest N rows, or everything.
pub enum ForgetMode<'a> {
    Patterns(&'a [String]),
    LastN(i64),
    OlderThanDays(i64),
    MaxRows(i64),
    All,
}

pub struct ForgetReport {
    pub matched: i64,
    pub deleted: i64,
    /// Newest matches, for `--dry-run` previews.
    pub rows: Vec<(i64, i64, String, String)>,
}

/// Delete history rows selected by `mode`. `dry_run` reports what would be
/// removed without deleting. FTS delete triggers keep the search index in sync.
pub fn forget(
    conn: &Connection,
    mode: &ForgetMode<'_>,
    dry_run: bool,
) -> rusqlite::Result<ForgetReport> {
    use rusqlite::types::Value;

    let mut params: Vec<Value> = Vec::new();
    let where_sql = match mode {
        ForgetMode::Patterns(patterns) => {
            if patterns.is_empty() {
                return Ok(ForgetReport {
                    matched: 0,
                    deleted: 0,
                    rows: Vec::new(),
                });
            }
            let mut clauses = Vec::with_capacity(patterns.len());
            for (i, pattern) in patterns.iter().enumerate() {
                params.push(Value::Text(format!("%{}%", escape_like(pattern))));
                clauses.push(format!("cmd LIKE ?{} ESCAPE '\\'", i + 1));
            }
            format!("WHERE ({})", clauses.join(" OR "))
        }
        ForgetMode::LastN(n) => {
            params.push(Value::Integer((*n).max(0)));
            "WHERE id IN (SELECT id FROM history ORDER BY started_at DESC, id DESC LIMIT ?1)"
                .to_string()
        }
        ForgetMode::OlderThanDays(days) => {
            let cutoff = now_ns().saturating_sub(
                (*days).max(0)
                    .saturating_mul(86_400)
                    .saturating_mul(1_000_000_000),
            );
            params.push(Value::Integer(cutoff));
            "WHERE started_at < ?1".to_string()
        }
        ForgetMode::MaxRows(n) => {
            params.push(Value::Integer((*n).max(0)));
            "WHERE id NOT IN (SELECT id FROM history ORDER BY started_at DESC, id DESC LIMIT ?1)"
                .to_string()
        }
        ForgetMode::All => String::new(),
    };

    let count_sql = format!("SELECT count(*) FROM history {where_sql}");
    if dry_run {
        let matched: i64 =
            conn.query_row(&count_sql, params_from_iter(params.iter()), |r| r.get(0))?;
        let sql = format!(
            "SELECT started_at, exit_code, cwd, cmd FROM history {where_sql} \
             ORDER BY started_at DESC, id DESC LIMIT 20"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(params.iter()), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        return Ok(ForgetReport {
            matched,
            deleted: 0,
            rows,
        });
    }
    // Count and delete under the same immediate transaction so a concurrent
    // daemon insert cannot make the report inconsistent.
    let tx = begin_write(conn)?;
    let matched: i64 = tx.query_row(&count_sql, params_from_iter(params.iter()), |r| r.get(0))?;
    let deleted = tx.execute(
        &format!("DELETE FROM history {where_sql}"),
        params_from_iter(params),
    )?;
    tx.commit()?;
    Ok(ForgetReport {
        matched,
        deleted: deleted as i64,
        rows: Vec::new(),
    })
}

/// Reclaim free pages after a delete. `incremental_vacuum` is a no-op on
/// databases created before auto-vacuum was enabled; `--vacuum` forces a full
/// rewrite, which also converts an old database to incremental mode.
pub fn vacuum(conn: &Connection, full: bool) -> rusqlite::Result<()> {
    if full {
        conn.execute_batch("VACUUM;")
    } else {
        conn.execute_batch("PRAGMA incremental_vacuum;")
    }
}
