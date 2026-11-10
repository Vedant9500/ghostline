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
pub const SCHEMA_VERSION: i64 = 6;

/// Latest `history` table. `id` is a 16-byte UUIDv7 BLOB: it is internal, so
/// the compact form saves the 36-byte text index without changing ordering
/// (UUIDv7 bytes sort chronologically).
fn history_table_ddl(name: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {name}(
  id BLOB PRIMARY KEY,
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
) STRICT;"
    )
}

/// Cumulative baseline DDL (version 0 -> latest). Idempotent, so re-baselining
/// an existing pre-migration database is safe. Indexes that depend on a column
/// added by a migration (for example `import_key`) live in that migration, not
/// here, so re-baselining a legacy table without the column cannot fail.
///
/// Suggestion stats (`cmd_stats`/`cmd_dir_stats`) are maintained by triggers:
/// they contain only successful, non-sensitive rows, exactly the set suggest
/// may offer. `cmd_index`/`cwd_index` are distinct-value sets for the 1-2
/// character search path; stale values are harmless because the final query
/// still reads `history`.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS history(
  id BLOB PRIMARY KEY,
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
  import_key TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS idx_time ON history(started_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_cwd ON history(cwd, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_session ON history(session_id, started_at);
-- Covering partial index for legacy/fallback prefix lookups.
CREATE INDEX IF NOT EXISTS idx_cmd ON history(cmd COLLATE NOCASE, cwd, started_at DESC)
  WHERE exit_code = 0 AND sensitive = 0;
-- Full command index for the 1-2 character search path (all rows, not partial).
CREATE INDEX IF NOT EXISTS idx_cmd_all ON history(cmd COLLATE NOCASE, started_at DESC);
-- idx_import_key is created by migration_3, after the column exists. Keeping
-- it out of the baseline lets a pre-versioning database (user_version=0) whose
-- table predates import_key still re-baseline without error.

-- External-content FTS: the trigram index stores postings only, not a second
-- copy of every command. Triggers must use the special 'delete' command for
-- external content, hence the old plain DELETE is only used by migration 2.
CREATE VIRTUAL TABLE IF NOT EXISTS history_fts USING fts5(
  cmd, cwd, content='history', content_rowid='rowid', tokenize='trigram'
);
CREATE TRIGGER IF NOT EXISTS history_ai AFTER INSERT ON history BEGIN
  INSERT INTO history_fts(rowid, cmd, cwd) VALUES (new.rowid, new.cmd, new.cwd);
END;
CREATE TRIGGER IF NOT EXISTS history_ad AFTER DELETE ON history BEGIN
  INSERT INTO history_fts(history_fts, rowid, cmd, cwd)
    VALUES ('delete', old.rowid, old.cmd, old.cwd);
END;
CREATE TRIGGER IF NOT EXISTS history_au AFTER UPDATE ON history BEGIN
  INSERT INTO history_fts(history_fts, rowid, cmd, cwd)
    VALUES ('delete', old.rowid, old.cmd, old.cwd);
  INSERT INTO history_fts(rowid, cmd, cwd) VALUES (new.rowid, new.cmd, new.cwd);
END;

-- Precomputed suggestion aggregates (no scan over history on a keystroke).
CREATE TABLE IF NOT EXISTS cmd_stats(
  cmd TEXT PRIMARY KEY COLLATE NOCASE,
  n INTEGER NOT NULL,
  last_seen INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS cmd_dir_stats(
  cmd TEXT NOT NULL COLLATE NOCASE,
  cwd TEXT NOT NULL,
  n INTEGER NOT NULL,
  last_seen INTEGER NOT NULL,
  PRIMARY KEY (cmd, cwd)
) STRICT, WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS cmd_index(
  cmd TEXT PRIMARY KEY COLLATE NOCASE
) STRICT, WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS cwd_index(
  cwd TEXT PRIMARY KEY
) STRICT, WITHOUT ROWID;

CREATE TRIGGER IF NOT EXISTS history_stats_ai AFTER INSERT ON history
WHEN new.exit_code = 0 AND new.sensitive = 0
BEGIN
  INSERT INTO cmd_stats(cmd, n, last_seen) VALUES(new.cmd, 1, new.started_at)
    ON CONFLICT(cmd) DO UPDATE SET
      n = n + 1, last_seen = max(last_seen, excluded.last_seen);
  INSERT INTO cmd_dir_stats(cmd, cwd, n, last_seen)
    VALUES(new.cmd, new.cwd, 1, new.started_at)
    ON CONFLICT(cmd, cwd) DO UPDATE SET
      n = n + 1, last_seen = max(last_seen, excluded.last_seen);
END;
CREATE TRIGGER IF NOT EXISTS history_stats_ad AFTER DELETE ON history
WHEN old.exit_code = 0 AND old.sensitive = 0
BEGIN
  UPDATE cmd_stats SET
    n = n - 1,
    last_seen = CASE WHEN old.started_at >= last_seen
      THEN COALESCE((SELECT max(started_at) FROM history
                     WHERE cmd = old.cmd COLLATE NOCASE
                       AND exit_code = 0 AND sensitive = 0), last_seen)
      ELSE last_seen END
  WHERE cmd = old.cmd COLLATE NOCASE;
  DELETE FROM cmd_stats WHERE cmd = old.cmd COLLATE NOCASE AND n <= 0;
  UPDATE cmd_dir_stats SET
    n = n - 1,
    last_seen = CASE WHEN old.started_at >= last_seen
      THEN COALESCE((SELECT max(started_at) FROM history
                     WHERE cmd = old.cmd COLLATE NOCASE AND cwd = old.cwd
                       AND exit_code = 0 AND sensitive = 0), last_seen)
      ELSE last_seen END
  WHERE cmd = old.cmd COLLATE NOCASE AND cwd = old.cwd;
  DELETE FROM cmd_dir_stats
    WHERE cmd = old.cmd COLLATE NOCASE AND cwd = old.cwd AND n <= 0;
END;
-- UPDATE needs separate remove/add triggers so the aggregate stays exact when
-- a redaction or future edit changes cmd/cwd/exit/sensitive/started_at.
CREATE TRIGGER IF NOT EXISTS history_stats_au_remove AFTER UPDATE ON history
WHEN old.exit_code = 0 AND old.sensitive = 0
BEGIN
  UPDATE cmd_stats SET
    n = n - 1,
    last_seen = CASE WHEN old.started_at >= last_seen
      THEN COALESCE((SELECT max(started_at) FROM history
                     WHERE cmd = old.cmd COLLATE NOCASE
                       AND exit_code = 0 AND sensitive = 0), last_seen)
      ELSE last_seen END
  WHERE cmd = old.cmd COLLATE NOCASE;
  DELETE FROM cmd_stats WHERE cmd = old.cmd COLLATE NOCASE AND n <= 0;
  UPDATE cmd_dir_stats SET
    n = n - 1,
    last_seen = CASE WHEN old.started_at >= last_seen
      THEN COALESCE((SELECT max(started_at) FROM history
                     WHERE cmd = old.cmd COLLATE NOCASE AND cwd = old.cwd
                       AND exit_code = 0 AND sensitive = 0), last_seen)
      ELSE last_seen END
  WHERE cmd = old.cmd COLLATE NOCASE AND cwd = old.cwd;
  DELETE FROM cmd_dir_stats
    WHERE cmd = old.cmd COLLATE NOCASE AND cwd = old.cwd AND n <= 0;
END;
CREATE TRIGGER IF NOT EXISTS history_stats_au_add AFTER UPDATE ON history
WHEN new.exit_code = 0 AND new.sensitive = 0
BEGIN
  INSERT INTO cmd_stats(cmd, n, last_seen) VALUES(new.cmd, 1, new.started_at)
    ON CONFLICT(cmd) DO UPDATE SET
      n = n + 1, last_seen = max(last_seen, excluded.last_seen);
  INSERT INTO cmd_dir_stats(cmd, cwd, n, last_seen)
    VALUES(new.cmd, new.cwd, 1, new.started_at)
    ON CONFLICT(cmd, cwd) DO UPDATE SET
      n = n + 1, last_seen = max(last_seen, excluded.last_seen);
END;

-- Distinct-value sets for short search. Stale entries only cause harmless
-- index probes; the query still selects from history.
CREATE TRIGGER IF NOT EXISTS history_index_ai AFTER INSERT ON history BEGIN
  INSERT OR IGNORE INTO cmd_index(cmd) VALUES(new.cmd);
  INSERT OR IGNORE INTO cwd_index(cwd) VALUES(new.cwd);
END;
CREATE TRIGGER IF NOT EXISTS history_index_au AFTER UPDATE ON history BEGIN
  INSERT OR IGNORE INTO cmd_index(cmd) VALUES(new.cmd);
  INSERT OR IGNORE INTO cwd_index(cwd) VALUES(new.cwd);
END;
";

/// Version 1 -> 2: keep the FTS index in sync on updates, and repair the
/// delete trigger. `history_fts` is a regular FTS5 table, so the special
/// `'delete'` insert used by older databases raises "SQL logic error"; a
/// plain `DELETE ... WHERE rowid` is the correct operation. This step is
/// skipped when the table is already external-content (the v4 conversion
/// installs the special-command triggers itself).
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

/// Turn a stored UUID into the 16-byte BLOB form. Malformed legacy ids are
/// regenerated: the id is internal, never displayed, and only needs to be
/// unique and roughly time-ordered.
fn uuid_blob(value: rusqlite::types::Value) -> Vec<u8> {
    use rusqlite::types::Value;
    match value {
        Value::Blob(b) if b.len() == 16 => b,
        Value::Text(s) => uuid::Uuid::parse_str(&s)
            .map(|u| u.as_bytes().to_vec())
            .unwrap_or_else(|_| uuid::Uuid::now_v7().as_bytes().to_vec()),
        _ => uuid::Uuid::now_v7().as_bytes().to_vec(),
    }
}

/// Rebuild `history` with a BLOB primary key. SQLite cannot change a column
/// type in place, and the table rebuild is also the cleanest way to drop the
/// old UUID-text index. All columns are copied; FTS/indexes/triggers are
/// recreated by `SCHEMA` afterwards.
fn rebuild_history_blob_ids(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS history_ai;
         DROP TRIGGER IF EXISTS history_ad;
         DROP TRIGGER IF EXISTS history_au;
         DROP TABLE IF EXISTS history_fts;
         DROP TABLE IF EXISTS history_new;",
    )?;
    conn.execute_batch(&history_table_ddl("history_new"))?;
    {
        let mut select = conn.prepare(
            "SELECT id, cmd, cwd, exit_code, duration_ns, started_at, ended_at,
                    hostname, session_id, shell, tty, ssh_ctx, tmux, container,
                    sudo_user, git_repo, git_branch, sensitive, import_key
               FROM history",
        )?;
        let mut rows = select.query([])?;
        let mut insert = conn.prepare(
            "INSERT INTO history_new(
                id, cmd, cwd, exit_code, duration_ns, started_at, ended_at,
                hostname, session_id, shell, tty, ssh_ctx, tmux, container,
                sudo_user, git_repo, git_branch, sensitive, import_key
             ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )?;
        while let Some(row) = rows.next()? {
            let id = uuid_blob(row.get(0)?);
            insert.execute(params![
                id,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<String>>(13)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, Option<String>>(16)?,
                row.get::<_, i64>(17)?,
                row.get::<_, Option<String>>(18)?,
            ])?;
        }
    }
    conn.execute_batch(
        "DROP TABLE history;
         ALTER TABLE history_new RENAME TO history;",
    )
}

/// Version 3 -> 4: BLOB UUIDs and external-content FTS. The table rebuild and
/// FTS rebuild are done in one step so a 500k-row database pays the copy cost
/// once. `SCHEMA` recreates the latest indexes/triggers/stats tables.
fn migration_4(conn: &Connection) -> rusqlite::Result<()> {
    let id_type: String = conn.query_row(
        "SELECT type FROM pragma_table_info('history') WHERE name = 'id'",
        [],
        |r| r.get(0),
    )?;
    if !id_type.eq_ignore_ascii_case("BLOB") {
        rebuild_history_blob_ids(conn)?;
    }
    // Drop the old regular-content FTS (or a stale external one) and let the
    // latest baseline recreate external-content FTS + all triggers.
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS history_ai;
         DROP TRIGGER IF EXISTS history_ad;
         DROP TRIGGER IF EXISTS history_au;
         DROP TABLE IF EXISTS history_fts;",
    )?;
    conn.execute_batch(SCHEMA)?;
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_import_key ON history(import_key)
           WHERE import_key IS NOT NULL;
         INSERT INTO history_fts(history_fts) VALUES('rebuild');",
    )
}

/// Version 5 -> 6: make the time index cover `id` as well. Broad 1-2 char
/// searches then scan this index in recency order and stop at the LIMIT,
/// without a temp sort over every match.
fn migration_6(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_time;
         CREATE INDEX idx_time ON history(started_at DESC, id DESC);",
    )
}

/// Version 4 -> 5: backfill the suggestion aggregates and short-search
/// distinct sets. Triggers keep them current for all later inserts/deletes.
fn migration_5(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)?;
    conn.execute_batch(
        "DELETE FROM cmd_stats;
         DELETE FROM cmd_dir_stats;
         DELETE FROM cmd_index;
         DELETE FROM cwd_index;
         INSERT INTO cmd_stats(cmd, n, last_seen)
           SELECT cmd, count(*), max(started_at) FROM history
            WHERE exit_code = 0 AND sensitive = 0
            GROUP BY cmd COLLATE NOCASE;
         INSERT INTO cmd_dir_stats(cmd, cwd, n, last_seen)
           SELECT cmd, cwd, count(*), max(started_at) FROM history
            WHERE exit_code = 0 AND sensitive = 0
            GROUP BY cmd COLLATE NOCASE, cwd;
         INSERT OR IGNORE INTO cmd_index(cmd) SELECT DISTINCT cmd FROM history;
         INSERT OR IGNORE INTO cwd_index(cwd) SELECT DISTINCT cwd FROM history;",
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

/// Surface permission failures instead of silently leaving history readable.
/// A warning is used rather than an error because a successful insert must not
/// be reported as failed just because a filesystem refuses a chmod.
#[cfg(unix)]
fn warn_permission(path: &std::path::Path, result: std::io::Result<()>) {
    if let Err(e) = result {
        eprintln!(
            "ghostline: warning: could not set private permissions on {}: {e}",
            path.display()
        );
    }
}

fn ensure_parent(path: &std::path::Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if parent.as_os_str().is_empty() {
        return;
    }
    let existed = parent.exists();
    if let Err(e) = std::fs::create_dir_all(parent) {
        eprintln!(
            "ghostline: warning: could not create data directory {}: {e}",
            parent.display()
        );
    }
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::fs::PermissionsExt;
        // A newly created leaf is ours; an existing "ghostline" directory is
        // the default data dir and should never be group/world readable.
        let is_data_dir = parent.file_name() == Some(OsStr::new("ghostline"));
        if !existed || is_data_dir {
            warn_permission(
                parent,
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)),
            );
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
        warn_permission(
            path,
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)),
        );
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = path.as_os_str().to_owned();
            sidecar.push(suffix);
            let sidecar = std::path::PathBuf::from(sidecar);
            if sidecar.exists() {
                warn_permission(
                    &sidecar,
                    std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o600)),
                );
            }
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
    if version == 0 {
        let history_exists: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='history'",
            [],
            |r| r.get(0),
        )?;
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(SCHEMA)?;
        if history_exists == 0 {
            // Fresh database: the baseline is already the latest shape, so do
            // not replay the historical steps (and do not pay for a rebuild).
            // idx_import_key is normally created by migration_3; a fresh DB
            // skips that step, so create it here.
            tx.execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_import_key ON history(import_key)
                   WHERE import_key IS NOT NULL;",
            )?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
            return Ok(());
        }
        tx.pragma_update(None, "user_version", 1)?;
        tx.commit()?;
        version = 1;
    }
    if version < 2 {
        let tx = conn.unchecked_transaction()?;
        let external: i64 = tx.query_row(
            "SELECT count(*) FROM sqlite_master
              WHERE name = 'history_fts' AND sql LIKE '%content=%'",
            [],
            |r| r.get(0),
        )?;
        if external == 0 {
            tx.execute_batch(MIGRATION_2)?;
        }
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
    if version < 4 {
        let tx = conn.unchecked_transaction()?;
        migration_4(&tx)?;
        tx.pragma_update(None, "user_version", 4)?;
        tx.commit()?;
        version = 4;
    }
    if version < 5 {
        let tx = conn.unchecked_transaction()?;
        migration_5(&tx)?;
        tx.pragma_update(None, "user_version", 5)?;
        tx.commit()?;
        version = 5;
    }
    if version < 6 {
        let tx = conn.unchecked_transaction()?;
        migration_6(&tx)?;
        tx.pragma_update(None, "user_version", 6)?;
        tx.commit()?;
        version = 6;
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
                uuid::Uuid::now_v7().as_bytes().to_vec(),
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

/// Frecency-ish search. FTS5 trigram phrase when the query is matchable.
/// 1-2 character queries have two shapes: a rare/miss query probes the distinct
/// `cmd_index`/`cwd_index` sets, while a broad query walks `idx_time` in
/// recency order and stops at the LIMIT (no temp sort over every match).
/// Filters: cwd subtree (`--dir` includes descendants), exit (`N` or `!N`),
/// session, host, since (unix nanoseconds; `cmd_search` converts seconds).
pub fn search_sql(
    query: &str,
    dir: Option<&str>,
    exit_filter: Option<&str>,
    session: Option<&str>,
    host: Option<&str>,
    since: Option<i64>,
    short_recent: bool,
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
    let short = !fts && !query.is_empty();
    let indexed_short = short && !short_recent;
    let recent_short = short && short_recent;
    // For a rare short query, force index probes through a candidate-rowid
    // CTE. A plain multi-index OR lets SQLite fall back to `SCAN h` when the
    // subqueries are empty, which is exactly the 500k-row miss case.
    let mut sql = if indexed_short {
        String::from(
            "WITH candidates(rowid) AS (
               SELECT h.rowid FROM history h INDEXED BY idx_cmd_all
                WHERE h.cmd COLLATE NOCASE IN (
                  SELECT cmd FROM cmd_index
                   WHERE cmd LIKE '%' || ?1 || '%' ESCAPE '\\'
                )
               UNION
               SELECT h.rowid FROM history h INDEXED BY idx_cwd
                WHERE h.cwd IN (
                  SELECT cwd FROM cwd_index
                   WHERE cwd LIKE '%' || ?1 || '%' ESCAPE '\\'
                )
             )
             SELECT h.started_at, h.exit_code, h.cwd, h.cmd
               FROM history h JOIN candidates c ON c.rowid = h.rowid ",
        )
    } else if recent_short {
        String::from(
            "SELECT h.started_at, h.exit_code, h.cwd, h.cmd
               FROM history h INDEXED BY idx_time ",
        )
    } else {
        String::from("SELECT h.started_at, h.exit_code, h.cwd, h.cmd FROM history h ")
    };
    if fts {
        sql.push_str("JOIN history_fts ON history_fts.rowid = h.rowid ");
    }
    sql.push_str("WHERE 1=1 ");
    if fts {
        sql.push_str("AND history_fts MATCH ?1 ");
    } else if recent_short {
        // Broad short query: recency order + LIMIT, so the index walk can
        // stop after the first 20 matches instead of sorting all of them.
        sql.push_str(
            "AND (h.cmd LIKE '%' || ?1 || '%' ESCAPE '\\' \
             OR h.cwd LIKE '%' || ?1 || '%' ESCAPE '\\') \
             AND ?7 IS NOT NULL AND ?8 IS NOT NULL ",
        );
    } else if !indexed_short {
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
    if recent_short {
        // `idx_time` includes `id`, so this is a straight index walk with no
        // temp b-tree and an early exit at LIMIT.
        sql.push_str(" ORDER BY h.started_at DESC, h.id DESC LIMIT ?6");
        return sql;
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

/// Above this many candidate history rows, materializing every matching rowid
/// costs more than walking the time index. 256 is well above the 20-result
/// limit while keeping the rare/miss index path tiny.
const SHORT_INDEX_CANDIDATE_CAP: i64 = 257;

/// `None` means there is no matching command or cwd at all, so history cannot
/// match either; `Some(true)` selects the broad time-index path. Counting is
/// capped at 257 rows, so a common command/cwd stops early instead of scanning
/// the full history twice.
fn short_match_strategy(conn: &Connection, query: &str) -> rusqlite::Result<Option<bool>> {
    let pattern = format!("%{}%", escape_like(query));
    let count: i64 = conn.query_row(
        "SELECT
           (SELECT count(*) FROM (
              SELECT 1 FROM history h INDEXED BY idx_cmd_all
               WHERE h.cmd COLLATE NOCASE IN (
                 SELECT cmd FROM cmd_index WHERE cmd LIKE ?1 ESCAPE '\\'
               ) LIMIT ?2))
         + (SELECT count(*) FROM (
              SELECT 1 FROM history h INDEXED BY idx_cwd
               WHERE h.cwd IN (
                 SELECT cwd FROM cwd_index WHERE cwd LIKE ?1 ESCAPE '\\'
               ) LIMIT ?2))",
        params![pattern, SHORT_INDEX_CANDIDATE_CAP],
        |r| r.get(0),
    )?;
    if count == 0 {
        Ok(None)
    } else {
        Ok(Some(count >= SHORT_INDEX_CANDIDATE_CAP))
    }
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
    let short_recent = if !use_fts(query) && !query.is_empty() {
        match short_match_strategy(conn, query)? {
            Some(broad) => broad,
            None => return Ok(Vec::new()),
        }
    } else {
        false
    };
    let sql = search_sql(query, dir, exit_filter, session, host, since, short_recent);
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
    let sql = search_sql("bench", None, None, None, None, None, false);
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

/// Query plan for the 1-2 character path; must probe `idx_cmd_all`/`idx_cwd`
/// rather than scan `history`.
pub fn search_short_plan(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let sql = search_sql("00", None, None, None, None, None, false);
    let full = format!("EXPLAIN QUERY PLAN {sql}");
    let mut stmt = conn.prepare(&full)?;
    let bind_q = escape_like("00");
    let none: Option<&str> = None;
    let rows = stmt.query_map(
        params![
            Some(bind_q.as_str()),
            none,
            none,
            none,
            None::<i64>,
            20i64,
            "00",
            "00"
        ],
        |r| r.get::<_, String>(3),
    )?;
    rows.collect()
}

/// Query plan for a broad 1-2 character search; must walk `idx_time` and avoid
/// a temp sort over every match.
pub fn search_short_recent_plan(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let sql = search_sql("c", None, None, None, None, None, true);
    let full = format!("EXPLAIN QUERY PLAN {sql}");
    let mut stmt = conn.prepare(&full)?;
    let bind_q = escape_like("c");
    let none: Option<&str> = None;
    let rows = stmt.query_map(
        params![
            Some(bind_q.as_str()),
            none,
            none,
            none,
            None::<i64>,
            20i64,
            "c",
            "c"
        ],
        |r| r.get::<_, String>(3),
    )?;
    rows.collect()
}

/// Frecency weight by age: a run in the last four days counts as a full use;
/// older runs decay (4d/14d/90d buckets), so 100 runs from last year no longer
/// drown out 10 runs from this week.
///
/// This reads the precomputed aggregates only; the per-keystroke cost is the
/// number of distinct commands sharing the prefix, never the history size.
/// `cmd` is a NOCASE primary key, so the LIKE prefix is an index range scan.
pub const SUGGEST_SQL: &str = "SELECT cmd_stats.cmd,
           (100.0 * CASE WHEN d.n IS NOT NULL THEN 1 ELSE 0 END)
           + (10.0 * COALESCE(d.n, 0))
           + (cmd_stats.n * CASE
               WHEN ?5 - cmd_stats.last_seen <= 345600000000000 THEN 4.0
               WHEN ?5 - cmd_stats.last_seen <= 1209600000000000 THEN 2.0
               WHEN ?5 - cmd_stats.last_seen <= 7776000000000000 THEN 1.0
               ELSE 0.25
             END)
           AS w,
           cmd_stats.last_seen
         FROM cmd_stats
         LEFT JOIN cmd_dir_stats d
           ON d.cmd = cmd_stats.cmd AND d.cwd = ?2
         WHERE cmd_stats.cmd LIKE ?1 ESCAPE '\\' AND cmd_stats.cmd != ?4
         ORDER BY w DESC, cmd_stats.last_seen DESC, cmd_stats.cmd COLLATE NOCASE
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
/// Weight is computed from triggers-maintained aggregates (`cmd_stats` /
/// `cmd_dir_stats`); no history scan and no per-keystroke GROUP BY. Only
/// exit-0, non-sensitive runs count toward frequency — failed runs must not
/// promote a suggestion.
///
/// SAFETY: candidates containing control characters (ESC, newline, BEL, ...) are
/// silently dropped. A ghost is painted raw onto the terminal every keystroke;
/// replaying an entry like `echo -e '\e[7m..'` would fire inverse video, cursor
/// jumps and scrolls — i.e. whole-terminal flicker plus readline desync.
/// Destructive commands (`git push --force`, `rm -rf`, ...) are also filtered:
/// docs/07 forbids offering footguns even when the user typed their prefix.
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
    // count more), and last_seen breaks remaining ties. Aggregates are already
    // filtered to successful/non-sensitive rows by the triggers.
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
            .filter(|c| !c.chars().any(|ch| ch.is_control()) && !crate::redact::is_destructive(c))
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
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type IN ('table','index') ORDER BY name")?;
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
                (*days)
                    .max(0)
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
