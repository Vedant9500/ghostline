use crate::db;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static BENCH_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct BenchOpts {
    pub n: usize,
    pub reps: usize,
    pub concurrency: usize,
    pub db_path: Option<PathBuf>,
    pub json: bool,
    pub gate: bool,
    pub max_log_ms: f64,
    pub max_search_ms: f64,
    pub max_suggest_ms: f64,
    pub max_hook_ms: f64,
    pub min_inserts_per_s: f64,
}

impl Default for BenchOpts {
    fn default() -> Self {
        Self {
            n: 10_000,
            reps: 20,
            concurrency: 1,
            db_path: None,
            json: false,
            gate: false,
            max_log_ms: 5.0,
            max_search_ms: 50.0,
            max_suggest_ms: 50.0,
            max_hook_ms: 100.0,
            min_inserts_per_s: 2000.0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchReport {
    pub n: usize,
    pub reps: usize,
    pub concurrency: usize,
    pub log_ms_p50: f64,
    pub log_ms_p99: f64,
    pub inserts_per_s: f64,
    pub search_ms_p50: f64,
    pub search_ms_p99: f64,
    pub suggest_ms_p50: f64,
    pub suggest_ms_p99: f64,
    pub hook_ms_p50: f64,
    pub hook_ms_p99: f64,
    pub hook_subprocess: bool,
    pub busy_count: u64,
    pub wal_bytes: u64,
    pub db_bytes: u64,
    pub binary_bytes: u64,
    pub rss_kb: u64,
    pub indexes_ok: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GateCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

fn pct(mut v: Vec<f64>, p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((p / 100.0) * (v.len() as f64 - 1.0)).round() as usize;
    v[idx.min(v.len() - 1)]
}

fn bench_entry(i: usize) -> db::Entry {
    db::Entry {
        cmd: format!("cmd-{} run {}", i % 50, i),
        cwd: format!("/r/{}", i % 7),
        exit_code: if i % 10 == 9 { 1 } else { 0 },
        duration_ns: 0,
        started_at: 1_700_000_000 + i as i64,
        ended_at: 1_700_000_000 + i as i64,
        hostname: "bench".into(),
        session_id: "bench".into(),
        shell: "bash".into(),
        sensitive: false,
    }
}

fn file_bytes(p: &std::path::Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

fn rss_kb() -> u64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
        }
    }
    0
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(e, _)
        if e.code == rusqlite::ErrorCode::DatabaseBusy)
}

/// Batch throughput helper (also used by the non-vacuous NORMAL-vs-FULL test).
pub fn batch_throughput(conn: &rusqlite::Connection, n: usize) -> rusqlite::Result<f64> {
    let batch: Vec<db::Entry> = (0..n).map(bench_entry).collect();
    let t = Instant::now();
    for chunk in batch.chunks(500) {
        db::insert_batch(conn, chunk)?;
    }
    let el = t.elapsed().as_secs_f64();
    Ok(n as f64 / el.max(1e-9))
}

pub fn run(opts: &BenchOpts) -> Result<BenchReport, String> {
    if opts.n == 0 {
        return Err("bench --n must be > 0".into());
    }
    // Own temp dir unless --db given (never touch real history by default).
    let owned_dir: Option<PathBuf> = if opts.db_path.is_none() {
        let seq = BENCH_SEQ.fetch_add(1, Ordering::SeqCst);
        let mut d = std::env::temp_dir();
        d.push(format!("gl-bench-{}-{seq}", std::process::id()));
        Some(d)
    } else {
        None
    };
    let dbp = match &opts.db_path {
        Some(p) => p.clone(),
        None => owned_dir.as_ref().unwrap().join("bench.db"),
    };
    let conn = db::open(&dbp).map_err(|e| format!("open db: {e}"))?;

    // Seed + throughput (batch path = daemon/log bulk shape).
    let inserts_per_s = batch_throughput(&conn, opts.n).map_err(|e| format!("seed: {e}"))?;

    // Single-insert latency (log hot path: open once, insert reps).
    let mut log_ms = Vec::with_capacity(opts.reps.max(1));
    for i in 0..opts.reps.max(1) {
        let e = bench_entry(opts.n + i);
        let t = Instant::now();
        db::insert(&conn, &e).map_err(|e| format!("insert: {e}"))?;
        log_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    // Query latencies over seeded rows.
    let reps = opts.reps.max(1);
    let mut search_ms = Vec::with_capacity(reps);
    for _ in 0..reps {
        let t = Instant::now();
        let _ = db::search(&conn, "cmd-1", 20, None, None, None, None, None)
            .map_err(|e| format!("search: {e}"))?;
        search_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let mut suggest_ms = Vec::with_capacity(reps);
    for _ in 0..reps {
        let t = Instant::now();
        let _ = db::suggest(&conn, "cmd-1", "/r/1", 5)
            .map_err(|e| format!("suggest: {e}"))?;
        suggest_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    // Ghost fetch cost as the shell sees it: fork+exec+query round-trip of
    // `ghostline suggest` (what the bash C hook blocks on per diverge).
    // Under `cargo test` the exe is the test harness (`ghostline-<hash>`),
    // so require an exact `ghostline` stem and fall back otherwise.
    let exe = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
    let exe_is_ghostline = exe
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s == "ghostline")
        .unwrap_or(false);
    let (hook_ms_p50, hook_ms_p99, hook_subprocess) = if exe_is_ghostline {
        let mut hook_ms = Vec::with_capacity(reps.min(10).max(1));
        for _ in 0..reps.min(10).max(1) {
            let t = Instant::now();
            let out = std::process::Command::new(&exe)
                .args(["suggest", "--db"])
                .arg(&dbp)
                .args(["--cwd", "/r/1", "--", "cmd-1"])
                .output()
                .map_err(|e| format!("hook subprocess: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "hook subprocess failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
            hook_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        (pct(hook_ms.clone(), 50.0), pct(hook_ms, 99.0), true)
    } else {
        (pct(suggest_ms.clone(), 50.0), pct(suggest_ms.clone(), 99.0), false)
    };

    // Concurrency contention: threads × rows, busy_timeout=0 surfaces BUSY.
    let mut busy_count: u64 = 0;
    if opts.concurrency > 1 {
        let per = opts.n.div_ceil(opts.concurrency);
        let dbp2 = dbp.clone();
        std::thread::scope(|s| {
            let mut handles = vec![];
            for t in 0..opts.concurrency {
                let p = dbp2.clone();
                handles.push(s.spawn(move || -> Result<u64, String> {
                    let c = db::open(&p).map_err(|e| format!("open db: {e}"))?;
                    c.execute_batch("PRAGMA busy_timeout=0;")
                        .map_err(|e| format!("pragma: {e}"))?;
                    let mut busy = 0u64;
                    for i in 0..per {
                        let e = bench_entry(t * per + i);
                        match db::insert(&c, &e) {
                            Ok(()) => {}
                            Err(e) if is_busy(&e) => busy += 1,
                            Err(e) => return Err(format!("insert: {e}")),
                        }
                    }
                    Ok(busy)
                }));
            }
            let mut out = Ok(0u64);
            for h in handles {
                match h.join() {
                    Ok(Ok(b)) => {
                        if let Ok(total) = &mut out {
                            *total += b;
                        }
                    }
                    Ok(Err(e)) => out = Err(e),
                    Err(_) => out = Err("worker panicked".into()),
                }
            }
            out
        })
        .map(|b| busy_count = b)?;
    }

    // Sizes + schema inventory for the gate/CI artifact.
    let wal = dbp.with_extension("db-wal");
    let wal_bytes = file_bytes(&wal);
    let db_bytes = file_bytes(&dbp);
    let binary_bytes = file_bytes(&exe);
    let objs = db::schema_objects(&conn).map_err(|e| format!("schema: {e}"))?;
    let need = ["history", "history_fts", "idx_cwd", "idx_session", "idx_time"];
    let indexes_ok = need.iter().all(|n| objs.iter().any(|o| o == n));

    let report = BenchReport {
        n: opts.n,
        reps: opts.reps,
        concurrency: opts.concurrency,
        log_ms_p50: pct(log_ms.clone(), 50.0),
        log_ms_p99: pct(log_ms, 99.0),
        inserts_per_s,
        search_ms_p50: pct(search_ms.clone(), 50.0),
        search_ms_p99: pct(search_ms, 99.0),
        suggest_ms_p50: pct(suggest_ms.clone(), 50.0),
        suggest_ms_p99: pct(suggest_ms, 99.0),
        hook_ms_p50,
        hook_ms_p99,
        hook_subprocess,
        busy_count,
        wal_bytes,
        db_bytes,
        binary_bytes,
        rss_kb: rss_kb(),
        indexes_ok,
    };

    drop(conn);
    if let Some(d) = owned_dir {
        std::fs::remove_dir_all(&d).ok();
    }
    Ok(report)
}

/// Gate vs `08-performance-budgets.md` thresholds. Pure logic (no timing),
/// so unit tests prove enforcement deterministically (non-vacuous gate).
pub fn check_gate(report: &BenchReport, opts: &BenchOpts) -> Vec<GateCheck> {
    vec![
        GateCheck {
            name: "log_ms_p99",
            ok: report.log_ms_p99 <= opts.max_log_ms,
            detail: format!("{:.3}ms <= {:.3}ms", report.log_ms_p99, opts.max_log_ms),
        },
        GateCheck {
            name: "inserts_per_s",
            ok: report.inserts_per_s >= opts.min_inserts_per_s,
            detail: format!("{:.0}/s >= {:.0}/s", report.inserts_per_s, opts.min_inserts_per_s),
        },
        GateCheck {
            name: "search_ms_p50",
            ok: report.search_ms_p50 <= opts.max_search_ms,
            detail: format!("{:.3}ms <= {:.3}ms", report.search_ms_p50, opts.max_search_ms),
        },
        GateCheck {
            name: "suggest_ms_p50",
            ok: report.suggest_ms_p50 <= opts.max_suggest_ms,
            detail: format!("{:.3}ms <= {:.3}ms", report.suggest_ms_p50, opts.max_suggest_ms),
        },
        GateCheck {
            name: "hook_ms_p99",
            ok: report.hook_ms_p99 <= opts.max_hook_ms,
            detail: format!("{:.3}ms <= {:.3}ms", report.hook_ms_p99, opts.max_hook_ms),
        },
        GateCheck {
            name: "indexes",
            ok: report.indexes_ok,
            detail: "history, history_fts, idx_time, idx_cwd, idx_session present".into(),
        },
    ]
}

/// `ghostline bench [--n N] [--reps R] [--concurrency C] [--json] [--db PATH]
/// [--gate] [--max-log-ms X] [--max-search-ms X] [--max-suggest-ms X]
/// [--max-hook-ms X] [--min-inserts-per-s X]`
pub fn run_cli(args: &[String]) -> Result<(), String> {
    let mut opts = BenchOpts::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--n" => {
                i += 1;
                opts.n = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(10_000);
            }
            "--reps" => {
                i += 1;
                opts.reps = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(20);
            }
            "--concurrency" => {
                i += 1;
                opts.concurrency = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
            }
            "--db" => {
                i += 1;
                opts.db_path = args.get(i).map(PathBuf::from);
            }
            "--json" => opts.json = true,
            "--gate" => opts.gate = true,
            "--max-log-ms" => {
                i += 1;
                opts.max_log_ms = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(5.0);
            }
            "--max-search-ms" => {
                i += 1;
                opts.max_search_ms = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(50.0);
            }
            "--max-suggest-ms" => {
                i += 1;
                opts.max_suggest_ms = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(50.0);
            }
            "--max-hook-ms" => {
                i += 1;
                opts.max_hook_ms = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(100.0);
            }
            "--min-inserts-per-s" => {
                i += 1;
                opts.min_inserts_per_s = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(2000.0);
            }
            s if s.starts_with("--") => return Err(format!("unknown flag '{s}'")),
            s => return Err(format!("unexpected arg '{s}'")),
        }
        i += 1;
    }
    let report = run(&opts)?;
    // Representative plans: visible + diffable. Schema-only temp DB is
    // enough (plans don't need rows). Human mode prints to stdout;
    // JSON mode keeps stdout parseable and puts plans in the payload.
    let mut plan_lines: Vec<String> = vec![];
    if opts.gate {
        let mut pdir = std::env::temp_dir();
        pdir.push(format!("gl-bench-plan-{}", std::process::id()));
        let pdb = pdir.join("plan.db");
        let pc = db::open(&pdb).map_err(|e| format!("open db: {e}"))?;
        for row in db::search_plan(&pc).map_err(|e| format!("search plan: {e}"))? {
            plan_lines.push(format!("search:  {row}"));
        }
        for row in db::suggest_plan(&pc).map_err(|e| format!("suggest plan: {e}"))? {
            plan_lines.push(format!("suggest: {row}"));
        }
        let objs = db::schema_objects(&pc).map_err(|e| format!("schema: {e}"))?;
        plan_lines.push(format!("schema:  {}", objs.join(",")));
        drop(pc);
        std::fs::remove_dir_all(&pdir).ok();
    }
    let mut gate_checks: Vec<GateCheck> = vec![];
    let mut gate_failed = false;
    if opts.gate {
        for c in check_gate(&report, &opts) {
            gate_failed |= !c.ok;
            gate_checks.push(c);
        }
    }
    if opts.json {
        let payload = serde_json::json!({
            "report": report,
            "gate": gate_checks,
            "gate_pass": !gate_failed,
            "query_plans": plan_lines,
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        println!("ghostline bench: n={} reps={} concurrency={}", opts.n, opts.reps, opts.concurrency);
        println!("  log_ms        p50 {:8.3}  p99 {:8.3}", report.log_ms_p50, report.log_ms_p99);
        println!("  inserts_per_s {:8.0}", report.inserts_per_s);
        println!("  search_ms     p50 {:8.3}  p99 {:8.3}", report.search_ms_p50, report.search_ms_p99);
        println!("  suggest_ms    p50 {:8.3}  p99 {:8.3}", report.suggest_ms_p50, report.suggest_ms_p99);
        println!(
            "  hook_ms{}      p50 {:8.3}  p99 {:8.3}",
            if report.hook_subprocess { " (fork+exec)" } else { " (in-proc fallback)" },
            report.hook_ms_p50,
            report.hook_ms_p99
        );
        println!("  busy_count    {:8}  (concurrency {})", report.busy_count, opts.concurrency);
        println!("  wal_bytes     {:8}  db_bytes {:8}", report.wal_bytes, report.db_bytes);
        println!("  binary_bytes  {:8}  rss_kb {:8}", report.binary_bytes, report.rss_kb);
        println!("  indexes_ok    {}", report.indexes_ok);
        if opts.gate {
            println!("gate query plans (representative shapes):");
            for line in &plan_lines {
                println!("  {line}");
            }
            for c in &gate_checks {
                println!("gate {:<15} {} ({})", c.name, if c.ok { "PASS" } else { "FAIL" }, c.detail);
            }
        }
    }
    if gate_failed {
        return Err("bench gate FAILED".into());
    }
    if opts.gate && !opts.json {
        println!("gate: all PASS");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_opts() -> BenchOpts {
        BenchOpts {
            n: 300,
            reps: 5,
            concurrency: 1,
            ..BenchOpts::default()
        }
    }

    #[test]
    fn bench_small_runs_and_reports() {
        let r = run(&small_opts()).expect("bench run");
        assert_eq!(r.n, 300);
        assert!(r.inserts_per_s > 0.0, "throughput must be positive");
        assert!(r.log_ms_p99 >= r.log_ms_p50);
        assert!(r.indexes_ok, "budgeted schema objects must exist");
        assert!(!r.hook_subprocess, "cargo test harness is not the ghostline binary");
    }

    #[test]
    fn gate_passes_with_generous_budgets() {
        let mut o = small_opts();
        o.max_log_ms = 1000.0;
        o.max_search_ms = 1000.0;
        o.max_suggest_ms = 1000.0;
        o.max_hook_ms = 60_000.0;
        o.min_inserts_per_s = 1.0;
        let r = run(&o).expect("bench run");
        let checks = check_gate(&r, &o);
        assert!(checks.iter().all(|c| c.ok), "all gates pass: {checks:?}");
    }

    /// Non-vacuous gate, part 1: impossible budgets MUST fail (proves the
    /// gate enforces instead of rubber-stamping).
    #[test]
    fn gate_fails_with_impossible_budget() {
        let mut o = small_opts();
        o.max_log_ms = 0.000_001;
        let r = run(&o).expect("bench run");
        let checks = check_gate(&r, &o);
        let log = checks.iter().find(|c| c.name == "log_ms_p99").unwrap();
        assert!(!log.ok, "impossible budget must fail: {log:?}");
    }

    /// Non-vacuous gate, part 2: gate logic on synthetic numbers.
    #[test]
    fn gate_logic_rejects_bad_report() {
        let o = BenchOpts::default();
        let mut r = BenchReport {
            n: 10,
            reps: 1,
            concurrency: 1,
            log_ms_p50: 0.5,
            log_ms_p99: 999.0,
            inserts_per_s: 5000.0,
            search_ms_p50: 1.0,
            search_ms_p99: 2.0,
            suggest_ms_p50: 1.0,
            suggest_ms_p99: 2.0,
            hook_ms_p50: 2.0,
            hook_ms_p99: 3.0,
            hook_subprocess: false,
            busy_count: 0,
            wal_bytes: 0,
            db_bytes: 0,
            binary_bytes: 0,
            rss_kb: 0,
            indexes_ok: true,
        };
        assert!(check_gate(&r, &o).iter().any(|c| !c.ok));
        r.log_ms_p99 = 0.5;
        r.indexes_ok = false;
        let checks = check_gate(&r, &o);
        assert!(!checks.iter().find(|c| c.name == "indexes").unwrap().ok);
    }

    #[test]
    fn concurrency_counts_busy_without_error() {
        let o = BenchOpts {
            n: 400,
            reps: 2,
            concurrency: 4,
            ..BenchOpts::default()
        };
        let r = run(&o).expect("bench run");
        assert!(r.inserts_per_s > 0.0);
        // busy_count may be 0 on fast disks; assert the field exists and ran.
        let _ = r.busy_count;
    }

    #[test]
    fn explain_plans_are_valid() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("gl-bench-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = db::open(&dir.join("p.db")).unwrap();
        assert!(!db::search_plan(&conn).unwrap().is_empty());
        assert!(!db::suggest_plan(&conn).unwrap().is_empty());
        let objs = db::schema_objects(&conn).unwrap();
        for need in ["history", "history_fts", "idx_time", "idx_cwd", "idx_session"] {
            assert!(objs.contains(&need.to_string()), "missing {need}: {objs:?}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
