mod cmd_daemon;
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
  ghostline fix [--last] [command...]  did-you-mean repair (exit 0 = found)
  ghostline search [flags] [query]    search history (tab-separated: ts, exit, cwd, cmd)
  ghostline suggest [flags] <buffer>  print ghost suffix for buffer (or --all for full cmds)

log flags:     --shell S --cwd DIR --exit CODE --session ID [--db PATH] [--queue] [--queue-dir D]
daemon flags:  [--once] [--interval-ms N] [--queue-dir D] [--db PATH]
fix flags:     [--last] [--exit CODE] [--cwd DIR] [--db P] [-q]
search flags:  [--limit N] [--dir DIR] [--exit 0|!0] [--session S] [--host H] [--since UNIX] [--db PATH]
suggest flags: [--cwd DIR] [--limit N] [--all] [--db PATH]

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
}
