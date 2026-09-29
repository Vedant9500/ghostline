use crate::{db, queue, redact};
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

/// Build a log entry. Returns Ok(None) if skipped (ignored/empty/self).
pub fn build_entry(
    shell: &str,
    cwd: &str,
    exit_code: i64,
    session: &str,
    cmd: &str,
) -> Option<db::Entry> {
    let mut cmd = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    if cmd.is_empty() {
        return None;
    }
    const MAX: usize = 8192;
    if cmd.len() > MAX {
        cmd.truncate(MAX);
    }
    if cmd.starts_with("ghostline ") || cmd == "ghostline" {
        return None; // never log self
    }
    let cwd = if cwd.is_empty() {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        cwd.to_string()
    };
    let ignore_env = std::env::var("GHOSTLINE_IGNORE").ok();
    if redact::is_ignored(&cmd, ignore_env.as_deref()) {
        return None;
    }
    let (redacted, sensitive) = redact::redact(&cmd);
    let now = now_ns();
    Some(db::Entry {
        cmd: redacted,
        cwd,
        exit_code,
        duration_ns: 0,
        started_at: now,
        ended_at: now,
        hostname: hostname(),
        session_id: session.to_string(),
        shell: if shell.is_empty() {
            "unknown".to_string()
        } else {
            shell.to_string()
        },
        sensitive,
    })
}

/// Log one command. Returns Ok(true) if stored, Ok(false) if skipped (ignored/empty/self).
pub fn run(args: &[String]) -> Result<bool, String> {
    let mut shell = std::env::var("GHOSTLINE_SHELL").unwrap_or_default();
    let mut cwd = String::new();
    let mut exit_code: i64 = 0;
    let mut session = std::env::var("GHOSTLINE_SESSION").unwrap_or_default();
    let mut db_path: Option<String> = None;
    let mut queue_dir: Option<String> = None;
    let mut use_queue = std::env::var("GHOSTLINE_QUEUE")
        .map(|v| v == "1")
        .unwrap_or(false);
    let mut cmd_parts: Vec<String> = vec![];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--shell" => {
                i += 1;
                shell = args.get(i).cloned().unwrap_or_default();
            }
            "--cwd" => {
                i += 1;
                cwd = args.get(i).cloned().unwrap_or_default();
            }
            "--exit" | "--exit-code" => {
                i += 1;
                exit_code = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            "--session" => {
                i += 1;
                session = args.get(i).cloned().unwrap_or_default();
            }
            "--db" => {
                i += 1;
                db_path = args.get(i).cloned();
            }
            "--queue" => use_queue = true,
            "--queue-dir" => {
                i += 1;
                queue_dir = args.get(i).cloned();
                use_queue = true;
            }
            "--" => {
                cmd_parts.extend_from_slice(&args[i + 1..]);
                break;
            }
            s if s.starts_with("--") => return Err(format!("unknown flag '{s}'")),
            _ => cmd_parts.push(args[i].clone()),
        }
        i += 1;
    }
    let cmd = cmd_parts.join(" ");
    let Some(entry) = build_entry(&shell, &cwd, exit_code, &session, &cmd) else {
        return Ok(false);
    };
    if use_queue {
        // Fast path: O_APPEND JSONL, no SQLite open in the prompt path (~0.1-0.3ms).
        let qdir = queue_dir
            .map(std::path::PathBuf::from)
            .unwrap_or_else(queue::default_queue_dir);
        let q = queue::QueuedEntry {
            cmd: entry.cmd,
            cwd: entry.cwd,
            exit_code: entry.exit_code,
            duration_ns: entry.duration_ns,
            started_at: entry.started_at,
            ended_at: entry.ended_at,
            hostname: entry.hostname,
            session_id: entry.session_id,
            shell: entry.shell,
            sensitive: entry.sensitive,
        };
        queue::append(&qdir, &q).map_err(|e| format!("queue append: {e}"))?;
        return Ok(true);
    }
    let path = db_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(db::default_db_path);
    let conn = db::open(&path).map_err(|e| format!("open db: {e}"))?;
    db::insert(&conn, &entry).map_err(|e| format!("insert: {e}"))?;
    Ok(true)
}
