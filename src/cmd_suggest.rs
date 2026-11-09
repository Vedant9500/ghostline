use crate::{complete, db};

/// Suffix to paint for `cand` when `buffer` is its prefix, or None when the
/// candidate is not a prefix or would produce an empty ghost. SQLite's LIKE is
/// ASCII case-insensitive while `strip_prefix` is exact, so compare the head
/// case-insensitively and slice by buffer byte length. `get` keeps multibyte
/// boundaries safe.
fn ghost_suffix<'a>(cand: &'a str, buffer: &str) -> Option<&'a str> {
    if cand.len() <= buffer.len() {
        return None;
    }
    let head = cand.get(..buffer.len())?;
    if head.eq_ignore_ascii_case(buffer) {
        Some(&cand[buffer.len()..])
    } else {
        None
    }
}

/// `ghostline suggest [--cwd DIR] [--limit N] [--all] <buffer>`
/// Default prints only the ghost suffix (what comes after the typed buffer),
/// so shells can render it dim. `--all` prints full candidate commands.
/// History first (dir-aware frecency); when history misses and the buffer is
/// a single command token, fall back to command-name completion (the pool
/// behind Tab: PATH executables + builtins).
pub fn run(args: &[String]) -> Result<(), String> {
    let mut cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut limit: i64 = 5;
    let mut all = false;
    let mut db_path: Option<String> = None;
    let mut buf_parts: Vec<String> = vec![];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--cwd" => {
                i += 1;
                cwd = args.get(i).cloned().unwrap_or_default();
            }
            "--limit" | "-n" => {
                i += 1;
                limit = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(5);
            }
            "--all" => all = true,
            "--db" => {
                i += 1;
                db_path = args.get(i).cloned();
            }
            "--" => {
                buf_parts.extend_from_slice(&args[i + 1..]);
                break;
            }
            s if s.starts_with("--") => return Err(format!("unknown flag '{s}'")),
            _ => buf_parts.push(args[i].clone()),
        }
        i += 1;
    }
    let buffer = buf_parts.join(" ");
    if buffer.len() > 200 {
        return Ok(()); // size guard: never suggest for huge buffers
    }
    if buffer.trim().is_empty() {
        return Ok(());
    }
    let path = db_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(db::default_db_path);
    let conn = db::open(&path).map_err(|e| format!("open db: {e}"))?;
    let mut cands = db::suggest(&conn, &buffer, &cwd, limit).map_err(|e| format!("suggest: {e}"))?;
    if cands.is_empty() && !buffer.contains(char::is_whitespace) {
        cands = complete::complete_command(&buffer)
            .into_iter()
            .take(limit.max(0) as usize)
            .collect();
    }
    if all {
        for c in &cands {
            println!("{c}");
        }
    } else if let Some(suffix) = cands.iter().find_map(|c| ghost_suffix(c, &buffer)) {
        print!("{suffix}");
        use std::io::Write;
        std::io::stdout().flush().ok();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ghost_suffix;

    #[test]
    fn exact_case_prefix() {
        assert_eq!(ghost_suffix("git stash", "git"), Some(" stash"));
    }

    #[test]
    fn case_insensitive_prefix() {
        assert_eq!(ghost_suffix("Git status", "git"), Some(" status"));
    }

    #[test]
    fn full_match_has_no_suffix() {
        assert_eq!(ghost_suffix("git", "git"), None);
        assert_eq!(ghost_suffix("Git status", "git status"), None);
    }

    #[test]
    fn non_prefix_is_rejected() {
        assert_eq!(ghost_suffix("cargo test", "git"), None);
    }

    #[test]
    fn multibyte_boundaries_are_safe() {
        assert_eq!(ghost_suffix("éclair", "e"), None);
        assert_eq!(ghost_suffix("éclair", "é"), Some("clair"));
    }
}
