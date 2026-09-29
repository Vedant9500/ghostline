use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

/// A log entry serialized to the ingest queue (JSONL, one object per line).
/// Mirrors db::Entry so the daemon can batch-insert without re-parsing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedEntry {
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

/// Queue dir: $XDG_CACHE_HOME/ghostline/queue or ~/.cache/ghostline/queue
pub fn default_queue_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        let mut p = PathBuf::from(xdg);
        p.push("ghostline/queue");
        return p;
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut p = home;
    p.push(".cache/ghostline/queue");
    p
}

fn queue_file(dir: &std::path::Path) -> PathBuf {
    dir.join("history.jsonl")
}

/// Append one entry. O_APPEND single write; small lines stay atomic on POSIX.
pub fn append(dir: &std::path::Path, entry: &QueuedEntry) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(queue_file(dir))?;
    let mut line = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// Atomically claim the queue file for processing (rename), return its old path or None.
pub fn claim(dir: &std::path::Path) -> Option<PathBuf> {
    let src = queue_file(dir);
    let meta = std::fs::metadata(&src).ok()?;
    if meta.len() == 0 {
        return None;
    }
    let dest = dir.join(format!(".processing-{}-{}", std::process::id(), meta.len()));
    std::fs::rename(&src, &dest).ok()?;
    Some(dest)
}

/// Parse claimed file into entries. Malformed lines are counted as skipped, never fatal
/// (crash-safe: a torn last line from a killed writer is dropped, everything else survives).
pub fn parse_file(path: &std::path::Path) -> (Vec<QueuedEntry>, u64) {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut entries = vec![];
    let mut skipped = 0u64;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<QueuedEntry>(line) {
            Ok(e) => entries.push(e),
            Err(_) => skipped += 1,
        }
    }
    (entries, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("gl-qtest-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn sample(cmd: &str) -> QueuedEntry {
        QueuedEntry {
            cmd: cmd.into(),
            cwd: "/repo".into(),
            exit_code: 0,
            duration_ns: 0,
            started_at: 1,
            ended_at: 1,
            hostname: "h".into(),
            session_id: "s".into(),
            shell: "bash".into(),
            sensitive: false,
        }
    }

    #[test]
    fn append_claim_parse_roundtrip() {
        let dir = tmpdir("roundtrip");
        append(&dir, &sample("git status")).unwrap();
        append(&dir, &sample("cargo test")).unwrap();
        let claimed = claim(&dir).expect("queue claimed");
        assert!(
            queue_file(&dir).exists() == false
                || std::fs::metadata(queue_file(&dir))
                    .map(|m| m.len())
                    .unwrap_or(0)
                    == 0
        );
        let (entries, skipped) = parse_file(&claimed);
        assert_eq!(skipped, 0);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].cmd, "cargo test");
        std::fs::remove_file(&claimed).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn torn_last_line_does_not_kill_batch() {
        let dir = tmpdir("torn");
        append(&dir, &sample("git status")).unwrap();
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(queue_file(&dir))
                .unwrap();
            f.write_all(b"{\"cmd\": \"torn...\n").unwrap();
        }
        let claimed = claim(&dir).expect("queue claimed");
        let (entries, skipped) = parse_file(&claimed);
        assert_eq!(entries.len(), 1);
        assert_eq!(skipped, 1);
        std::fs::remove_file(&claimed).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_queue_claims_nothing() {
        let dir = tmpdir("empty");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(claim(&dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
