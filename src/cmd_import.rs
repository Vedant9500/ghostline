use crate::db;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ns() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string()))
}

/// Parse a bash HISTFILE: lines, with optional `#<unix-secs>` timestamp lines
/// (HISTTIMEFORMAT) applying to the following entry. Returns (cmd, started_ns).
pub fn parse_histfile(text: &str, fallback_ns: i64) -> Vec<(String, i64)> {
    let mut out = vec![];
    let mut pending_ts: Option<i64> = None;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(ts) = line.strip_prefix('#') {
            if ts.len() == 10 && ts.bytes().all(|b| b.is_ascii_digit()) {
                pending_ts = ts.parse::<i64>().ok().map(|s| s * 1_000_000_000);
                continue;
            }
        }
        let cmd = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if cmd.is_empty() || cmd.starts_with("ghostline ") || cmd == "ghostline" {
            pending_ts = None;
            continue;
        }
        const MAX: usize = 8192;
        let mut cmd = cmd;
        if cmd.len() > MAX {
            cmd.truncate(MAX);
        }
        out.push((cmd, pending_ts.unwrap_or(fallback_ns)));
        pending_ts = None;
    }
    out
}

/// `ghostline import [--from PATH] [--db P] [--shell S] [--cwd DIR]`
/// Imported rows keep file order; unknown cwd recorded as "" (global rank only).
pub fn run(args: &[String]) -> Result<(), String> {
    let mut from: Option<String> = None;
    let mut db_path: Option<String> = None;
    let mut shell = "bash".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                i += 1;
                from = args.get(i).cloned();
            }
            "--db" => {
                i += 1;
                db_path = args.get(i).cloned();
            }
            "--shell" => {
                i += 1;
                shell = args.get(i).cloned().unwrap_or("bash".into());
            }
            s => return Err(format!("unknown flag '{s}' (usage: ghostline import [--from HISTFILE] [--db P])")),
        }
        i += 1;
    }
    let histfile = from.map(std::path::PathBuf::from).unwrap_or_else(|| {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(|| ".".into());
        home.join(".bash_history")
    });
    let text = std::fs::read_to_string(&histfile).map_err(|e| format!("read {}: {e}", histfile.display()))?;
    let now = now_ns();
    let entries: Vec<db::Entry> = parse_histfile(&text, now)
        .into_iter()
        .map(|(cmd, ts)| {
            let (cmd, sensitive) = crate::redact::redact(&cmd);
            db::Entry {
                cmd, cwd: String::new(), exit_code: 0, duration_ns: 0,
                started_at: ts, ended_at: ts, hostname: hostname(),
                session_id: "import".into(), shell: shell.clone(), sensitive,
            }
        })
        .collect();
    if entries.is_empty() {
        println!("nothing to import from {}", histfile.display());
        return Ok(());
    }
    let path = db_path.map(std::path::PathBuf::from).unwrap_or_else(db::default_db_path);
    let conn = db::open(&path).map_err(|e| format!("open db: {e}"))?;
    let mut n = 0;
    for chunk in entries.chunks(500) {
        n += db::insert_batch(&conn, chunk).map_err(|e| format!("insert: {e}"))?;
    }
    println!("imported {n} commands from {}", histfile.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timestamps_and_skips_self() {
        let text = "#1727000000\ngit status\n\nghostline search x\n  \n#1727000060\ncargo test --all\nnot-a-timestamp #123\n";
        let rows = parse_histfile(text, 9);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], ("git status".into(), 1727000000 * 1_000_000_000));
        assert_eq!(rows[1].0, "cargo test --all");
        assert_eq!(rows[1].1, 1727000060 * 1_000_000_000);
        assert_eq!(rows[2], ("not-a-timestamp #123".into(), 9));
    }

    #[test]
    fn truncates_huge_lines() {
        let text = "x".repeat(9000);
        let rows = parse_histfile(&text, 1);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].0.len() <= 8192);
    }
}
