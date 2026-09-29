use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Optimal string alignment distance (Damerau-Levenshtein restricted to
/// adjacent transpositions). Case-insensitive: caller lowercases first;
/// exact-but-for-case matches are handled as distance 0 by callers.
pub fn osa(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for i in 0..=n {
        d[i][0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[n][m]
}

/// Git-style proportional threshold: reject if distance > max(2, len/3).
pub fn within_threshold(typed: &str, dist: usize) -> bool {
    dist <= (2usize.max(typed.len() / 3))
}

/// Adjacent-transposition check: same length, exactly one swapped adjacent pair.
/// Used to prefer `gti→git` (transposition, the common typo class) over `gti→gtf`.
fn is_transposition(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len() != b.len() {
        return false;
    }
    let diffs: Vec<usize> = a.iter().zip(b.iter()).enumerate().filter(|(_, (x, y))| x != y).map(|(i, _)| i).collect();
    diffs.len() == 2 && diffs[1] == diffs[0] + 1 && a[diffs[0]] == b[diffs[1]] && a[diffs[1]] == b[diffs[0]]
}

/// Best candidate within threshold, case-insensitive.
/// Tie-breaks: history membership (user's own vocabulary) → transposition → shorter distance → order.
pub fn best<'a>(
    typed: &str,
    candidates: impl IntoIterator<Item = &'a str>,
    preferred: &HashSet<&str>,
) -> Option<&'a str> {
    let lower = typed.to_lowercase();
    // (exact_fold desc, preferred desc, transposition desc, dist asc)
    let mut best: Option<(&str, bool, bool, bool, usize)> = None;
    for c in candidates {
        let cl = c.to_lowercase();
        let d = osa(&lower, &cl);
        if !within_threshold(&lower, d) {
            continue;
        }
        let key = (cl == lower, preferred.contains(c), is_transposition(&lower, &cl), d);
        let take = match best {
            None => true,
            Some((_, bex, bpref, btrans, bd)) => {
                let (ex, pref, trans, _) = key;
                (ex, pref, trans, bd.saturating_sub(d)) > (bex, bpref, btrans, 0)
            }
        };
        if take {
            best = Some((c, key.0, key.1, key.2, d));
        }
    }
    best.map(|(c, _, _, _, _)| c)
}

const SHELL_BUILTINS: &[&str] = &[
    "cd", "echo", "export", "alias", "unalias", "source", ".", "exit", "return", "set", "unset",
    "shift", "test", "[", "pwd", "jobs", "fg", "bg", "kill", "wait", "read", "printf", "true",
    "false", "type", "command", "exec", "eval", "trap", "umask", "history", "fc", "let", "declare",
    "local", "readonly", "pushd", "popd", "dirs", "suspend", "ulimit", "bind", "shopt", "complete",
];

/// Verbs seen in history (first tokens), for command-name correction.
pub fn history_verbs(history: &[String]) -> Vec<&str> {
    let mut seen = HashSet::new();
    let mut out = vec![];
    for h in history {
        if let Some(v) = h.split_whitespace().next() {
            if seen.insert(v) {
                out.push(v);
            }
        }
    }
    out
}

/// Is `prog` runnable (PATH lookup or builtin)? Used to decide if token[0] is a typo.
pub fn known_command(prog: &str, path_bins: &HashSet<String>) -> bool {
    if prog.contains('/') {
        return Path::new(prog).exists();
    }
    SHELL_BUILTINS.contains(&prog) || path_bins.contains(prog)
}

/// Collect executable basenames from $PATH (one scandir per dir, no recursion).
pub fn path_bins() -> HashSet<String> {
    let mut out = HashSet::new();
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for ent in rd.flatten() {
            let p = ent.path();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let Ok(md) = ent.metadata() else { continue };
                if !md.is_file() || md.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                out.insert(name.to_string());
            }
        }
    }
    out
}

const GIT_SUBCOMMANDS: &[&str] = &[
    "add", "bisect", "branch", "checkout", "clone", "commit", "diff", "fetch", "grep", "init",
    "log", "merge", "mv", "pull", "push", "rebase", "reset", "restore", "rm", "show", "stash",
    "status", "switch", "tag", "worktree", "config", "remote", "reflog", "clean", "cherry-pick",
    "describe", "apply", "am", "blame", "shortlog", "ls-files", "rev-parse", "submodule", "gc",
];

/// Branches in cwd's repo (empty outside a repo — fails fast, no output parsed on error).
pub fn git_branches(cwd: &str) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args(["-C", cwd, "branch", "--format=%(refname:short)"])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => vec![],
    }
}

/// Resolve `path` against `cwd`; if a component is missing, walk up to find the
/// deepest existing ancestor and the first missing component.
fn missing_component(cwd: &str, path: &str) -> Option<(PathBuf, String)> {
    let full = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        Path::new(cwd).join(path)
    };
    if full.exists() {
        return None;
    }
    let mut cur = full.clone();
    let mut missing: Vec<String> = vec![];
    while !cur.exists() {
        match cur.file_name().and_then(|s| s.to_str()) {
            Some(name) => missing.push(name.to_string()),
            None => return None, // hit filesystem root without finding anything existing
        }
        match cur.parent() {
            Some(p) if !p.as_os_str().is_empty() => cur = p.to_path_buf(),
            _ => return None,
        }
    }
    missing.into_iter().next().map(|first| (cur, first))
}

/// Suggest a fix for a failed command. Filesystem-grounded only (no stderr available).
/// Returns up to 3 candidates, best first. `history` = recent commands for verb/flag learning.
pub fn suggest(cmd: &str, cwd: &str, history: &[String]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    if toks.is_empty() {
        return out;
    }

    // Rule 0: swapped program order — `checkout git main` (token[0] unknown, token[1] known).
    if toks.len() >= 2 {
        let bins = path_bins();
        if !known_command(toks[0], &bins) && known_command(toks[1], &bins) {
            let mut fixed = vec![toks[1], toks[0]];
            fixed.extend_from_slice(&toks[2..]);
            out.push(fixed.join(" "));
        }
    }

    // Rule 1: unknown command name — typo over PATH + builtins + history verbs.
    {
        let bins = path_bins();
        if !known_command(toks[0], &bins) && out.is_empty() {
            let verbs = history_verbs(history);
            let preferred: HashSet<&str> = verbs.iter().copied().collect();
            let mut pool: Vec<&str> = vec![];
            for b in &bins {
                pool.push(b.as_str());
            }
            pool.extend_from_slice(SHELL_BUILTINS);
            pool.extend(verbs);
            if let Some(hit) = best(toks[0], pool, &preferred) {
                let mut fixed = vec![hit.to_string()];
                fixed.extend(toks[1..].iter().map(|s| s.to_string()));
                out.push(fixed.join(" "));
            }
        }
    }

    // Rule 2: git subcommand / branch typo.
    if toks[0] == "git" && toks.len() >= 2 {
        let no_pref: HashSet<&str> = HashSet::new();
        if !GIT_SUBCOMMANDS.contains(&toks[1]) {
            if let Some(hit) = best(toks[1], GIT_SUBCOMMANDS.iter().copied(), &no_pref) {
                let mut fixed = vec!["git".to_string(), hit.to_string()];
                fixed.extend(toks[2..].iter().map(|s| s.to_string()));
                out.push(fixed.join(" "));
            }
        } else if toks[1] == "checkout" || toks[1] == "switch" {
            if let Some(branch_arg) = toks.get(2) {
                let branches = git_branches(cwd);
                if !branches.is_empty() && !branches.iter().any(|b| b == branch_arg) {
                    let refs: Vec<&str> = branches.iter().map(|s| s.as_str()).collect();
                    if let Some(hit) = best(branch_arg, refs, &no_pref) {
                        let mut fixed = vec!["git".to_string(), toks[1].to_string(), hit.to_string()];
                        fixed.extend(toks[3..].iter().map(|s| s.to_string()));
                        out.push(fixed.join(" "));
                    }
                }
            }
        }
    }

    // Rule 3: cd case / path typo (first missing path component).
    if (toks[0] == "cd" || toks[0] == "cat" || toks[0] == "ls" || toks[0] == "touch")
        && toks.len() >= 2
    {
        if let Some((parent, missing)) = missing_component(cwd, toks[1]) {
            if let Ok(rd) = std::fs::read_dir(&parent) {
                let siblings: Vec<String> = rd
                    .flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect();
                // Case-only fix first (the `cd downloads` class).
                if let Some(exact) = siblings.iter().find(|s| s.to_lowercase() == missing.to_lowercase()) {
                    let fixed_path = parent.join(exact);
                    let display = pretty_path(cwd, &fixed_path);
                    let mut fixed = vec![toks[0].to_string(), display];
                    fixed.extend(toks[2..].iter().map(|s| s.to_string()));
                    out.push(fixed.join(" "));
                } else {
                    let refs: Vec<&str> = siblings.iter().map(|s| s.as_str()).collect();
                    let no_pref: HashSet<&str> = HashSet::new();
                    if let Some(hit) = best(&missing, refs, &no_pref) {
                        let fixed_path = parent.join(hit);
                        let display = pretty_path(cwd, &fixed_path);
                        let mut fixed = vec![toks[0].to_string(), display];
                        fixed.extend(toks[2..].iter().map(|s| s.to_string()));
                        out.push(fixed.join(" "));
                    }
                }
            }
        }
    }

    // Rule 4: missing `mkdir -p` — touch/mkdir/cp/mv with missing intermediate dir.
    if matches!(toks[0], "touch" | "mkdir" | "cp" | "mv") && toks.len() >= 2 {
        // Last arg is the path that needs its parent to exist (for cp/mv target likewise).
        let target = toks.last().unwrap();
        let full = if Path::new(target).is_absolute() {
            PathBuf::from(target)
        } else {
            Path::new(cwd).join(target)
        };
        let parent = if toks[0] == "mkdir" { full.clone() } else { full.parent().map(|p| p.to_path_buf()).unwrap_or(full.clone()) };
        if !parent.exists() {
            if let Some(dir) = parent.to_str() {
                let display = pretty_path(cwd, &parent);
                let _ = dir;
                out.push(format!("mkdir -p {display} && {cmd}"));
            }
        }
    }

    // Rule 5: flag typo from history — same program (+subcommand), closest flag spelling.
    if toks.len() >= 2 {
        if let Some(bad_flag) = toks.iter().find(|t| t.starts_with('-') && t.len() > 2) {
            let prefix: Vec<&str> = toks[..toks.iter().position(|t| *t == *bad_flag).unwrap()].to_vec();
            let mut flag_pool: Vec<&str> = vec![];
            for h in history {
                let ht: Vec<&str> = h.split_whitespace().collect();
                if ht.len() > prefix.len() && ht[..prefix.len()] == prefix[..] {
                    for t in &ht[prefix.len()..] {
                        if t.starts_with('-') {
                            flag_pool.push(t);
                        }
                    }
                }
            }
            if let Some(hit) = best(bad_flag, flag_pool, &HashSet::new()) {
                out.push(cmd.replacen(bad_flag, hit, 1));
            }
        }
    }

    // Rule 6 (heuristic, ranked last): missing sudo for root-needing prefixes on failure.
    // No stderr available, so this only fires for a narrow allowlist and is always last.
    const SUDO_PREFIXES: &[&str] = &[
        "apt install", "apt-get install", "systemctl", "mkdir /etc/", "mkdir /opt/",
        "tee /etc/", "tee /sys/",
    ];
    if SUDO_PREFIXES.iter().any(|p| cmd.starts_with(p)) {
        out.push(format!("sudo {cmd}"));
    }

    // Dedupe, cap 3. Never suggest destructive rewrites (no rm/dd/chmod rules by design).
    let mut seen = HashSet::new();
    out.into_iter().filter(|c| seen.insert(c.clone())).take(3).collect()
}

fn pretty_path(cwd: &str, p: &Path) -> String {
    match p.strip_prefix(cwd) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().into_owned(),
        _ => {
            if let Some(home) = std::env::var_os("HOME") {
                if let Ok(rel) = p.strip_prefix(home) {
                    return format!("~/{}", rel.to_string_lossy());
                }
            }
            p.to_string_lossy().into_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osa_transposition_is_one() {
        assert_eq!(osa("gti", "git"), 1);
        assert_eq!(osa("chekout", "checkout"), 1);
        assert_eq!(osa("abc", "abc"), 0);
    }

    #[test]
    fn threshold_scales_with_length() {
        assert!(within_threshold("ls", 2)); // floor of 2 for short tokens
        assert!(!within_threshold("ls", 3));
        assert!(within_threshold("checkout", 2));
        assert!(!within_threshold("checkout", 3));
    }

    #[test]
    fn best_prefers_casefold_exact() {
        let pool = vec!["Download-old", "Downloads"];
        assert_eq!(best("downloads", pool, &HashSet::new()), Some("Downloads"));
    }

    #[test]
    fn best_prefers_transposition_then_history() {
        // gti->git is a transposition, gti->gtf a substitution: git wins with no history.
        let pool = vec!["gtf", "git"];
        assert_eq!(best("gti", pool.clone(), &HashSet::new()), Some("git"));
        // history membership beats even transposition.
        let pref: HashSet<&str> = ["gtf"].into_iter().collect();
        assert_eq!(best("gti", pool, &pref), Some("gtf"));
    }

    #[test]
    fn swapped_order() {
        let out = suggest("checkout git main", "/tmp", &[]);
        assert!(out.iter().any(|s| s == "git checkout main"));
    }

    #[test]
    fn cd_case_fixed() {
        let dir = std::env::temp_dir().join(format!("gl-fixtest-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Downloads")).unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        let out = suggest("cd downloads", &cwd, &[]);
        assert!(out.iter().any(|s| s == "cd Downloads"), "got: {out:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn path_typo_fixed() {
        let dir = std::env::temp_dir().join(format!("gl-fixtypo-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Documents")).unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        let out = suggest("cd Documnets", &cwd, &[]);
        assert!(out.iter().any(|s| s == "cd Documents"), "got: {out:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn git_subcommand_fixed() {
        let out = suggest("git chekout main", "/tmp", &[]);
        assert!(out.iter().any(|s| s == "git checkout main"), "got: {out:?}");
    }

    #[test]
    fn mkdir_p_suggested() {
        let dir = std::env::temp_dir().join(format!("gl-fixmkdir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        let out = suggest("touch a/b/c.txt", &cwd, &[]);
        assert!(out.iter().any(|s| s.starts_with("mkdir -p") && s.contains("touch a/b/c.txt")), "got: {out:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_absurd_suggestions() {
        let out = suggest("rm -rf / tmp/foo", "/tmp", &[]);
        assert!(!out.iter().any(|s| s.contains("rm")), "got: {out:?}");
        let out2 = suggest("ls", "/tmp", &[]);
        assert!(out2.is_empty(), "got: {out2:?}");
    }
}
