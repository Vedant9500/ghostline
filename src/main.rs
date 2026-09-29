mod cmd_init;
mod cmd_log;
mod cmd_search;
mod cmd_suggest;
mod db;
mod redact;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn usage() -> &'static str {
    "ghostline — near-instant shell history + ghost suggestions

Usage:
  ghostline init <bash|zsh|fish>      print shell hook (eval \"$(ghostline init bash)\")
  ghostline log [flags] <command...>  log one command
  ghostline search [flags] [query]    search history (tab-separated: ts, exit, cwd, cmd)
  ghostline suggest [flags] <buffer>  print ghost suffix for buffer (or --all for full cmds)

log flags:     --shell S --cwd DIR --exit CODE --session ID [--db PATH]
search flags:  [--limit N] [--dir DIR] [--exit 0|!0] [--session S] [--host H] [--since UNIX] [--db PATH]
suggest flags: [--cwd DIR] [--limit N] [--all] [--db PATH]

Env: GHOSTLINE_IGNORE (glob list), GHOSTLINE_DISABLED=1 to pause logging."
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
