use crate::{db, redact};

/// `ghostline forget [--db P] [--dry-run] [--vacuum]`
/// `                  [--last-n N | --older-than DAYS | --max-rows N | --all | PATTERN...]`
///
/// Deletes matching history rows (and their FTS entries via the delete
/// trigger). `--max-rows N` keeps the newest N rows and removes the rest.
pub fn run(args: &[String]) -> Result<(), String> {
    let mut db_path: Option<String> = None;
    let mut dry_run = false;
    let mut full_vacuum = false;
    let mut last_n: Option<i64> = None;
    let mut older_than: Option<i64> = None;
    let mut max_rows: Option<i64> = None;
    let mut all = false;
    let mut patterns: Vec<String> = vec![];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--db" => {
                i += 1;
                db_path = args.get(i).cloned();
            }
            "--dry-run" => dry_run = true,
            "--vacuum" => full_vacuum = true,
            "--last-n" => {
                i += 1;
                last_n = Some(parse_count(args.get(i), "--last-n")?);
            }
            "--older-than" => {
                i += 1;
                older_than = Some(parse_count(args.get(i), "--older-than")?);
            }
            "--max-rows" => {
                i += 1;
                max_rows = Some(parse_count(args.get(i), "--max-rows")?);
            }
            "--all" => all = true,
            "--" => {
                patterns.extend_from_slice(&args[i + 1..]);
                break;
            }
            s if s.starts_with("--") => return Err(format!("unknown flag '{s}'")),
            s => patterns.push(s.to_string()),
        }
        i += 1;
    }

    let selectors = [
        last_n.is_some(),
        older_than.is_some(),
        max_rows.is_some(),
        all,
        !patterns.is_empty(),
    ]
    .into_iter()
    .filter(|v| *v)
    .count();
    if selectors != 1 {
        return Err("usage: ghostline forget [--db P] [--dry-run] [--vacuum] \
             [--last-n N | --older-than DAYS | --max-rows N | --all | PATTERN...]"
            .into());
    }
    if patterns.iter().any(|p| p.is_empty()) {
        return Err("forget pattern must not be empty".into());
    }

    let mode = if let Some(n) = last_n {
        db::ForgetMode::LastN(n)
    } else if let Some(days) = older_than {
        db::ForgetMode::OlderThanDays(days)
    } else if let Some(n) = max_rows {
        db::ForgetMode::MaxRows(n)
    } else if all {
        db::ForgetMode::All
    } else {
        db::ForgetMode::Patterns(&patterns)
    };

    let path = db_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(db::default_db_path);
    let conn = db::open(&path).map_err(|e| format!("open db: {e}"))?;
    let report = db::forget(&conn, &mode, dry_run).map_err(|e| format!("forget: {e}"))?;

    if report.matched == 0 {
        println!("no commands matched");
        return Ok(());
    }
    if dry_run {
        for (ts, exit, cwd, cmd) in &report.rows {
            println!(
                "{ts}\t{exit}\t{}\t{}",
                redact::strip_controls(cwd),
                redact::strip_controls(cmd)
            );
        }
        println!(
            "would delete {} command(s){}",
            report.matched,
            if report.rows.len() < report.matched as usize {
                " (newest 20 shown)"
            } else {
                ""
            }
        );
        return Ok(());
    }

    println!("deleted {} command(s)", report.deleted);
    db::vacuum(&conn, full_vacuum).map_err(|e| format!("vacuum: {e}"))?;
    Ok(())
}

fn parse_count(raw: Option<&String>, flag: &str) -> Result<i64, String> {
    let raw = raw.ok_or_else(|| format!("{flag} expects a value"))?;
    let n = raw
        .parse::<i64>()
        .map_err(|_| format!("invalid {flag} value '{raw}'"))?;
    if n < 0 {
        return Err(format!("{flag} must be >= 0"));
    }
    Ok(n)
}
