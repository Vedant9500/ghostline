use crate::db;

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
                exit_filter = args.get(i).cloned();
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
                since = args.get(i).and_then(|s| s.parse().ok());
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
    let conn = db::open(&path).map_err(|e| format!("open db: {e}"))?;
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
        println!("{ts}\t{exit}\t{cwd}\t{cmd}");
    }
    Ok(())
}
