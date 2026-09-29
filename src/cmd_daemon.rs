use crate::{db, queue};
use std::time::Duration;

/// Drain the queue into SQLite once. Returns (inserted, skipped_lines).
pub fn drain_once(
    queue_dir: &std::path::Path,
    db_path: &std::path::Path,
) -> Result<(usize, u64), String> {
    let claimed = match queue::claim(queue_dir) {
        Some(p) => p,
        None => return Ok((0, 0)),
    };
    let (entries, skipped) = queue::parse_file(&claimed);
    let mut inserted = 0usize;
    if !entries.is_empty() {
        let conn = db::open(db_path).map_err(|e| format!("open db: {e}"))?;
        let batch: Vec<db::Entry> = entries
            .into_iter()
            .map(|q| db::Entry {
                cmd: q.cmd,
                cwd: q.cwd,
                exit_code: q.exit_code,
                duration_ns: q.duration_ns,
                started_at: q.started_at,
                ended_at: q.ended_at,
                hostname: q.hostname,
                session_id: q.session_id,
                shell: q.shell,
                sensitive: q.sensitive,
            })
            .collect();
        // Chunk so one giant queue file can't blow memory or hold the write lock forever.
        for chunk in batch.chunks(500) {
            inserted += db::insert_batch(&conn, chunk).map_err(|e| format!("insert: {e}"))?;
        }
        conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);").ok();
    }
    std::fs::remove_file(&claimed).map_err(|e| format!("remove claimed queue: {e}"))?;
    Ok((inserted, skipped))
}

/// `ghostline daemon [--once] [--interval-ms N] [--queue-dir D] [--db P]`
pub fn run(args: &[String]) -> Result<(), String> {
    let mut once = false;
    let mut interval_ms: u64 = 1000;
    let mut queue_dir: Option<String> = None;
    let mut db_path: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--once" => once = true,
            "--interval-ms" => {
                i += 1;
                interval_ms = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1000)
                    .max(100);
            }
            "--queue-dir" => {
                i += 1;
                queue_dir = args.get(i).cloned();
            }
            "--db" => {
                i += 1;
                db_path = args.get(i).cloned();
            }
            s => return Err(format!("unknown flag '{s}'")),
        }
        i += 1;
    }
    let qdir = queue_dir
        .map(std::path::PathBuf::from)
        .unwrap_or_else(queue::default_queue_dir);
    let dbp = db_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(db::default_db_path);
    std::fs::create_dir_all(&qdir).map_err(|e| format!("queue dir: {e}"))?;
    if once {
        let (n, skipped) = drain_once(&qdir, &dbp)?;
        if n > 0 || skipped > 0 {
            println!("drained {n} inserted, {skipped} skipped");
        }
        return Ok(());
    }
    // Run loop: batch drain every interval. Killable instantly (Ctrl-C / SIGTERM default).
    // Never touches the prompt path — sync/network stays out of band by design.
    loop {
        match drain_once(&qdir, &dbp) {
            Ok((0, 0)) => {}
            Ok((n, skipped)) => println!("drained {n} inserted, {skipped} skipped"),
            Err(e) => eprintln!("ghostline daemon: {e}"),
        }
        std::thread::sleep(Duration::from_millis(interval_ms));
    }
}
