use crate::{db, redact};

const NS_PER_SEC: i64 = 1_000_000_000;
/// Values below this are user-facing Unix seconds; larger values are already
/// nanoseconds (the storage unit). 1e12 seconds is year 33658, so the cutoff is
/// unambiguous for real histories.
const NS_INPUT_THRESHOLD: i64 = 1_000_000_000_000;

fn since_to_ns(raw: i64) -> Option<i64> {
    if raw < 0 {
        return None;
    }
    if raw >= NS_INPUT_THRESHOLD {
        Some(raw)
    } else {
        raw.checked_mul(NS_PER_SEC)
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let mut limit: i64 = 20;
    let mut dir: Option<String> = None;
    let mut exit_filter: Option<String> = None;
    let mut session: Option<String> = None;
    let mut host: Option<String> = None;
    let mut since: Option<i64> = None;
    let mut db_path: Option<String> = None;
    let mut query_parts: Vec<String> = vec![];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" | "-n" => {
                i += 1;
                limit = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(20);
            }
            "--dir" => {
                i += 1;
                dir = args.get(i).cloned();
            }
            "--exit" => {
                i += 1;
                let raw = args.get(i).ok_or("--exit expects a value")?;
                db::parse_exit_filter(raw)
                    .ok_or_else(|| format!("invalid --exit value '{raw}' (use N or !N)"))?;
                exit_filter = Some(raw.clone());
            }
            "--session" => {
                i += 1;
                session = args.get(i).cloned();
            }
            "--host" => {
                i += 1;
                host = args.get(i).cloned();
            }
            "--since" => {
                i += 1;
                let raw = args.get(i).ok_or("--since expects a value")?;
                let raw = raw
                    .parse::<i64>()
                    .map_err(|_| format!("invalid --since value '{raw}'"))?;
                since =
                    Some(since_to_ns(raw).ok_or("--since must be >= 0 and fit in nanoseconds")?);
            }
            "--db" => {
                i += 1;
                db_path = args.get(i).cloned();
            }
            "--" => {
                query_parts.extend_from_slice(&args[i + 1..]);
                break;
            }
            s if s.starts_with("--") => return Err(format!("unknown flag '{s}'")),
            _ => query_parts.push(args[i].clone()),
        }
        i += 1;
    }
    let query = query_parts.join(" ");
    let path = db_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(db::default_db_path);
    let Some(conn) = db::open_read(&path).map_err(|e| format!("open db: {e}"))? else {
        return Ok(()); // no history yet: do not create an empty database
    };
    let rows = db::search(
        &conn,
        &query,
        limit,
        dir.as_deref(),
        exit_filter.as_deref(),
        session.as_deref(),
        host.as_deref(),
        since,
    )
    .map_err(|e| format!("search: {e}"))?;
    for (ts, exit, cwd, cmd) in rows {
        // Databases written before write-time sanitization may still contain
        // terminal escapes; never replay them to the caller's terminal.
        println!(
            "{ts}\t{exit}\t{}\t{}",
            redact::strip_controls(&cwd),
            redact::strip_controls(&cmd)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::since_to_ns;

    #[test]
    fn seconds_are_expanded_to_nanoseconds() {
        assert_eq!(since_to_ns(1_700_000_000), Some(1_700_000_000_000_000_000));
        assert_eq!(since_to_ns(0), Some(0));
    }

    #[test]
    fn nanosecond_input_passes_through() {
        assert_eq!(
            since_to_ns(1_700_000_000_000_000_000),
            Some(1_700_000_000_000_000_000)
        );
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert_eq!(since_to_ns(-1), None);
        assert_eq!(since_to_ns(10_000_000_000), None); // ns overflow
    }
}
