use crate::{db, fix};

/// `ghostline fix [--last] [--exit CODE] [--cwd DIR] [--db P] [-q] [command...]`
/// Prints up to 3 `did you mean` candidates (human format), or just the top
/// candidate with -q (for `Enter to run` wiring). Exit 0 = found, 1 = none.
pub fn run(args: &[String]) -> Result<(), String> {
    let mut last = false;
    let mut quiet = false;
    let mut exit_code: Option<i64> = None;
    let mut cwd = String::new();
    let mut db_path: Option<String> = None;
    let mut cmd_parts: Vec<String> = vec![];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--last" => last = true,
            "-q" | "--quiet" => quiet = true,
            "--exit" | "--exit-code" => {
                i += 1;
                exit_code = args.get(i).and_then(|s| s.parse().ok());
            }
            "--cwd" => {
                i += 1;
                cwd = args.get(i).cloned().unwrap_or_default();
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
    let path = db_path.map(std::path::PathBuf::from).unwrap_or_else(db::default_db_path);
    let conn = db::open(&path).map_err(|e| format!("open db: {e}"))?;

    // Resolve target command: explicit args, or most recent non-zero-exit row.
    let (cmd, cwd, _exit) = if last {
        let mut stmt = conn
            .prepare("SELECT cmd, cwd, exit_code FROM history WHERE exit_code != 0 ORDER BY started_at DESC LIMIT 1")
            .map_err(|e| format!("query last failure: {e}"))?;
        let row: Option<(String, String, i64)> = stmt
            .query_row([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .ok();
        match row {
            Some(r) => r,
            None => {
                println!("no failed commands in history");
                std::process::exit(1);
            }
        }
    } else {
        if cmd_parts.is_empty() {
            return Err("usage: ghostline fix [--last] [command...]".into());
        }
        let c = cwd.clone();
        (cmd_parts.join(" "), c, exit_code.unwrap_or(1))
    };
    let cwd = if cwd.is_empty() {
        std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
    } else {
        cwd
    };

    // History context for verb/flag learning: recent commands sharing the first token.
    let first = cmd.split_whitespace().next().unwrap_or("").to_string();
    let history: Vec<String> = conn
        .prepare("SELECT cmd FROM history WHERE cmd LIKE ?1 || '%' ORDER BY started_at DESC LIMIT 200")
        .map_err(|e| format!("history: {e}"))?
        .query_map([first], |r| r.get(0))
        .map_err(|e| format!("history: {e}"))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("history: {e}"))?;

    let cands = fix::suggest(&cmd, &cwd, &history);
    if cands.is_empty() {
        if !quiet {
            println!("no suggestion for: {cmd}");
        }
        std::process::exit(1);
    }
    if quiet {
        println!("{}", cands[0]);
    } else {
        for (n, c) in cands.iter().enumerate() {
            if n == 0 {
                println!("did you mean: {c}?  [Enter run | e edit | d dismiss]");
            } else {
                println!("  also: {c}");
            }
        }
    }
    Ok(())
}
