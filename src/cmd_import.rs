use crate::db;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string()))
}

/// Parse a bash HISTFILE: lines, with optional `#<unix-secs>` timestamp lines
/// (HISTTIMEFORMAT) applying to the following entry. Returns
/// `(cmd, started_ns, 1-based-line-number, had_timestamp)`; the line number
/// keeps the import dedupe key stable when the same file is imported again.
pub fn parse_histfile(text: &str, fallback_ns: i64) -> Vec<(String, i64, usize, bool)> {
    let mut out = vec![];
    let mut pending_ts: Option<i64> = None;
    let mut fallback_offset: i64 = 0;
    for (line_no, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(ts) = line.strip_prefix('#')
            && ts.len() == 10
            && ts.bytes().all(|b| b.is_ascii_digit())
        {
            pending_ts = ts.parse::<i64>().ok().map(|s| s * 1_000_000_000);
            continue;
        }
        let cmd = crate::redact::clean_command(line);
        if cmd.is_empty() || cmd.starts_with("ghostline ") || cmd == "ghostline" {
            pending_ts = None;
            continue;
        }
        const MAX: usize = 8192;
        let mut cmd = cmd;
        crate::redact::truncate_utf8(&mut cmd, MAX);
        // Untimestamped lines keep file order instead of all sharing one
        // started_at (which made newest-first ordering arbitrary). The
        // `timed` flag keeps their dedupe key independent of this clock.
        let (ts, timed) = match pending_ts {
            Some(ts) => (ts, true),
            None => {
                let ts = fallback_ns.saturating_add(fallback_offset);
                fallback_offset = fallback_offset.saturating_add(1);
                (ts, false)
            }
        };
        out.push((cmd, ts, line_no + 1, timed));
        pending_ts = None;
    }
    out
}

/// FNV-1a 64: stable across runs (unlike Rust's `DefaultHasher`, whose keys
/// are not guaranteed). The key covers the source path, line number, timestamp
/// and stored command, so re-importing the same file is idempotent while a
/// different file (or an appended line) still imports.
fn import_key(source: &str, line_no: usize, started_at: i64, cmd: &str, timed: bool) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    feed(source.as_bytes());
    feed(&[0]);
    feed(if timed { b"t" } else { b"u" });
    feed(&line_no.to_string().into_bytes());
    feed(&[0]);
    if timed {
        feed(&started_at.to_string().into_bytes());
        feed(&[0]);
    }
    feed(cmd.as_bytes());
    format!("{hash:016x}")
}

/// `ghostline import [--from PATH] [--db P] [--shell S] [--no-dedupe]`
/// Imported rows keep file order; unknown cwd recorded as "" (global rank only).
/// Re-importing the same file skips rows whose stable import key already exists.
pub fn run(args: &[String]) -> Result<(), String> {
    let mut from: Option<String> = None;
    let mut db_path: Option<String> = None;
    let mut shell = "bash".to_string();
    let mut dedupe = true;
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
            "--no-dedupe" => dedupe = false,
            s => {
                return Err(format!(
                    "unknown flag '{s}' (usage: ghostline import [--from HISTFILE] [--db P] [--no-dedupe])"
                ));
            }
        }
        i += 1;
    }
    let histfile = from.map(std::path::PathBuf::from).unwrap_or_else(|| {
        let home = db::path_from_env(std::env::var_os("HOME")).unwrap_or_else(|| ".".into());
        home.join(".bash_history")
    });
    let text = std::fs::read_to_string(&histfile)
        .map_err(|e| format!("read {}: {e}", histfile.display()))?;
    let source = std::fs::canonicalize(&histfile)
        .unwrap_or_else(|_| histfile.clone())
        .to_string_lossy()
        .into_owned();
    let now = now_ns();
    let parsed = parse_histfile(&text, now);
    let total = parsed.len();
    let entries: Vec<db::Entry> = parsed
        .into_iter()
        .map(|(cmd, ts, line_no, timed)| {
            let (cmd, sensitive) = crate::redact::redact(&cmd);
            let key = dedupe.then(|| import_key(&source, line_no, ts, &cmd, timed));
            db::Entry {
                cmd,
                cwd: String::new(),
                exit_code: 0,
                duration_ns: 0,
                started_at: ts,
                ended_at: ts,
                hostname: hostname(),
                session_id: "import".into(),
                shell: shell.clone(),
                sensitive,
                import_key: key,
            }
        })
        .collect();
    if entries.is_empty() {
        println!("nothing to import from {}", histfile.display());
        return Ok(());
    }
    let path = db_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(db::default_db_path);
    let conn = db::open(&path).map_err(|e| format!("open db: {e}"))?;
    let mut n = 0;
    for chunk in entries.chunks(500) {
        n += db::insert_batch(&conn, chunk).map_err(|e| format!("insert: {e}"))?;
    }
    db::analyze(&conn).map_err(|e| format!("analyze: {e}"))?;
    let skipped = total - n;
    if skipped > 0 {
        println!(
            "imported {n} commands from {} ({skipped} duplicates skipped)",
            histfile.display()
        );
    } else {
        println!("imported {n} commands from {}", histfile.display());
    }
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
        assert_eq!(rows[0].0, "git status");
        assert_eq!(rows[0].1, 1727000000 * 1_000_000_000);
        assert_eq!(rows[1].0, "cargo test --all");
        assert_eq!(rows[1].1, 1727000060 * 1_000_000_000);
        assert_eq!(rows[2].0, "not-a-timestamp #123");
        assert_eq!(rows[2].1, 9);
    }

    #[test]
    fn untimed_entries_get_increasing_timestamps() {
        let rows = parse_histfile("one\ntwo\nthree\n", 100);
        assert_eq!(
            rows.iter().map(|(_, ts, _, _)| *ts).collect::<Vec<_>>(),
            vec![100, 101, 102]
        );
    }

    #[test]
    fn import_keys_are_stable_and_line_specific() {
        let text = "one\ntwo\n";
        let a = parse_histfile(text, 100);
        let b = parse_histfile(text, 999); // different fallback clock
        let ka: Vec<String> = a
            .iter()
            .map(|(cmd, ts, line, timed)| import_key("/hist", *line, *ts, cmd, *timed))
            .collect();
        let kb: Vec<String> = b
            .iter()
            .map(|(cmd, ts, line, timed)| import_key("/hist", *line, *ts, cmd, *timed))
            .collect();
        assert_eq!(ka, kb);
        assert_ne!(ka[0], ka[1]);
        assert_ne!(
            import_key("/hist", 2, 1, "one", false),
            import_key("/hist", 3, 1, "one", false)
        );
    }

    #[test]
    fn strips_terminal_controls() {
        let text = "echo \x1b[7mBOLD\x1b[0m\n\x1b\n";
        let rows = parse_histfile(text, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "echo [7mBOLD[0m");
    }

    #[test]
    fn truncates_huge_lines() {
        let text = "x".repeat(9000);
        let rows = parse_histfile(&text, 1);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].0.len() <= 8192);
    }

    #[test]
    fn truncates_multibyte_lines_at_a_char_boundary() {
        let text = format!("{}é{}", "a".repeat(8191), "z".repeat(10));
        let rows = parse_histfile(&text, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "a".repeat(8191));
    }
}
