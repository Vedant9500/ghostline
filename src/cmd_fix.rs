use crate::{db, fix, redact};

/// `ghostline fix [--last] [--exit CODE] [--cwd DIR] [--db P] [-q] [--] [command...]`
/// Prints up to 3 `did you mean` candidates (human format), or just the top
/// candidate with `-q` for scripting. Nothing is executed. Exit 0 = found,
/// 1 = none or error.
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
            "--" => {
                cmd_parts.extend_from_slice(&args[i + 1..]);
                break;
            }
            "--last" => last = true,
            "-q" | "--quiet" => quiet = true,
            "--exit" | "--exit-code" => {
                let flag = args[i].clone();
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| format!("{flag} requires a value"))?;
                let code: i64 = value
                    .parse()
                    .map_err(|_| format!("invalid exit code '{value}'"))?;
                if !(0..=255).contains(&code) {
                    return Err(format!("exit code out of range: {code}"));
                }
                exit_code = Some(code);
                i += 1;
            }
            "--cwd" => {
                let value = args
                    .get(i + 1)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "--cwd requires a non-empty value".to_string())?;
                cwd = value.clone();
                i += 1;
            }
            "--db" => {
                let value = args
                    .get(i + 1)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "--db requires a non-empty value".to_string())?;
                db_path = Some(value.clone());
                i += 1;
            }
            flag if flag.starts_with('-') && flag != "-" => {
                return Err(format!("unknown flag '{flag}'"));
            }
            _ => {
                // The first non-option starts the command; anything after it
                // is an argument, even if it looks like a flag.
                cmd_parts.extend_from_slice(&args[i..]);
                break;
            }
        }
        i += 1;
    }
    if last && !cmd_parts.is_empty() {
        return Err("--last cannot be combined with an explicit command".into());
    }
    if last && exit_code.is_some() {
        return Err("--last cannot be combined with --exit".into());
    }

    let path = db_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(db::default_db_path);
    let conn = match db::open_read(&path) {
        Ok(conn) => conn,
        Err(e) if last => return Err(format!("open db: {e}")),
        Err(e) => {
            // History is context, not a prerequisite for filesystem rules.
            eprintln!("ghostline fix: warning: cannot read history database: {e}");
            None
        }
    };

    // Resolve target command: explicit args, or most recent non-zero-exit row.
    let (cmd, cwd, exit, sensitive) = if last {
        let row: Option<(String, String, i64, i64)> = match &conn {
            Some(conn) => {
                let mut stmt = conn
                    .prepare("SELECT cmd, cwd, exit_code, sensitive FROM history WHERE exit_code != 0 ORDER BY started_at DESC, id DESC LIMIT 1")
                    .map_err(|e| format!("query last failure: {e}"))?;
                stmt.query_row([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                    .ok()
            }
            None => None,
        };
        match row {
            Some((cmd, cwd, exit, sensitive)) => {
                let sensitive = sensitive != 0 || redact::redact(&cmd).1;
                (cmd, cwd, Some(exit), sensitive)
            }
            None => {
                println!("no failed commands in history");
                std::process::exit(1);
            }
        }
    } else {
        if cmd_parts.is_empty() {
            return Err("usage: ghostline fix [--last] [command...]".into());
        }
        let cmd = cmd_parts.join(" ");
        let sensitive = redact::redact(&cmd).1;
        (cmd, cwd.clone(), exit_code, sensitive)
    };
    let cwd = if cwd.is_empty() {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        cwd
    };
    if sensitive {
        if !quiet {
            println!("no suggestion for a sensitive command");
        }
        std::process::exit(1);
    }

    // History context for verb/flag learning: recent successful, non-sensitive
    // commands sharing the first token, excluding the command being repaired.
    let first = cmd.split_whitespace().next().unwrap_or("").to_string();
    let history: Vec<String> = match &conn {
        Some(conn) => {
            let rows = (|| -> rusqlite::Result<Vec<String>> {
                let mut stmt = conn.prepare(
                    "SELECT cmd FROM history
                     WHERE cmd LIKE ?1 || '%'
                       AND cmd <> ?2
                       AND exit_code = 0
                       AND sensitive = 0
                     ORDER BY started_at DESC, id DESC LIMIT 200",
                )?;
                stmt.query_map([first.as_str(), cmd.as_str()], |r| r.get(0))?
                    .collect()
            })();
            match rows {
                Ok(rows) => rows,
                Err(e) => {
                    eprintln!("ghostline fix: warning: history query failed: {e}");
                    Vec::new()
                }
            }
        }
        None => Vec::new(),
    };

    let cands = fix::suggest_with_exit(&cmd, &cwd, &history, exit);
    if cands.is_empty() {
        if !quiet {
            println!("no suggestion for: {}", redact::strip_controls(&cmd));
        }
        std::process::exit(1);
    }
    if quiet {
        println!("{}", cands[0]);
    } else {
        for (n, c) in cands.iter().enumerate() {
            if n == 0 {
                println!("did you mean: {c}?");
            } else {
                println!("  also: {c}");
            }
        }
    }
    Ok(())
}
