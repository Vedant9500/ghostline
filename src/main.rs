mod cmd_bench;
mod cmd_daemon;
mod cmd_forget;
mod complete;
mod cmd_fix;
mod cmd_import;
mod cmd_init;
mod cmd_log;
mod cmd_search;
mod cmd_suggest;
mod db;
mod fix;
mod queue;
mod redact;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn usage() -> &'static str {
    "ghostline — near-instant shell history + ghost suggestions

Usage:
  ghostline init <bash|zsh|fish>      print shell hook (eval \"$(ghostline init bash)\")
  ghostline log [flags] <command...>  log one command (or --queue for queue-append)
  ghostline daemon [--once]           drain queue into SQLite (or run loop)
  ghostline import [--from HISTFILE]  bulk-import shell history
  ghostline forget [flags] [PATTERN...] delete history rows (or preview with --dry-run)
  ghostline fix [--last] [command...]  did-you-mean repair (exit 0 = found)
  ghostline search [flags] [query]    search history (tab-separated: ts, exit, cwd, cmd)
  ghostline suggest [flags] <buffer>  print ghost suffix for buffer (or --all for full cmds)
  ghostline bench [flags]             run perf budgets (see docs/08-performance-budgets.md)

log flags:     --shell S --cwd DIR --exit CODE --session ID [--db PATH] [--queue] [--queue-dir D]
daemon flags:  [--once] [--interval-ms N] [--queue-dir D] [--db PATH]
import flags:  [--from HISTFILE] [--db PATH] [--shell S] [--no-dedupe]
forget flags:  [--db P] [--dry-run] [--vacuum] [--last-n N | --older-than DAYS | --max-rows N | --all | PATTERN...]
fix flags:     [--last] [--exit CODE] [--cwd DIR] [--db P] [-q]
search flags:  [--limit N] [--dir DIR] [--exit N|!N] [--session S] [--host H] [--since UNIX_SECS] [--db PATH]
suggest flags: [--cwd DIR] [--limit N] [--all] [--db PATH]
bench flags:   [--n N] [--reps R] [--concurrency C] [--json] [--db PATH] [--gate]
               [--max-log-ms X] [--max-search-ms X] [--max-suggest-ms X] [--max-hook-ms X] [--min-inserts-per-s X]

Env: GHOSTLINE_IGNORE (glob list), GHOSTLINE_DISABLED=1 to pause logging, GHOSTLINE_QUEUE=1 for queue-append."
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(|s| s.as_str()).unwrap_or("");
    let rest = if args.len() > 2 { &args[2..] } else { &[] };
    let code = match sub {
        "init" => match rest.first().map(|s| s.as_str()) {
            Some(sh) => match cmd_init::snippet(sh) {
                Ok(s) => {
                    print!("{s}");
                    0
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    2
                }
            },
            None => {
                eprintln!("usage: ghostline init <bash|zsh|fish>");
                2
            }
        },
        "log" => match cmd_log::run(rest) {
            Ok(_) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        "search" => match cmd_search::run(rest) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        "daemon" => match cmd_daemon::run(rest) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        "import" => match cmd_import::run(rest) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        "forget" => match cmd_forget::run(rest) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        "fix" => match cmd_fix::run(rest) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        "suggest" => match cmd_suggest::run(rest) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        "bench" => match cmd_bench::run_cli(rest) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        "--help" | "-h" | "help" | "" => {
            println!("{}", usage());
            0
        }
        "--version" | "-V" | "version" => {
            println!("ghostline {VERSION}");
            0
        }
        other => {
            eprintln!("unknown subcommand '{other}'\n{}", usage());
            2
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod integration {
    use crate::db;
    use rusqlite::Connection;

    fn memdb() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(db::SCHEMA).unwrap();
        conn
    }

    fn entry(cmd: &str, cwd: &str, exit: i64, ts: i64) -> db::Entry {
        db::Entry {
            cmd: cmd.into(),
            cwd: cwd.into(),
            exit_code: exit,
            duration_ns: 0,
            started_at: ts,
            ended_at: ts,
            hostname: "h".into(),
            session_id: "s".into(),
            shell: "bash".into(),
            sensitive: false,
            import_key: None,
        }
    }

    #[test]
    fn log_then_search_finds_it() {
        let conn = memdb();
        db::insert(&conn, &entry("git checkout main", "/repo", 0, 100)).unwrap();
        let rows = db::search(&conn, "checkout", 10, None, None, None, None, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].3, "git checkout main");
    }

    #[test]
    fn suggest_prefers_same_dir() {
        let conn = memdb();
        db::insert(&conn, &entry("cargo test --all", "/rust", 0, 300)).unwrap();
        db::insert(&conn, &entry("cargo test --all", "/rust", 0, 301)).unwrap();
        db::insert(&conn, &entry("cargo test --all", "/web", 0, 302)).unwrap();
        db::insert(&conn, &entry("cargo build", "/rust", 0, 303)).unwrap();
        let cands = db::suggest(&conn, "cargo ", "/rust", 5).unwrap();
        assert!(!cands.is_empty());
        assert_eq!(cands[0], "cargo test --all");
    }

    #[test]
    fn suggest_ignores_failed_commands() {
        let conn = memdb();
        db::insert(&conn, &entry("deploy --prod", "/repo", 1, 100)).unwrap();
        let cands = db::suggest(&conn, "deploy", "/repo", 5).unwrap();
        assert!(cands.is_empty());
    }

    #[test]
    fn batch_insert_is_single_txn() {
        let conn = memdb();
        let batch: Vec<db::Entry> = (0..500)
            .map(|i| entry(&format!("cmd-{i}"), "/r", 0, 100 + i))
            .collect();
        let n = db::insert_batch(&conn, &batch).unwrap();
        assert_eq!(n, 500);
        let rows = db::search(&conn, "cmd-", 600, None, None, None, None, None).unwrap();
        assert_eq!(rows.len(), 500);
    }

    #[test]
    fn daemon_drain_moves_queue_to_db() {
        let mut qdir = std::env::temp_dir();
        qdir.push(format!("gl-drain-{}", std::process::id()));
        let dbp = qdir.join("history.db");
        std::fs::create_dir_all(&qdir).unwrap();
        let e = entry("git push origin main", "/repo", 0, 999);
        crate::queue::append(
            &qdir,
            &crate::queue::QueuedEntry {
                cmd: e.cmd.clone(),
                cwd: e.cwd.clone(),
                exit_code: e.exit_code,
                duration_ns: 0,
                started_at: e.started_at,
                ended_at: e.ended_at,
                hostname: e.hostname.clone(),
                session_id: e.session_id.clone(),
                shell: e.shell.clone(),
                sensitive: false,
            },
        )
        .unwrap();
        let (n, skipped) = crate::cmd_daemon::drain_once(&qdir, &dbp).unwrap();
        assert_eq!((n, skipped), (1, 0));
        let conn = db::open(&dbp).unwrap();
        let rows = db::search(&conn, "push", 10, None, None, None, None, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].3, "git push origin main");
        // second drain is a no-op
        let (n2, _) = crate::cmd_daemon::drain_once(&qdir, &dbp).unwrap();
        assert_eq!(n2, 0);
        std::fs::remove_dir_all(&qdir).ok();
    }

    #[test]
    fn suggest_skips_control_entries() {
        // Ghost is painted raw onto the terminal: replaying ESC/newline/BEL
        // entries would fire inverse video, cursor jumps, scrolls (flicker).
        let conn = memdb();
        db::insert(&conn, &entry("echo \x1b[7mBOLD", "/r", 0, 100)).unwrap();
        db::insert(&conn, &entry("echo line1\nline2", "/r", 0, 101)).unwrap();
        db::insert(&conn, &entry("echo beep\x07", "/r", 0, 102)).unwrap();
        db::insert(&conn, &entry("echo clean", "/r", 0, 103)).unwrap();
        let cands = db::suggest(&conn, "echo ", "/r", 5).unwrap();
        assert_eq!(cands, vec!["echo clean"]);
    }

    #[test]
    fn search_exit_filter() {
        let conn = memdb();
        db::insert(&conn, &entry("make", "/r", 0, 100)).unwrap();
        db::insert(&conn, &entry("make", "/r", 2, 101)).unwrap();
        let ok = db::search(&conn, "make", 10, None, Some("0"), None, None, None).unwrap();
        let bad = db::search(&conn, "make", 10, None, Some("!0"), None, None, None).unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(bad.len(), 1);
        assert_eq!(ok[0].1, 0);
    }

    #[test]
    fn suggest_skips_sensitive_entries() {
        let conn = memdb();
        let mut sensitive = entry("deploy --token [REDACTED]", "/r", 0, 100);
        sensitive.sensitive = true;
        db::insert(&conn, &sensitive).unwrap();
        db::insert(&conn, &entry("deploy --prod", "/r", 0, 101)).unwrap();
        let cands = db::suggest(&conn, "deploy", "/r", 5).unwrap();
        assert_eq!(cands, vec!["deploy --prod"]);
    }

    #[test]
    fn search_short_query_uses_like_fallback() {
        let conn = memdb();
        db::insert(&conn, &entry("git status", "/r", 0, 100)).unwrap();
        let rows = db::search(&conn, "gi", 10, None, None, None, None, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].3, "git status");
    }

    #[test]
    fn search_matches_cwd_via_fts() {
        let conn = memdb();
        db::insert(&conn, &entry("ls -la", "/repo/alpha", 0, 100)).unwrap();
        let rows = db::search(&conn, "alpha", 10, None, None, None, None, None).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn search_treats_like_wildcards_literally() {
        let conn = memdb();
        db::insert(&conn, &entry("echo 100% done", "/r", 0, 100)).unwrap();
        db::insert(&conn, &entry("echo 100x done", "/r", 0, 101)).unwrap();
        let rows = db::search(&conn, "100%", 10, None, None, None, None, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].3, "echo 100% done");
    }

    #[test]
    fn suggest_plan_uses_command_index() {
        let conn = memdb();
        db::insert(&conn, &entry("git status", "/r", 0, 100)).unwrap();
        let plan = db::suggest_plan(&conn).unwrap().join("\n");
        assert!(
            plan.contains("idx_cmd"),
            "plan did not use idx_cmd:\n{plan}"
        );
    }

    #[test]
    fn search_dir_matches_subtree() {
        let conn = memdb();
        db::insert(&conn, &entry("make", "/repo", 0, 100)).unwrap();
        db::insert(&conn, &entry("make", "/repo/sub", 0, 101)).unwrap();
        db::insert(&conn, &entry("make", "/repository", 0, 102)).unwrap();
        db::insert(&conn, &entry("make", "/other", 0, 103)).unwrap();
        let rows = db::search(&conn, "make", 10, Some("/repo"), None, None, None, None).unwrap();
        assert_eq!(rows.len(), 2);
        let rows = db::search(&conn, "make", 10, Some("/repo/"), None, None, None, None).unwrap();
        assert_eq!(rows.len(), 2);
        let rows = db::search(&conn, "make", 10, Some("/"), None, None, None, None).unwrap();
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn search_negative_limit_returns_nothing() {
        let conn = memdb();
        db::insert(&conn, &entry("git status", "/r", 0, 100)).unwrap();
        let rows = db::search(&conn, "", -1, None, None, None, None, None).unwrap();
        assert!(rows.is_empty());
        let rows = db::search(&conn, "git", 0, None, None, None, None, None).unwrap();
        assert!(rows.is_empty());
        let rows = db::search(&conn, "git", i64::MAX, None, None, None, None, None).unwrap();
        assert_eq!(rows.len(), 1);
        let cands = db::suggest(&conn, "git", "/r", i64::MAX).unwrap();
        assert_eq!(cands, vec!["git status"]);
    }

    #[test]
    fn suggest_survives_control_starvation() {
        let conn = memdb();
        for i in 0..3 {
            db::insert(&conn, &entry(&format!("bad clean {i}"), "/r", 0, 100 + i)).unwrap();
        }
        for i in 0..25 {
            db::insert(&conn, &entry(&format!("bad\x1b[7m{i}"), "/r", 0, 200 + i)).unwrap();
        }
        let cands = db::suggest(&conn, "bad", "/r", 3).unwrap();
        assert_eq!(cands, vec!["bad clean 2", "bad clean 1", "bad clean 0"]);
    }

    #[test]
    fn open_migrates_schema_and_preserves_rows() {
        let dir = std::env::temp_dir().join(format!("gl-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        {
            let conn = db::open(&path).unwrap();
            db::insert(&conn, &entry("git status", "/r", 0, 100)).unwrap();
            // Simulate a database created before versioning: no idx_cmd and
            // user_version left at 0.
            conn.execute_batch("DROP INDEX idx_cmd;").unwrap();
            conn.pragma_update(None, "user_version", 0).unwrap();
        }
        let conn = db::open(&path).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, db::SCHEMA_VERSION);
        let objs = db::schema_objects(&conn).unwrap();
        assert!(objs.iter().any(|o| o == "idx_cmd"));
        let rows = db::search(&conn, "status", 10, None, None, None, None, None).unwrap();
        assert_eq!(rows.len(), 1);
        drop(conn);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_rejects_newer_schema_version() {
        let dir = std::env::temp_dir().join(format!("gl-newer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        {
            let conn = db::open(&path).unwrap();
            conn.pragma_update(None, "user_version", db::SCHEMA_VERSION + 1)
                .unwrap();
        }
        assert!(db::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_exit_accepts_arbitrary_codes() {
        let conn = memdb();
        db::insert(&conn, &entry("make", "/r", 0, 100)).unwrap();
        db::insert(&conn, &entry("make", "/r", 2, 101)).unwrap();
        db::insert(&conn, &entry("make", "/r", 127, 102)).unwrap();
        let eq2 = db::search(&conn, "make", 10, None, Some("2"), None, None, None).unwrap();
        assert_eq!(eq2.len(), 1);
        assert_eq!(eq2[0].1, 2);
        let ne0 = db::search(&conn, "make", 10, None, Some("!0"), None, None, None).unwrap();
        assert_eq!(ne0.len(), 2);
        let ne2 = db::search(&conn, "make", 10, None, Some("!2"), None, None, None).unwrap();
        assert_eq!(ne2.len(), 2);
        assert!(db::parse_exit_filter("bogus").is_none());
        assert!(db::parse_exit_filter("!").is_none());
    }

    #[test]
    fn search_ties_are_deterministic() {
        let conn = memdb();
        db::insert(&conn, &entry("first", "/r", 0, 100)).unwrap();
        db::insert(&conn, &entry("second", "/r", 0, 100)).unwrap();
        conn.execute("UPDATE history SET id='id-a' WHERE cmd='first'", [])
            .unwrap();
        conn.execute("UPDATE history SET id='id-b' WHERE cmd='second'", [])
            .unwrap();
        let rows = db::search(&conn, "", 10, None, None, None, None, None).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.3.as_str()).collect::<Vec<_>>(),
            vec!["second", "first"]
        );
    }

    #[test]
    fn suggest_ties_are_deterministic() {
        let conn = memdb();
        db::insert(&conn, &entry("tool beta", "/r", 0, 100)).unwrap();
        db::insert(&conn, &entry("tool alpha", "/r", 0, 100)).unwrap();
        let cands = db::suggest(&conn, "tool", "/r", 5).unwrap();
        assert_eq!(cands, vec!["tool alpha", "tool beta"]);
    }

    #[test]
    fn fts_update_trigger_keeps_index_in_sync() {
        let conn = memdb();
        db::insert(&conn, &entry("old command", "/r", 0, 100)).unwrap();
        conn.execute(
            "UPDATE history SET cmd='new command' WHERE cmd='old command'",
            [],
        )
        .unwrap();
        let old = db::search(&conn, "old", 10, None, None, None, None, None).unwrap();
        assert!(old.is_empty());
        let new = db::search(&conn, "new", 10, None, None, None, None, None).unwrap();
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].3, "new command");
        conn.execute("DELETE FROM history WHERE cmd='new command'", [])
            .unwrap();
        let gone = db::search(&conn, "new", 10, None, None, None, None, None).unwrap();
        assert!(gone.is_empty());
    }

    #[test]
    fn migration_v1_adds_update_trigger() {
        let dir = std::env::temp_dir().join(format!("gl-migrate2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        {
            let conn = db::open(&path).unwrap();
            // Simulate a v1 database: no update trigger and the old FTS5
            // `'delete'` insert in the delete trigger.
            conn.execute_batch(
                "DROP TRIGGER history_au;
                 DROP TRIGGER history_ad;
                 CREATE TRIGGER history_ad AFTER DELETE ON history BEGIN
                   INSERT INTO history_fts(history_fts, rowid, cmd, cwd)
                     VALUES('delete', old.rowid, old.cmd, old.cwd);
                 END;",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
        }
        let conn = db::open(&path).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, db::SCHEMA_VERSION);
        let triggers: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name='history_au'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(triggers, 1);
        db::insert(&conn, &entry("delete me", "/r", 0, 100)).unwrap();
        conn.execute("DELETE FROM history WHERE cmd='delete me'", [])
            .unwrap();
        assert!(
            db::search(&conn, "delete", 10, None, None, None, None, None)
                .unwrap()
                .is_empty()
        );
        drop(conn);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn analyze_populates_planner_stats() {
        let conn = memdb();
        db::insert(&conn, &entry("git status", "/r", 0, 100)).unwrap();
        db::analyze(&conn).unwrap();
        let objs = db::schema_objects(&conn).unwrap();
        assert!(
            objs.iter().any(|o| o == "sqlite_stat1"),
            "ANALYZE must create sqlite_stat1: {objs:?}"
        );
        db::optimize(&conn).unwrap();
    }

    #[test]
    fn empty_xdg_base_is_ignored() {
        use std::ffi::OsString;
        assert_eq!(db::path_from_env(None), None);
        assert_eq!(db::path_from_env(Some(OsString::new())), None);
        assert_eq!(
            db::path_from_env(Some(OsString::from("/tmp/base"))),
            Some(std::path::PathBuf::from("/tmp/base"))
        );
    }

    #[test]
    fn open_read_missing_db_returns_none_without_creating() {
        let dir = std::env::temp_dir().join(format!("gl-ro-missing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        assert!(db::open_read(&path).unwrap().is_none());
        assert!(!path.exists(), "read path must not create an empty database");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_read_is_read_only_with_short_timeout() {
        let dir = std::env::temp_dir().join(format!("gl-ro-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        {
            let conn = db::open(&path).unwrap();
            db::insert(&conn, &entry("git status", "/r", 0, 100)).unwrap();
        }
        let conn = db::open_read(&path).unwrap().expect("existing db opens");
        let timeout: i64 = conn
            .pragma_query_value(None, "busy_timeout", |r| r.get(0))
            .unwrap();
        assert_eq!(timeout, 100);
        let query_only: i64 = conn
            .pragma_query_value(None, "query_only", |r| r.get(0))
            .unwrap();
        assert_eq!(query_only, 1);
        assert!(
            conn.execute(
                "INSERT INTO history(id,cmd,cwd,exit_code,started_at,ended_at)
                 VALUES('x','x','x',0,0,0)",
                []
            )
            .is_err(),
            "read-only connection must reject writes"
        );
        let rows = db::search(&conn, "status", 10, None, None, None, None, None).unwrap();
        assert_eq!(rows.len(), 1);
        drop(conn);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn batch_insert_takes_write_lock_before_first_row() {
        let dir = std::env::temp_dir().join(format!("gl-immediate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        let writer = db::open(&path).unwrap();
        let other = db::open(&path).unwrap();
        other
            .busy_timeout(std::time::Duration::from_millis(20))
            .unwrap();
        let tx = db::begin_write(&writer).unwrap();
        // BEGIN IMMEDIATE holds the write lock even though no row was written;
        // a deferred transaction would let this insert through until commit.
        let err = db::insert(&other, &entry("blocked", "/r", 0, 1)).unwrap_err();
        assert!(
            matches!(err, rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::DatabaseBusy),
            "expected BUSY, got {err:?}"
        );
        tx.commit().unwrap();
        db::insert(&other, &entry("after commit", "/r", 0, 2)).unwrap();
        drop(writer);
        drop(other);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn auto_vacuum_is_incremental_on_new_databases() {
        let dir = std::env::temp_dir().join(format!("gl-av-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = db::open(&dir.join("history.db")).unwrap();
        let mode: i64 = conn
            .pragma_query_value(None, "auto_vacuum", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, 2, "new databases should use incremental auto-vacuum");
        drop(conn);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_ranks_exact_then_prefix_then_substring() {
        let conn = memdb();
        db::insert(&conn, &entry("git", "/r", 0, 100)).unwrap();
        db::insert(&conn, &entry("git status", "/r", 0, 200)).unwrap();
        db::insert(&conn, &entry("run git", "/r", 0, 300)).unwrap();
        let rows = db::search(&conn, "git", 10, None, None, None, None, None).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.3.as_str()).collect::<Vec<_>>(),
            vec!["git", "git status", "run git"]
        );
    }

    #[test]
    fn suggest_decays_stale_frequency() {
        let conn = memdb();
        let day = 86_400_000_000_000i64;
        let now = 1_000 * day;
        for i in 0..100 {
            db::insert(&conn, &entry("deploy legacy", "/other", 0, i)).unwrap();
        }
        for i in 0..10 {
            db::insert(&conn, &entry("deploy current", "/repo", 0, now - day + i)).unwrap();
        }
        let cands = db::suggest_at(&conn, "deploy", "/repo", 5, now).unwrap();
        assert_eq!(cands[0], "deploy current");
    }

    #[test]
    fn suggest_prefers_recent_use_over_stale_frequency() {
        let conn = memdb();
        let day = 86_400_000_000_000i64;
        let now = 1_000 * day;
        for i in 0..10 {
            db::insert(&conn, &entry("deploy old", "/other", 0, now - 100 * day + i)).unwrap();
        }
        db::insert(&conn, &entry("deploy recent", "/other", 0, now - day)).unwrap();
        let cands = db::suggest_at(&conn, "deploy", "/repo", 5, now).unwrap();
        assert_eq!(cands[0], "deploy recent");
    }

    #[test]
    fn forget_pattern_deletes_and_syncs_fts() {
        let conn = memdb();
        db::insert(&conn, &entry("git status", "/r", 0, 100)).unwrap();
        db::insert(&conn, &entry("cargo test", "/r", 0, 200)).unwrap();
        db::insert(&conn, &entry("git log", "/r", 0, 300)).unwrap();
        let patterns = vec!["git".to_string()];
        let report = db::forget(&conn, &db::ForgetMode::Patterns(&patterns), false).unwrap();
        assert_eq!((report.matched, report.deleted), (2, 2));
        assert!(db::search(&conn, "git", 10, None, None, None, None, None)
            .unwrap()
            .is_empty());
        assert_eq!(
            db::search(&conn, "cargo", 10, None, None, None, None, None)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn forget_last_n_and_max_rows_are_opposites() {
        let conn = memdb();
        db::insert(&conn, &entry("first", "/r", 0, 100)).unwrap();
        db::insert(&conn, &entry("second", "/r", 0, 200)).unwrap();
        db::insert(&conn, &entry("third", "/r", 0, 300)).unwrap();
        let report = db::forget(&conn, &db::ForgetMode::LastN(2), false).unwrap();
        assert_eq!(report.deleted, 2);
        let rows = db::search(&conn, "", 10, None, None, None, None, None).unwrap();
        assert_eq!(rows.iter().map(|r| r.3.as_str()).collect::<Vec<_>>(), vec!["first"]);

        let report = db::forget(&conn, &db::ForgetMode::MaxRows(0), false).unwrap();
        assert_eq!(report.deleted, 1);
        assert!(db::search(&conn, "", 10, None, None, None, None, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn forget_pattern_treats_wildcards_literally() {
        let conn = memdb();
        db::insert(&conn, &entry("echo 100% done", "/r", 0, 100)).unwrap();
        db::insert(&conn, &entry("echo 100x done", "/r", 0, 200)).unwrap();
        let patterns = vec!["100%".to_string()];
        let report = db::forget(&conn, &db::ForgetMode::Patterns(&patterns), false).unwrap();
        assert_eq!(report.deleted, 1);
        let rows = db::search(&conn, "100", 10, None, None, None, None, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].3, "echo 100x done");
    }

    #[test]
    fn forget_older_than_days_uses_a_cutoff() {
        let conn = memdb();
        db::insert(&conn, &entry("very old", "/r", 0, 0)).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        db::insert(&conn, &entry("fresh", "/r", 0, now)).unwrap();
        let report = db::forget(&conn, &db::ForgetMode::OlderThanDays(1), false).unwrap();
        assert_eq!(report.deleted, 1);
        let rows = db::search(&conn, "", 10, None, None, None, None, None).unwrap();
        assert_eq!(rows.iter().map(|r| r.3.as_str()).collect::<Vec<_>>(), vec!["fresh"]);
    }

    #[test]
    fn forget_dry_run_reports_without_deleting() {
        let conn = memdb();
        db::insert(&conn, &entry("git status", "/r", 0, 100)).unwrap();
        db::insert(&conn, &entry("git log", "/r", 0, 200)).unwrap();
        let patterns = vec!["git".to_string()];
        let report = db::forget(&conn, &db::ForgetMode::Patterns(&patterns), true).unwrap();
        assert_eq!((report.matched, report.deleted), (2, 0));
        assert_eq!(report.rows.len(), 2);
        assert_eq!(
            db::search(&conn, "git", 10, None, None, None, None, None)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn forget_rejects_multiple_selectors() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(crate::cmd_forget::run(&args(&["--last-n", "1", "--all"])).is_err());
        assert!(crate::cmd_forget::run(&args(&["--older-than", "-1"])).is_err());
        assert!(crate::cmd_forget::run(&args(&["--max-rows", "x"])).is_err());
    }

    #[test]
    fn import_dedupes_reimported_histfile() {
        let dir = std::env::temp_dir().join(format!("gl-import-dedupe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hist = dir.join("history.txt");
        std::fs::write(&hist, "git status\ncargo test\n").unwrap();
        let dbp = dir.join("history.db");
        let args = |extra: &[&str]| {
            let mut v = vec![
                "--from".to_string(),
                hist.to_str().unwrap().to_string(),
                "--db".to_string(),
                dbp.to_str().unwrap().to_string(),
            ];
            v.extend(extra.iter().map(|s| s.to_string()));
            v
        };
        crate::cmd_import::run(&args(&[])).unwrap();
        crate::cmd_import::run(&args(&[])).unwrap();
        {
            let conn = db::open(&dbp).unwrap();
            let count: i64 = conn
                .query_row("SELECT count(*) FROM history", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 2, "second import must skip duplicates");
        }
        crate::cmd_import::run(&args(&["--no-dedupe"])).unwrap();
        let conn = db::open(&dbp).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 4, "--no-dedupe must force a fresh copy");
        drop(conn);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migration_v2_adds_import_key_index() {
        let dir = std::env::temp_dir().join(format!("gl-migrate3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        {
            let conn = db::open(&path).unwrap();
            conn.execute_batch(
                "DROP INDEX idx_import_key;
                 ALTER TABLE history DROP COLUMN import_key;",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
        }
        let conn = db::open(&path).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, db::SCHEMA_VERSION);
        let index: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='idx_import_key'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(index, 1);
        let column: i64 = conn
            .query_row(
                "SELECT count(*) FROM pragma_table_info('history') WHERE name='import_key'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(column, 1);
        drop(conn);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_missing_db_does_not_create_one() {
        let dir = std::env::temp_dir().join(format!("gl-search-nocreate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        let args = vec![
            "--db".to_string(),
            path.to_str().unwrap().to_string(),
            "anything".to_string(),
        ];
        crate::cmd_search::run(&args).unwrap();
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn open_makes_db_and_sidecars_private() {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::temp_dir().join(format!("gl-perm-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let dbp = base.join("nested").join("ghostline").join("history.db");
        let conn = db::open(&dbp).unwrap();
        db::insert(&conn, &entry("git status", "/r", 0, 100)).unwrap();

        let parent = dbp.parent().unwrap();
        assert_eq!(
            std::fs::metadata(parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let mut files = vec![dbp.clone()];
        for suffix in ["-wal", "-shm"] {
            let mut p = dbp.as_os_str().to_owned();
            p.push(suffix);
            files.push(std::path::PathBuf::from(p));
        }
        for p in files {
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{} has mode {mode:o}", p.display());
        }
        drop(conn);
        std::fs::remove_dir_all(&base).ok();
    }
}
