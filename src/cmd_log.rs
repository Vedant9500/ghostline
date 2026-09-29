use crate::{db, redact};
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

pub struct LogArgs {
    pub shell: String,
    pub cwd: String,
    pub exit_code: i64,
    pub session: String,
    pub cmd: String,
}

/// Log one command. Returns Ok(true) if stored, Ok(false) if skipped (ignored/empty/self).
pub fn run(args: &[String]) -> Result<bool, String> {
    let mut shell = std::env::var("GHOSTLINE_SHELL").unwrap_or_default();
    let mut cwd = String::new();
    let mut exit_code: i64 = 0;
    let mut session = std::env::var("GHOSTLINE_SESSION").unwrap_or_default();
    let mut db_path: Option<String> = None;
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
            "--" => {
                cmd_parts.extend_from_slice(&args[i + 1..]);
                break;
            }
            s if s.starts_with("--") => return Err(format!("unknown flag '{s}'")),
            _ => cmd_parts.push(args[i].clone()),
        }
        i += 1;
    }
    let mut cmd = cmd_parts.join(" ");
    if cmd.is_empty() {
        // also accept piped stdin? No — empty means nothing to log.
        return Ok(false);
    }
    // normalize whitespace, cap at 8KB
    cmd = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 8192;
    if cmd.len() > MAX {
        cmd.truncate(MAX);
    }
    if cmd.starts_with("ghostline ") || cmd == "ghostline" {
        return Ok(false); // never log self
    }
    if cwd.is_empty() {
        cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    if shell.is_empty() {
        shell = "unknown".to_string();
    }
    let ignore_env = std::env::var("GHOSTLINE_IGNORE").ok();
    if redact::is_ignored(&cmd, ignore_env.as_deref()) {
        return Ok(false);
    }
    let (redacted, sensitive) = redact::redact(&cmd);
    let now = now_ns();
    let entry = db::Entry {
        cmd: redacted,
        cwd,
        exit_code,
        duration_ns: 0,
        started_at: now,
        ended_at: now,
        hostname: hostname(),
        session_id: session,
        shell,
        sensitive,
    };
    let path = db_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(db::default_db_path);
    let conn = db::open(&path).map_err(|e| format!("open db: {e}"))?;
    db::insert(&conn, &entry).map_err(|e| format!("insert: {e}"))?;
    Ok(true)
}
