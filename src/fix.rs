use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::redact;

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
    "builtin",
];

/// Shell syntax that must never be treated as a command name.
const SHELL_RESERVED: &[&str] = &[
    "!", "{", "}", "[[", "]]", "if", "then", "elif", "else", "fi", "for", "while", "until", "do",
    "done", "case", "esac", "in", "function", "select", "time", "coproc",
];

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_shell_syntax(word: &str) -> bool {
    SHELL_RESERVED.contains(&word) || is_assignment(word)
}

/// Verbs seen in history (first tokens), for command-name correction.
pub fn history_verbs(history: &[String]) -> Vec<&str> {
    let mut seen = HashSet::new();
    let mut out = vec![];
    for h in history {
        if let Some(v) = h.split_whitespace().next()
            && !is_shell_syntax(v)
            && seen.insert(v)
        {
            out.push(v);
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

/// A shell word with both its unquoted value and the exact source spelling.
/// Keeping `raw` lets us preserve globs, quotes, and escapes in untouched
/// arguments while still diagnosing and quoting only the token we change.
#[derive(Clone, Debug)]
struct ShellToken {
    value: String,
    raw: String,
}

/// Split a simple command into words. Returns `None` for anything that would
/// require a real shell parser: operators, substitutions, control characters,
/// or unbalanced quoting. Suggestions are safer than guesses.
fn shell_split(cmd: &str) -> Option<Vec<ShellToken>> {
    if cmd.chars().any(|c| c.is_control()) {
        return None;
    }
    let chars: Vec<char> = cmd.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start = i;
        let mut value = String::new();
        let mut quote: Option<char> = None;
        while i < chars.len() {
            let c = chars[i];
            match quote {
                Some(q) if c == q => {
                    quote = None;
                    i += 1;
                }
                Some('"') if c == '\\' => {
                    i += 1;
                    if i >= chars.len() {
                        return None;
                    }
                    let next = chars[i];
                    // Inside double quotes, backslash only escapes these
                    // characters; otherwise it is a literal backslash.
                    if matches!(next, '$' | '`' | '"' | '\\') {
                        value.push(next);
                    } else {
                        value.push('\\');
                        value.push(next);
                    }
                    i += 1;
                }
                Some('"') if c == '$' || c == '`' => return None,
                Some(_) => {
                    value.push(c);
                    i += 1;
                }
                None => {
                    if c.is_whitespace() {
                        break;
                    }
                    match c {
                        '\'' | '"' => {
                            quote = Some(c);
                            i += 1;
                        }
                        '\\' => {
                            i += 1;
                            if i >= chars.len() {
                                return None;
                            }
                            value.push(chars[i]);
                            i += 1;
                        }
                        '$' | ';' | '|' | '&' | '<' | '>' | '(' | ')' | '`' => return None,
                        _ => {
                            value.push(c);
                            i += 1;
                        }
                    }
                }
            }
        }
        if quote.is_some() {
            return None;
        }
        let raw: String = chars[start..i].iter().collect();
        out.push(ShellToken { value, raw });
    }
    Some(out)
}

/// Quote a value for insertion into a shell command. Safe words are left
/// alone; everything else is single-quoted with embedded quotes escaped.
fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '-' | '.' | '/' | ':' | '@' | '%' | '+' | '=' | ',')
        });
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Quote a path operand, preserving `~/` expansion and making leading-dash
/// names explicit paths instead of options.
fn path_quote(s: &str) -> String {
    let s = if s.starts_with('-') {
        format!("./{s}")
    } else {
        s.to_string()
    };
    if s == "~" {
        return s;
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return format!("~/{}", shell_quote(rest));
    }
    shell_quote(&s)
}

fn render_raw(tokens: &[ShellToken]) -> String {
    tokens
        .iter()
        .map(|t| t.raw.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Replace selected token indices with already-quoted values; all other
/// tokens keep their original source spelling.
fn render_with(tokens: &[ShellToken], replacements: &[(usize, String)]) -> String {
    let mut parts: Vec<String> = tokens.iter().map(|t| t.raw.clone()).collect();
    for (idx, value) in replacements {
        if let Some(part) = parts.get_mut(*idx) {
            *part = value.clone();
        }
    }
    parts.join(" ")
}

fn render_swapped(tokens: &[ShellToken]) -> String {
    let mut parts: Vec<String> = tokens.iter().map(|t| t.raw.clone()).collect();
    if parts.len() >= 2 {
        parts.swap(0, 1);
    }
    parts.join(" ")
}

fn option_takes_value(cmd: &str, flag: &str) -> bool {
    match cmd {
        "ls" => matches!(
            flag,
            "-I" | "-w" | "-T" | "--ignore" | "--width" | "--tabsize"
        ),
        "touch" => matches!(flag, "-r" | "-d" | "-t" | "--reference" | "--date"),
        _ => false,
    }
}

/// Indices of non-option operands for the commands that repair missing paths.
/// Values belonging to flags are not operands; `--` ends option parsing.
fn path_operands(toks: &[&str]) -> Vec<usize> {
    let Some(&cmd) = toks.first() else {
        return vec![];
    };
    let mut out = vec![];
    let mut i = 1;
    let mut after_dd = false;
    while i < toks.len() {
        let t = toks[i];
        if !after_dd {
            if t == "--" {
                after_dd = true;
                i += 1;
                continue;
            }
            if t == "-" {
                i += 1;
                continue;
            }
            if t.starts_with('-') && t.len() > 1 {
                let takes_value = match cmd {
                    "touch" => {
                        matches!(t, "-r" | "-d" | "-t" | "--reference" | "--date")
                    }
                    "mkdir" => matches!(t, "-m" | "--mode" | "-Z" | "--context"),
                    "cp" | "mv" => {
                        matches!(t, "-S" | "--suffix" | "-t" | "--target-directory")
                    }
                    _ => false,
                };
                i += if takes_value { 2 } else { 1 };
                continue;
            }
        }
        out.push(i);
        i += 1;
    }
    out
}

/// Index of the first path operand for the path-oriented commands we repair.
/// Options are skipped, including the separate values of the common ones that
/// take them; `--` ends option parsing.
fn path_operand_index(toks: &[&str]) -> Option<usize> {
    let cmd = *toks.first()?;
    let mut i = 1;
    let mut after_dd = false;
    while i < toks.len() {
        let t = toks[i];
        if !after_dd {
            if t == "--" {
                after_dd = true;
                i += 1;
                continue;
            }
            if t == "-" {
                return if matches!(cmd, "cd" | "cat") {
                    None
                } else {
                    Some(i)
                };
            }
            if cmd == "cd" && t.starts_with('-') && !matches!(t, "-L" | "-P" | "-e" | "-@") {
                // A leading-dash directory is more likely a path than a cd option.
                return Some(i);
            }
            if t.starts_with('-') && t.len() > 1 {
                i += if option_takes_value(cmd, t) { 2 } else { 1 };
                continue;
            }
        }
        return Some(i);
    }
    None
}

/// Index of git's subcommand after global options. Returns `None` when the
/// command is incomplete or uses `--` before a subcommand.
fn git_subcommand_index(toks: &[&str]) -> Option<usize> {
    if toks.first().copied() != Some("git") {
        return None;
    }
    let mut i = 1;
    while i < toks.len() {
        let t = toks[i];
        if t == "--" {
            return None;
        }
        if t.starts_with('-') && t.len() > 1 {
            let takes_value = matches!(
                t,
                "-C" | "-c"
                    | "--git-dir"
                    | "--work-tree"
                    | "--namespace"
                    | "--config-env"
                    | "--super-prefix"
            );
            i += if takes_value { 2 } else { 1 };
        } else {
            return Some(i);
        }
    }
    None
}

/// Index of a checkout/switch branch operand, or `None` when the argument is
/// an option value, a path, `--`, or a new-branch form. Conservative by design:
/// a missed repair is better than checking out the wrong thing.
fn git_branch_operand(toks: &[&str], sub_idx: usize, cwd: &str) -> Option<usize> {
    let mut i = sub_idx + 1;
    while i < toks.len() {
        let t = toks[i];
        if t == "--" || t == "-" {
            return None;
        }
        if t.starts_with('-') && t.len() > 1 {
            if matches!(
                t,
                "-b" | "-B" | "-c" | "-C" | "--orphan" | "--create" | "--force-create"
            ) {
                return None;
            }
            i += if matches!(
                t,
                "--conflict" | "--recurse-submodules" | "--pathspec-from-file"
            ) {
                2
            } else {
                1
            };
            continue;
        }
        if t.contains('/')
            || t.contains('.')
            || t.starts_with('~')
            || Path::new(cwd).join(t).exists()
        {
            return None;
        }
        return Some(i);
    }
    None
}

fn is_destructive_candidate(cmd: &str) -> bool {
    if redact::is_destructive(cmd) {
        return true;
    }
    // These names are not in the ghost predicate unless they carry a dangerous
    // flag, but a `fix` suggestion that introduces the command itself is still
    // a destructive rewrite.
    let Some(prog) = cmd.split_whitespace().next() else {
        return false;
    };
    matches!(prog, "rm" | "rmdir" | "shred" | "unlink" | "dd") || prog.starts_with("mkfs")
}

/// Resolve `path` against `cwd`; if a component is missing, walk up to find the
/// deepest existing ancestor and return the missing components from that
/// ancestor down to the leaf, in path order.
fn missing_components(cwd: &str, path: &str) -> Option<(PathBuf, Vec<String>)> {
    let full = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        Path::new(cwd).join(path)
    };
    if full.exists() {
        return None;
    }
    let mut cur = full;
    let mut missing: Vec<String> = vec![];
    while !cur.exists() {
        let name = cur.file_name().and_then(|s| s.to_str())?;
        missing.push(name.to_string());
        let parent = cur.parent()?;
        if parent.as_os_str().is_empty() {
            return None;
        }
        cur = parent.to_path_buf();
    }
    missing.reverse();
    Some((cur, missing))
}

fn flag_index(toks: &[&str]) -> Option<usize> {
    for (i, t) in toks.iter().enumerate().skip(1) {
        if *t == "--" {
            break;
        }
        if *t == "-" || !t.starts_with('-') {
            continue;
        }
        let body = &t[1..];
        if body.chars().all(|c| c.is_ascii_digit() || c == '.') {
            continue; // negative number or numeric sort key
        }
        if body
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '-')
        {
            return Some(i);
        }
    }
    None
}

/// Suggest a fix for a failed command when the shell status is unknown.
///
/// Prefer [`suggest_with_exit`] when the status is available; this wrapper is
/// retained for callers that only have the command text.
#[allow(dead_code)] // public compatibility wrapper; the CLI uses the exit-aware variant
pub fn suggest(cmd: &str, cwd: &str, history: &[String]) -> Vec<String> {
    suggest_with_exit(cmd, cwd, history, None)
}

/// Suggest a fix for a failed command. Filesystem-grounded only (no stderr available).
/// Returns up to 3 candidates, best first. `history` = recent commands for verb/flag learning.
pub fn suggest_with_exit(
    cmd: &str,
    cwd: &str,
    history: &[String],
    exit_code: Option<i64>,
) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    if exit_code == Some(0) {
        return out;
    }
    let Some(tokens) = shell_split(cmd) else {
        return out;
    };
    let toks: Vec<&str> = tokens.iter().map(|t| t.value.as_str()).collect();
    if toks.is_empty() {
        return out;
    }
    let bins = path_bins();
    let unknown_command = exit_code.is_none() || exit_code == Some(127);
    let git_failure = exit_code.is_none() || exit_code == Some(1);
    let flag_failure = exit_code.is_none() || matches!(exit_code, Some(1) | Some(2));
    let sudo_failure = exit_code.is_none() || matches!(exit_code, Some(1) | Some(126));

    // Rule 0: the narrow swapped-program shape `checkout git main`. Do not
    // touch assignments, reserved words, wrappers, or command typos.
    if unknown_command
        && toks.len() >= 2
        && toks[1] == "git"
        && GIT_SUBCOMMANDS.contains(&toks[0])
        && !known_command(toks[0], &bins)
        && known_command(toks[1], &bins)
    {
        out.push(render_swapped(&tokens));
    }

    // Rule 1: unknown command name — typo over PATH + builtins + history verbs.
    if unknown_command
        && !known_command(toks[0], &bins)
        && !is_shell_syntax(toks[0])
        && out.is_empty()
    {
        let verbs = history_verbs(history);
        let preferred: HashSet<&str> = verbs.iter().copied().collect();
        let mut pool: Vec<&str> = bins.iter().map(String::as_str).collect();
        pool.extend_from_slice(SHELL_BUILTINS);
        pool.extend(verbs);
        // Never offer the failed token back as its own correction. History
        // can legitimately contain it (e.g. the current failed command).
        let typed_lower = toks[0].to_lowercase();
        pool.retain(|c| c.to_lowercase() != typed_lower);
        if let Some(hit) = best(toks[0], pool, &preferred) {
            out.push(render_with(&tokens, &[(0, shell_quote(hit))]));
        }
    }

    // Rule 2: git subcommand / branch typo, after git's global options.
    if git_failure
        && toks[0] == "git"
        && let Some(sub_idx) = git_subcommand_index(&toks)
    {
        let no_pref: HashSet<&str> = HashSet::new();
        let sub = toks[sub_idx];
        if !GIT_SUBCOMMANDS.contains(&sub) {
            if let Some(hit) = best(sub, GIT_SUBCOMMANDS.iter().copied(), &no_pref) {
                out.push(render_with(&tokens, &[(sub_idx, shell_quote(hit))]));
            }
        } else if (sub == "checkout" || sub == "switch")
            && let Some(arg_idx) = git_branch_operand(&toks, sub_idx, cwd)
        {
            let branch_arg = toks[arg_idx];
            let branches = git_branches(cwd);
            if !branches.is_empty() && !branches.iter().any(|b| b == branch_arg) {
                let refs: Vec<&str> = branches.iter().map(|s| s.as_str()).collect();
                if let Some(hit) = best(branch_arg, refs, &no_pref) {
                    out.push(render_with(&tokens, &[(arg_idx, shell_quote(hit))]));
                }
            }
        }
    }

    // Rule 3: cd case / path typo at the first real path operand.
    if matches!(toks[0], "cd" | "cat" | "ls" | "touch")
        && let Some(path_idx) = path_operand_index(&toks)
        && let Some((parent, missing)) = missing_components(cwd, toks[path_idx])
        && let Ok(rd) = std::fs::read_dir(&parent)
    {
        let mut siblings: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        siblings.sort();
        let first = &missing[0];
        // Case-only fix first (the `cd downloads` class), then fuzzy.
        let mut repaired: Vec<PathBuf> = siblings
            .iter()
            .find(|s| s.to_lowercase() == first.to_lowercase())
            .map(|exact| vec![parent.join(exact)])
            .unwrap_or_default();
        let refs: Vec<&str> = siblings.iter().map(|s| s.as_str()).collect();
        let no_pref: HashSet<&str> = HashSet::new();
        if let Some(hit) = best(first, refs, &no_pref) {
            repaired.push(parent.join(hit));
        }
        for fixed_root in repaired {
            // Keep every component after the repaired one intact: a partial
            // path must not silently collapse to a sibling.
            let fixed_path = missing
                .iter()
                .skip(1)
                .fold(fixed_root, |path, component| path.join(component));
            if !fixed_path.exists() || (toks[0] == "cd" && !fixed_path.is_dir()) {
                continue;
            }
            let display = pretty_path(cwd, &fixed_path);
            if display.chars().any(|c| c.is_control()) {
                continue;
            }
            out.push(render_with(&tokens, &[(path_idx, path_quote(&display))]));
            break;
        }
    }

    // Rule 4: missing `mkdir -p` — touch/mkdir/cp/mv with missing intermediate dir.
    if matches!(toks[0], "touch" | "mkdir" | "cp" | "mv") && toks.len() >= 2 {
        // Last non-option operand is the path that needs its parent to exist
        // (for cp/mv target likewise). Flags are never candidates.
        let operands = path_operands(&toks);
        if let Some(&target_idx) = operands.last() {
            let target = toks[target_idx];
            let full = if Path::new(target).is_absolute() {
                PathBuf::from(target)
            } else {
                Path::new(cwd).join(target)
            };
            let parent = if toks[0] == "mkdir" {
                full.clone()
            } else {
                full.parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or(full.clone())
            };
            if !parent.exists() {
                let display = pretty_path(cwd, &parent);
                if !display.chars().any(|c| c.is_control()) {
                    out.push(format!(
                        "mkdir -p {} && {}",
                        path_quote(&display),
                        render_raw(&tokens)
                    ));
                }
            }
        }
    }

    // Rule 5: flag typo from history — same program (+subcommand), closest flag spelling.
    if flag_failure && let Some(flag_idx) = flag_index(&toks) {
        // Git's global options come before the subcommand; they are parsed
        // separately from subcommand flags and must not be "repaired" from
        // unrelated history. A global option is never a Rule 5 candidate.
        let git_global_flag = if toks[0] == "git" {
            match git_subcommand_index(&toks) {
                Some(sub_idx) => flag_idx < sub_idx,
                None => true,
            }
        } else {
            false
        };
        // `checkout -b`/`switch -c` create refs; changing one creation flag
        // into another changes semantics, so never repair them from history.
        let git_creation_flag = toks[0] == "git"
            && matches!(
                toks[flag_idx],
                "-b" | "-B" | "-c" | "-C" | "--orphan" | "--create" | "--force-create"
            );
        if !git_global_flag && !git_creation_flag {
            let bad_token = toks[flag_idx];
            let (bad_name, value) = match bad_token.split_once('=') {
                Some((name, value)) if name.starts_with("--") => (name, Some(value)),
                _ => (bad_token, None),
            };
            let prefix: Vec<&str> = toks[..flag_idx].to_vec();
            let mut flag_pool: Vec<String> = vec![];
            for h in history {
                let Some(htoks) = shell_split(h) else {
                    continue;
                };
                let ht: Vec<&str> = htoks.iter().map(|t| t.value.as_str()).collect();
                if ht.len() > prefix.len() && ht[..prefix.len()] == prefix[..] {
                    for t in &ht[prefix.len()..] {
                        if *t == "--" {
                            break;
                        }
                        if t.starts_with('-') {
                            let name = t.split('=').next().unwrap_or(t);
                            if name != bad_name && !flag_pool.iter().any(|p| p == name) {
                                flag_pool.push(name.to_string());
                            }
                        }
                    }
                }
            }
            let refs: Vec<&str> = flag_pool.iter().map(String::as_str).collect();
            if let Some(hit) = best(bad_name, refs, &HashSet::new()) {
                let replacement = match value {
                    Some(value) => format!("{hit}={value}"),
                    None => hit.to_string(),
                };
                out.push(render_with(
                    &tokens,
                    &[(flag_idx, shell_quote(&replacement))],
                ));
            }
        }
    }

    // Rule 6 (heuristic, ranked last): missing sudo for root-needing prefixes on failure.
    // No stderr available, so this only fires for a narrow allowlist and is always last.
    const SUDO_PREFIXES: &[&str] = &[
        "apt install",
        "apt-get install",
        "systemctl",
        "mkdir /etc/",
        "mkdir /opt/",
        "tee /etc/",
        "tee /sys/",
    ];
    if sudo_failure && SUDO_PREFIXES.iter().any(|p| cmd.starts_with(p)) {
        out.push(format!("sudo {}", render_raw(&tokens)));
    }

    // Dedupe, cap 3. Destructive candidates are suppressed, not merely ranked
    // last: the UI advertises an Enter-to-run action.
    let mut seen = HashSet::new();
    out.into_iter()
        .filter(|c| !is_destructive_candidate(c))
        .filter(|c| seen.insert(c.clone()))
        .take(3)
        .collect()
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
    fn path_typo_preserves_multi_component_suffix() {
        let dir = std::env::temp_dir().join(format!("gl-fixsuffix-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Documents/sub")).unwrap();
        std::fs::create_dir_all(dir.join("Downloads/sub")).unwrap();
        std::fs::create_dir_all(dir.join("sun")).unwrap();
        let cwd = dir.to_string_lossy().into_owned();

        let out = suggest("cd Documnets/sub", &cwd, &[]);
        assert!(out.iter().any(|s| s == "cd Documents/sub"), "got: {out:?}");

        let out = suggest("cd downloads/sub", &cwd, &[]);
        assert!(out.iter().any(|s| s == "cd Downloads/sub"), "got: {out:?}");
        assert!(!out.iter().any(|s| s == "cd sun"), "truncated path: {out:?}");

        let out = suggest("cd downloads/missing", &cwd, &[]);
        assert!(!out.iter().any(|s| s == "cd Downloads"), "truncated path: {out:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn failed_self_history_is_not_a_candidate() {
        let history = vec!["cdd".to_string(), "cd".to_string()];
        let out = suggest("cdd", "/tmp", &history);
        assert!(out.iter().any(|s| s == "cd"), "got: {out:?}");
        assert!(!out.iter().any(|s| s == "cdd"), "self suggestion: {out:?}");
    }

    #[test]
    fn failed_flag_in_history_is_not_a_candidate() {
        let history = vec!["echo --colr".to_string(), "echo --color".to_string()];
        let out = suggest("echo --colr", "/tmp", &history);
        assert!(out.iter().any(|s| s == "echo --color"), "got: {out:?}");
        assert!(!out.iter().any(|s| s == "echo --colr"), "self suggestion: {out:?}");
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
    fn shell_split_preserves_quoted_values_and_rejects_operators() {
        let toks = shell_split("cd \"My Docz\"").unwrap();
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[1].value, "My Docz");
        assert_eq!(toks[1].raw, "\"My Docz\"");
        let toks = shell_split("echo \"a\\qb\"").unwrap();
        assert_eq!(toks[1].value, "a\\qb");
        assert!(shell_split("ls | head").is_none());
        assert!(shell_split("echo $(rm x)").is_none());
        assert!(shell_split("cd \"unterminated").is_none());
    }

    #[test]
    fn path_operand_skips_options() {
        assert_eq!(path_operand_index(&["ls", "-la", "downloads"]), Some(2));
        assert_eq!(
            path_operand_index(&["ls", "--color=auto", "downloads"]),
            Some(2)
        );
        assert_eq!(path_operand_index(&["cat", "-n", "file"]), Some(2));
        assert_eq!(path_operand_index(&["cd", "-P", "downloads"]), Some(2));
        assert_eq!(path_operand_index(&["ls", "--", "-la"]), Some(2));
        assert_eq!(path_operand_index(&["cd", "-L"]), None);
        assert_eq!(path_operands(&["touch", "-m", "file"]), vec![2]);
        assert_eq!(path_operands(&["touch", "file", "-m"]), vec![1]);
        assert_eq!(path_operands(&["touch", "-d", "date", "file"]), vec![3]);
        assert_eq!(path_operands(&["mkdir", "-p", "dir"]), vec![2]);
        assert_eq!(path_operands(&["touch", "--", "-file"]), vec![2]);
    }

    #[test]
    fn git_global_options_and_branch_guards() {
        assert_eq!(
            git_subcommand_index(&["git", "-C", "/tmp", "status"]),
            Some(3)
        );
        assert_eq!(
            git_subcommand_index(&["git", "-c", "core.pager=cat", "status"]),
            Some(3)
        );
        assert_eq!(
            git_subcommand_index(&["git", "--no-pager", "status"]),
            Some(2)
        );
        assert_eq!(git_subcommand_index(&["git", "--", "status"]), None);
        assert_eq!(
            git_branch_operand(&["git", "checkout", "-b", "newbranch"], 1, "/tmp"),
            None
        );
        assert_eq!(
            git_branch_operand(&["git", "checkout", "--", "mian"], 1, "/tmp"),
            None
        );
        assert_eq!(
            git_branch_operand(&["git", "checkout", "main.c"], 1, "/tmp"),
            None
        );
        assert_eq!(
            git_branch_operand(&["git", "checkout", "-q", "mian"], 1, "/tmp"),
            Some(3)
        );
    }

    #[test]
    fn shell_syntax_is_not_swapped_or_corrected() {
        for cmd in [
            "time git status",
            "FOO=1 git status",
            "! git status",
            "builtin rm file",
        ] {
            let out = suggest(cmd, "/tmp", &[]);
            assert!(out.is_empty(), "{cmd}: {out:?}");
        }
        let out = suggest("gti status", "/tmp", &[]);
        assert!(out.iter().any(|s| s == "git status"), "got: {out:?}");
    }

    #[test]
    fn quoted_paths_and_flags_are_repaired_safely() {
        let dir = std::env::temp_dir().join(format!("gl-fixquoted-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("My Docs")).unwrap();
        std::fs::create_dir_all(dir.join("Downloads")).unwrap();
        let cwd = dir.to_string_lossy().into_owned();

        let out = suggest("cd \"My Docz\"", &cwd, &[]);
        assert!(out.iter().any(|s| s == "cd 'My Docs'"), "got: {out:?}");

        let out = suggest("ls -la downloads", &cwd, &[]);
        assert!(out.iter().any(|s| s == "ls -la Downloads"), "got: {out:?}");

        let out = suggest("touch \"My Dir/file\"", &cwd, &[]);
        assert!(
            out.iter()
                .any(|s| s == "mkdir -p 'My Dir' && touch \"My Dir/file\""),
            "got: {out:?}"
        );

        let out = suggest("touch -m \"My Dir/file\"", &cwd, &[]);
        assert!(
            out.iter()
                .any(|s| s == "mkdir -p 'My Dir' && touch -m \"My Dir/file\""),
            "got: {out:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn leading_dash_path_is_rendered_as_a_path() {
        let dir = std::env::temp_dir().join(format!("gl-fixdash-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("-Downloads")).unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        let out = suggest("cd -downloads", &cwd, &[]);
        assert!(out.iter().any(|s| s == "cd ./-Downloads"), "got: {out:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn git_global_options_do_not_produce_false_fixes() {
        let out = suggest("git -C /tmp status", "/tmp", &[]);
        assert!(out.is_empty(), "got: {out:?}");
        let out = suggest("git -c core.pager=cat status", "/tmp", &[]);
        assert!(out.is_empty(), "got: {out:?}");
        let history = vec!["git -c core.pager=cat status".to_string()];
        let out = suggest("git -C /tmp status", "/tmp", &history);
        assert!(out.is_empty(), "global flag must not be repaired: {out:?}");
        let history = vec!["git checkout -B newbranch".to_string()];
        let out = suggest("git checkout -b newbranch", "/tmp", &history);
        assert!(
            out.is_empty(),
            "creation flag must not be repaired: {out:?}"
        );
        let out = suggest("git -C /tmp chekout", "/tmp", &[]);
        assert!(
            out.iter().any(|s| s == "git -C /tmp checkout"),
            "got: {out:?}"
        );
    }

    #[test]
    fn successful_exit_has_no_suggestions() {
        let out = suggest_with_exit("ls -la downloads", "/tmp", &[], Some(0));
        assert!(out.is_empty(), "got: {out:?}");
    }

    #[test]
    fn destructive_candidates_are_suppressed() {
        assert!(suggest("rmm", "/tmp", &[]).is_empty());
        assert!(suggest("gti push --force", "/tmp", &[]).is_empty());
        let history = vec!["git clean -fdx".to_string()];
        assert!(suggest("git clean -fd", "/tmp", &history).is_empty());
        assert!(suggest("chmdo -R 777 /", "/tmp", &[]).is_empty());
    }

    #[test]
    fn flag_repair_is_anchored_and_stops_at_double_dash() {
        let history = vec!["ls x--colr --color".to_string()];
        let out = suggest("ls x--colr --colr", "/tmp", &history);
        assert!(
            out.iter().any(|s| s == "ls x--colr --color"),
            "got: {out:?}"
        );

        let out = suggest("ls -- --colr", "/tmp", &history);
        assert!(out.is_empty(), "got: {out:?}");

        let out = suggest("sort -10 file", "/tmp", &history);
        assert!(out.is_empty(), "got: {out:?}");

        let history = vec!["ls --color=auto".to_string()];
        let out = suggest("ls --colr=auto", "/tmp", &history);
        assert!(out.iter().any(|s| s == "ls --color=auto"), "got: {out:?}");
    }

    #[test]
    fn no_absurd_suggestions() {
        let out = suggest("rm -rf / tmp/foo", "/tmp", &[]);
        assert!(!out.iter().any(|s| s.contains("rm")), "got: {out:?}");
        let out2 = suggest("ls", "/tmp", &[]);
        assert!(out2.is_empty(), "got: {out2:?}");
    }
}
