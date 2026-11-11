use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::redact;
use crate::shell;

/// Longest single word we will fuzzy-match. Command and path names are far
/// shorter; the cap keeps a pathological token from allocating a huge matrix.
const MAX_WORD_LEN: usize = 200;

/// Optimal string alignment distance (Damerau-Levenshtein restricted to
/// adjacent transpositions). Case-insensitive: caller lowercases first;
/// exact-but-for-case matches are handled as distance 0 by callers.
///
/// Uses three rolling rows: the transposition step only needs `d[i-2]`, so
/// memory is O(m) rather than O(n*m).
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
    let mut prev2 = vec![0usize; m + 1];
    let mut prev: Vec<usize> = (0..=m).collect();
    for i in 1..=n {
        let mut cur = vec![0usize; m + 1];
        cur[0] = i;
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                cur[j] = cur[j].min(prev2[j - 2] + 1);
            }
        }
        prev2 = std::mem::replace(&mut prev, cur);
    }
    prev[m]
}

/// Git-style proportional threshold: reject if distance > max(2, len/3).
/// Counts characters, not bytes, so non-ASCII words are not treated as
/// longer than they are.
pub fn within_threshold(typed: &str, dist: usize) -> bool {
    dist <= 2usize.max(typed.chars().count() / 3)
}

/// Adjacent-transposition check: same length, exactly one swapped adjacent pair.
/// Used to prefer `gti→git` (transposition, the common typo class) over `gti→gtf`.
fn is_transposition(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len() != b.len() {
        return false;
    }
    let mut first = None;
    let mut second = None;
    let mut diffs = 0;
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        if x != y {
            diffs += 1;
            match diffs {
                1 => first = Some(i),
                2 => second = Some(i),
                _ => return false,
            }
        }
    }
    match (first, second) {
        (Some(i), Some(j)) => j == i + 1 && a[i] == b[j] && a[j] == b[i],
        _ => false,
    }
}

/// Best candidate within threshold, case-insensitive.
/// Tie-breaks: exact case-fold → history membership → transposition → shorter
/// distance → lexicographic name. The final name tie-break makes output stable
/// regardless of the order in which the candidate pool was collected.
pub fn best<'a>(
    typed: &str,
    candidates: impl IntoIterator<Item = &'a str>,
    preferred: &HashSet<&str>,
) -> Option<&'a str> {
    let lower = typed.to_lowercase();
    let typed_len = lower.chars().count();
    if typed_len == 0 || typed_len > MAX_WORD_LEN {
        return None;
    }
    let threshold = 2usize.max(typed_len / 3);
    let mut best: Option<(&'a str, String, bool, bool, bool, usize)> = None;
    for c in candidates {
        if c.is_empty() {
            continue;
        }
        let cl = c.to_lowercase();
        let cand_len = cl.chars().count();
        // Cheap prefilter before the OSA matrix: a length difference larger
        // than the threshold can never be within it.
        if cand_len > MAX_WORD_LEN || typed_len.abs_diff(cand_len) > threshold {
            continue;
        }
        let d = osa(&lower, &cl);
        if !within_threshold(&lower, d) {
            continue;
        }
        let key = (
            cl == lower,
            preferred.contains(c),
            is_transposition(&lower, &cl),
        );
        let take = match &best {
            None => true,
            Some((_, best_name, bex, bpref, btrans, bd)) => {
                if key != (*bex, *bpref, *btrans) {
                    key > (*bex, *bpref, *btrans)
                } else if d != *bd {
                    d < *bd
                } else {
                    cl.as_str() < best_name.as_str()
                }
            }
        };
        if take {
            best = Some((c, cl, key.0, key.1, key.2, d));
        }
    }
    best.map(|(c, _, _, _, _, _)| c)
}

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
    shell::is_reserved(word) || is_assignment(word)
}

/// Verbs seen in history (first tokens), for command-name correction.
pub fn history_verbs(history: &[String]) -> Vec<&str> {
    let mut seen = HashSet::new();
    let mut out = vec![];
    for h in history {
        if let Some(v) = h.split_whitespace().next()
            && !is_shell_syntax(v)
            && is_displayable(v)
            && seen.insert(v)
        {
            out.push(v);
        }
    }
    out
}

/// Is `prog` runnable (PATH lookup, builtin, or executable path)? Used to
/// decide if token[0] is a typo. Relative paths resolve against `cwd`, not the
/// process working directory, and directories/non-executables are rejected.
pub fn known_command(prog: &str, cwd: &str, path_bins: &BTreeSet<String>) -> bool {
    if prog.contains('/') {
        let path = Path::new(prog);
        let full = if path.is_absolute() {
            path.to_path_buf()
        } else {
            Path::new(cwd).join(path)
        };
        return shell::is_executable(&full);
    }
    shell::is_builtin(prog) || path_bins.contains(prog)
}

const GIT_SUBCOMMANDS: &[&str] = &[
    "add",
    "bisect",
    "branch",
    "checkout",
    "clone",
    "commit",
    "diff",
    "fetch",
    "grep",
    "init",
    "log",
    "merge",
    "mv",
    "pull",
    "push",
    "rebase",
    "reset",
    "restore",
    "rm",
    "show",
    "stash",
    "status",
    "switch",
    "tag",
    "worktree",
    "config",
    "remote",
    "reflog",
    "clean",
    "cherry-pick",
    "describe",
    "apply",
    "am",
    "blame",
    "shortlog",
    "ls-files",
    "rev-parse",
    "submodule",
    "gc",
];

/// Keep `fix` responsive even when git is slow or the repository is huge.
const GIT_TIMEOUT: Duration = Duration::from_millis(250);

/// Run a command with a hard timeout. `std` has no built-in timeout, so poll
/// `try_wait` and kill the child when the deadline passes.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<Output, String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().map_err(|e| format!("wait: {e}")),
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}ms", timeout.as_millis()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait: {e}"));
            }
        }
    }
}

/// Branches, tags (checkout only), and remote-tracking refs in `cwd`'s repo.
/// Returns an error for a missing git or a broken repository instead of
/// silently pretending there are no refs.
fn git_refs(cwd: &str, include_tags_and_remotes: bool) -> Result<Vec<String>, String> {
    let mut cmd = Command::new("git");
    cmd.args(["-C", cwd, "for-each-ref", "--format=%(refname:short)"]);
    cmd.arg("refs/heads");
    if include_tags_and_remotes {
        cmd.arg("refs/tags");
        cmd.arg("refs/remotes");
    }
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    let out = run_with_timeout(cmd, GIT_TIMEOUT)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git for-each-ref failed: {}", err.trim()));
    }
    let mut refs: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.ends_with("/HEAD"))
        .map(str::to_owned)
        .collect();
    refs.sort();
    refs.dedup();
    Ok(refs)
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

/// Options that consume a separate value token. Knowing these prevents a
/// value (which may itself start with `-`) from being diagnosed as a flag or
/// treated as a path operand.
fn option_takes_value(cmd: &str, flag: &str) -> bool {
    match cmd {
        "ls" => matches!(
            flag,
            "-I" | "-w"
                | "-T"
                | "--ignore"
                | "--width"
                | "--tabsize"
                | "--block-size"
                | "--format"
                | "--time-style"
                | "--quoting-style"
                | "--indicator-style"
                | "--hide"
                | "--sort"
        ),
        "touch" => matches!(flag, "-r" | "-d" | "-t" | "--reference" | "--date"),
        "mkdir" => matches!(flag, "-m" | "--mode" | "-Z" | "--context"),
        "cp" | "mv" => matches!(flag, "-S" | "--suffix" | "-t" | "--target-directory"),
        "grep" => matches!(
            flag,
            "-e" | "-f"
                | "-m"
                | "-A"
                | "-B"
                | "-C"
                | "-d"
                | "-D"
                | "--regexp"
                | "--file"
                | "--max-count"
                | "--after-context"
                | "--before-context"
                | "--context"
                | "--directories"
                | "--devices"
                | "--include"
                | "--exclude"
                | "--exclude-dir"
                | "--label"
                | "--binary-files"
        ),
        "sed" => matches!(
            flag,
            "-e" | "-f" | "-l" | "--expression" | "--file" | "--line-length"
        ),
        "awk" => matches!(flag, "-v" | "-F" | "-f"),
        "sort" => matches!(
            flag,
            "-k" | "-t"
                | "-o"
                | "-S"
                | "-T"
                | "--key"
                | "--field-separator"
                | "--output"
                | "--buffer-size"
                | "--temporary-directory"
                | "--parallel"
                | "--batch-size"
                | "--files0-from"
        ),
        "head" | "tail" => matches!(flag, "-n" | "-c" | "--lines" | "--bytes"),
        "cut" => matches!(
            flag,
            "-b" | "-c" | "-d" | "-f" | "--bytes" | "--characters" | "--delimiter" | "--fields"
        ),
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
                i += if option_takes_value(cmd, t) { 2 } else { 1 };
                continue;
            }
        }
        out.push(i);
        i += 1;
    }
    out
}

/// Destination of `cp`/`mv`: `(token index, embedded value, is_explicit_dir)`.
/// Covers `-t DIR`, `--target-directory DIR`, `--target-directory=DIR`, and
/// `-tDIR`. Without an explicit target directory the last operand is the
/// destination.
fn destination_operand<'a>(toks: &[&'a str]) -> Option<(usize, Option<&'a str>, bool)> {
    let cmd = toks.first().copied().unwrap_or("");
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
            if t == "-t" || t == "--target-directory" {
                return toks.get(i + 1).map(|_| (i + 1, None, true));
            }
            if let Some(value) = t.strip_prefix("--target-directory=") {
                return Some((i, Some(value), true));
            }
            if let Some(value) = t.strip_prefix("-t").filter(|v| !v.is_empty()) {
                return Some((i, Some(value), true));
            }
            // A value for an earlier option is not a target option
            // (`cp -S -t file`: `-t` is the suffix).
            if option_takes_value(cmd, t) {
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    path_operands(toks)
        .last()
        .copied()
        .map(|idx| (idx, None, false))
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
        if t.starts_with('~') || Path::new(cwd).join(t).exists() {
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

/// Resolve `path` against `cwd`, expanding `~` and `~/…` against `home`.
/// Other `$…` forms are deliberately not expanded; the lexer rejects unquoted
/// substitutions, and a quoted `$HOME` is literal shell text.
fn resolve_path(cwd: &str, home: Option<&Path>, path: &str) -> Option<PathBuf> {
    if path == "~" {
        return home.map(Path::to_path_buf);
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.map(|h| h.join(rest));
    }
    let p = Path::new(path);
    Some(if p.is_absolute() {
        p.to_path_buf()
    } else {
        Path::new(cwd).join(p)
    })
}

/// Resolve `path` against `cwd`; if a component is missing, walk up to find the
/// deepest existing ancestor and return the missing components from that
/// ancestor down to the leaf, in path order.
fn missing_components(
    cwd: &str,
    home: Option<&Path>,
    path: &str,
) -> Option<(PathBuf, Vec<String>)> {
    let full = resolve_path(cwd, home, path)?;
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

/// Path-kind guard for repairs: `cd` needs a directory, `cat` a file, while
/// `ls`/`touch` accept any existing path.
fn path_fits_command(cmd: &str, path: &Path) -> bool {
    match cmd {
        "cd" => path.is_dir(),
        "cat" => path.is_file(),
        _ => path.exists(),
    }
}

/// Index of the first token that plausibly is an option. Stops at `--`, skips
/// negative numbers, and skips separate values of options known to take one
/// (so `grep -e -foo file` does not diagnose `-foo`).
fn flag_index(toks: &[&str]) -> Option<usize> {
    let cmd = toks.first().copied().unwrap_or("");
    let mut i = 1;
    while i < toks.len() {
        let t = toks[i];
        if t == "--" {
            break;
        }
        if t == "-" || !t.starts_with('-') {
            i += 1;
            continue;
        }
        if option_takes_value(cmd, t) {
            i += 2;
            continue;
        }
        let body = &t[1..];
        if body.chars().all(|c| c.is_ascii_digit() || c == '.') {
            i += 1;
            continue; // negative number or numeric sort key
        }
        if body
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '-')
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// True when a token could be a command name that needs a `PATH` lookup.
/// Builtins, shell syntax, and explicit paths never do.
fn needs_path_lookup(word: &str) -> bool {
    !word.contains('/') && !shell::is_builtin(word) && !is_shell_syntax(word)
}

/// True when `flag` appears as a short option, including clusters like `-pv`.
fn has_short_flag(toks: &[&str], short: char) -> bool {
    toks.iter()
        .take_while(|t| **t != "--")
        .any(|t| t.starts_with('-') && !t.starts_with("--") && t[1..].chars().any(|c| c == short))
}

fn is_displayable(s: &str) -> bool {
    !s.chars().any(char::is_control)
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
    let home = std::env::var_os("HOME").map(PathBuf::from);
    // An unknown EUID is treated as root: never suggest sudo unless we can
    // prove the caller is unprivileged.
    let euid = effective_uid().unwrap_or(0);
    suggest_with_context(cmd, cwd, history, exit_code, None, home.as_deref(), euid)
}

/// Inner entry point with injectable command pool, home, and EUID so rules can
/// be tested without mutating process-wide environment.
fn suggest_with_context(
    cmd: &str,
    cwd: &str,
    history: &[String],
    exit_code: Option<i64>,
    bins: Option<&BTreeSet<String>>,
    home: Option<&Path>,
    euid: u32,
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
    // At most one PATH scan per call, and only when the first token actually
    // needs PATH/builtin knowledge.
    let empty_bins = BTreeSet::new();
    let owned_bins: BTreeSet<String>;
    let bins: &BTreeSet<String> = match bins {
        Some(bins) => bins,
        None if needs_path_lookup(toks[0]) => {
            owned_bins = shell::path_bins();
            &owned_bins
        }
        None => &empty_bins,
    };
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
        && !known_command(toks[0], cwd, bins)
        && known_command(toks[1], cwd, bins)
    {
        out.push(render_swapped(&tokens));
    }

    // Rule 1: unknown command name — typo over PATH + builtins + history verbs.
    if unknown_command
        && !known_command(toks[0], cwd, bins)
        && !is_shell_syntax(toks[0])
        && out.is_empty()
    {
        let verbs = history_verbs(history);
        let preferred: HashSet<&str> = verbs.iter().copied().collect();
        let mut pool: Vec<&str> = bins.iter().map(String::as_str).collect();
        pool.extend_from_slice(shell::BUILTINS);
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
            // `checkout` accepts tags and remote refs; `switch` only accepts
            // local branches (remote refs need an explicit --detach).
            match git_refs(cwd, sub == "checkout") {
                Ok(refs)
                    if !refs.is_empty()
                        && !refs.iter().any(|candidate| candidate == branch_arg) =>
                {
                    let refs: Vec<&str> = refs.iter().map(String::as_str).collect();
                    if let Some(hit) = best(branch_arg, refs, &no_pref) {
                        out.push(render_with(&tokens, &[(arg_idx, shell_quote(hit))]));
                    }
                }
                Ok(_) => {}
                Err(e) => eprintln!("ghostline fix: warning: git refs unavailable: {e}"),
            }
        }
    }

    // Rule 3: cd case / path typo at the first real path operand.
    if matches!(toks[0], "cd" | "cat" | "ls" | "touch")
        && let Some(path_idx) = path_operand_index(&toks)
        && let Some((parent, missing)) = missing_components(cwd, home, toks[path_idx])
        && let Ok(rd) = std::fs::read_dir(&parent)
    {
        let mut siblings: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        siblings.sort();
        // Filter by the command's expected path kind before ranking, so a
        // regular file named like a directory cannot shadow the real one.
        siblings.retain(|s| path_fits_command(toks[0], &parent.join(s)));
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
            if !path_fits_command(toks[0], &fixed_path) {
                continue;
            }
            let display = pretty_path(cwd, home, &fixed_path);
            if !is_displayable(&display) {
                continue;
            }
            out.push(render_with(&tokens, &[(path_idx, path_quote(&display))]));
            break;
        }
    }

    // Rule 4: missing `mkdir -p` — touch/mkdir/cp/mv with a missing target
    // directory. The candidate must create what the original command needs,
    // not run the original a second time.
    if matches!(toks[0], "touch" | "mkdir" | "cp" | "mv") && toks.len() >= 2 {
        match toks[0] {
            "mkdir" => {
                let has_parents = has_short_flag(&toks, 'p') || toks.contains(&"--parents");
                if !has_parents
                    && path_operands(&toks)
                        .iter()
                        .any(|&i| resolve_path(cwd, home, toks[i]).is_some_and(|p| !p.exists()))
                {
                    out.push(format!("mkdir -p {}", render_raw(&tokens[1..])));
                }
            }
            "touch" => {
                if let Some(&target_idx) = path_operands(&toks).last()
                    && let Some(full) = resolve_path(cwd, home, toks[target_idx])
                    && let Some(parent) = full.parent()
                    && !parent.exists()
                {
                    let display = pretty_path(cwd, home, parent);
                    if is_displayable(&display) {
                        out.push(format!(
                            "mkdir -p {} && {}",
                            path_quote(&display),
                            render_raw(&tokens)
                        ));
                    }
                }
            }
            _ => {
                // cp/mv: `-t` wins; otherwise the last operand is the target.
                let operands = path_operands(&toks);
                if let Some((dest_idx, embedded, explicit)) = destination_operand(&toks) {
                    let target = embedded.unwrap_or(toks[dest_idx]).trim_end_matches('/');
                    if !target.is_empty()
                        && let Some(full) = resolve_path(cwd, home, target)
                    {
                        let source_count = if explicit {
                            operands.len()
                        } else {
                            operands.len().saturating_sub(1)
                        };
                        let target_is_dir =
                            explicit || source_count > 1 || toks[dest_idx].ends_with('/');
                        let dir = if target_is_dir {
                            full
                        } else {
                            full.parent().map(Path::to_path_buf).unwrap_or(full)
                        };
                        if !dir.exists() {
                            let display = pretty_path(cwd, home, &dir);
                            if is_displayable(&display) {
                                out.push(format!(
                                    "mkdir -p {} && {}",
                                    path_quote(&display),
                                    render_raw(&tokens)
                                ));
                            }
                        }
                    }
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
                    let mut j = prefix.len();
                    while j < ht.len() {
                        let t = ht[j];
                        if t == "--" {
                            break;
                        }
                        if let Some(body) = t.strip_prefix('-') {
                            // A value belonging to an earlier option is not a
                            // flag (`grep -e -foo`).
                            if option_takes_value(toks[0], t) {
                                j += 2;
                                continue;
                            }
                            if !body.chars().all(|c| c.is_ascii_digit() || c == '.') {
                                let name = t.split('=').next().unwrap_or(t);
                                if name != bad_name && !flag_pool.iter().any(|p| p == name) {
                                    flag_pool.push(name.to_string());
                                }
                            }
                        }
                        j += 1;
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

    // Rule 6 (heuristic, ranked last): missing sudo for root-needing commands.
    // Token-aware, checks EUID and that a privilege tool exists. A wrong sudo
    // suggestion is worse than none, so every uncertainty suppresses it.
    if sudo_failure && let Some(candidate) = sudo_candidate(&tokens, &toks, cwd, bins, euid) {
        out.push(candidate);
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

/// Linux effective UID. `None` means unknown, which callers treat as root
/// (i.e. no sudo suggestion).
fn effective_uid() -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let uid_line = status.lines().find(|line| line.starts_with("Uid:"))?;
        uid_line.split_whitespace().nth(2)?.parse().ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// True when `path` resolves inside `root`. Component-based, so `/etcetera`
/// is not treated as `/etc`.
fn path_under(cwd: &str, path: &str, root: &str) -> bool {
    resolve_path(cwd, None, path).is_some_and(|p| p.starts_with(root))
}

/// Paths whose creation normally requires root.
fn is_protected_path(cwd: &str, path: &str) -> bool {
    ["/etc", "/opt", "/usr", "/var"]
        .iter()
        .any(|root| path_under(cwd, path, root))
}

/// Narrow, token-aware root-need allowlist. Package installs and systemd
/// control need root; `mkdir`/`tee` do only when their operand is a missing
/// system path (`tee` may target an existing protected file on EACCES).
fn command_needs_root(toks: &[&str], cwd: &str) -> bool {
    match toks[0] {
        "systemctl" => true,
        "apt" | "apt-get" => toks.get(1) == Some(&"install"),
        "mkdir" => path_operands(toks).iter().any(|&i| {
            is_protected_path(cwd, toks[i])
                && resolve_path(cwd, None, toks[i]).is_some_and(|p| !p.exists())
        }),
        "tee" => path_operands(toks)
            .iter()
            .any(|&i| is_protected_path(cwd, toks[i]) || path_under(cwd, toks[i], "/sys")),
        _ => false,
    }
}

fn sudo_candidate(
    tokens: &[ShellToken],
    toks: &[&str],
    cwd: &str,
    bins: &BTreeSet<String>,
    euid: u32,
) -> Option<String> {
    if euid == 0 || !known_command(toks[0], cwd, bins) || !command_needs_root(toks, cwd) {
        return None;
    }
    let tool = if bins.contains("sudo") {
        "sudo"
    } else if bins.contains("doas") {
        "doas"
    } else {
        return None;
    };
    let body = if toks[0] == "mkdir" && !has_short_flag(toks, 'p') && !toks.contains(&"--parents") {
        format!("mkdir -p {}", render_raw(&tokens[1..]))
    } else {
        render_raw(tokens)
    };
    Some(format!("{tool} {body}"))
}

fn pretty_path(cwd: &str, home: Option<&Path>, p: &Path) -> String {
    if let Ok(rel) = p.strip_prefix(cwd)
        && !rel.as_os_str().is_empty()
    {
        return rel.to_string_lossy().into_owned();
    }
    if let Some(home) = home
        && let Ok(rel) = p.strip_prefix(home)
        && !rel.as_os_str().is_empty()
    {
        return format!("~/{}", rel.to_string_lossy());
    }
    p.to_string_lossy().into_owned()
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
        let dir = tmp_dir("swap");
        let cwd = dir.to_string_lossy().into_owned();
        let out = suggest("checkout git main", &cwd, &[]);
        assert!(out.iter().any(|s| s == "git checkout main"), "got: {out:?}");
    }

    #[test]
    fn cd_case_fixed() {
        let dir = tmp_dir("fixtest");
        std::fs::create_dir_all(dir.join("Downloads")).unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        let out = suggest("cd downloads", &cwd, &[]);
        assert!(out.iter().any(|s| s == "cd Downloads"), "got: {out:?}");
    }

    #[test]
    fn path_typo_fixed() {
        let dir = tmp_dir("fixtypo");
        std::fs::create_dir_all(dir.join("Documents")).unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        let out = suggest("cd Documnets", &cwd, &[]);
        assert!(out.iter().any(|s| s == "cd Documents"), "got: {out:?}");
    }

    #[test]
    fn path_typo_preserves_multi_component_suffix() {
        let dir = tmp_dir("fixsuffix");
        std::fs::create_dir_all(dir.join("Documents/sub")).unwrap();
        std::fs::create_dir_all(dir.join("Downloads/sub")).unwrap();
        std::fs::create_dir_all(dir.join("sun")).unwrap();
        let cwd = dir.to_string_lossy().into_owned();

        let out = suggest("cd Documnets/sub", &cwd, &[]);
        assert!(out.iter().any(|s| s == "cd Documents/sub"), "got: {out:?}");

        let out = suggest("cd downloads/sub", &cwd, &[]);
        assert!(out.iter().any(|s| s == "cd Downloads/sub"), "got: {out:?}");
        assert!(
            !out.iter().any(|s| s == "cd sun"),
            "truncated path: {out:?}"
        );

        let out = suggest("cd downloads/missing", &cwd, &[]);
        assert!(
            !out.iter().any(|s| s == "cd Downloads"),
            "truncated path: {out:?}"
        );
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
        assert!(
            !out.iter().any(|s| s == "echo --colr"),
            "self suggestion: {out:?}"
        );
    }

    #[test]
    fn git_subcommand_fixed() {
        let dir = tmp_dir("gitsub");
        let cwd = dir.to_string_lossy().into_owned();
        let out = suggest("git chekout main", &cwd, &[]);
        assert!(out.iter().any(|s| s == "git checkout main"), "got: {out:?}");
    }

    #[test]
    fn mkdir_p_suggested() {
        let dir = tmp_dir("fixmkdir");
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        let out = suggest("touch a/b/c.txt", &cwd, &[]);
        assert!(
            out.iter()
                .any(|s| s.starts_with("mkdir -p") && s.contains("touch a/b/c.txt")),
            "got: {out:?}"
        );
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
        let path_dir = tmp_dir("gitpath");
        std::fs::write(path_dir.join("main.c"), "x").unwrap();
        let path_cwd = path_dir.to_string_lossy().into_owned();
        assert_eq!(
            git_branch_operand(&["git", "checkout", "main.c"], 1, &path_cwd),
            None
        );
        // A ref name may contain dots (tags commonly do) or slashes (remote
        // refs); only an existing path is rejected.
        assert_eq!(
            git_branch_operand(&["git", "checkout", "v1.1"], 1, &path_cwd),
            Some(2)
        );
        assert_eq!(
            git_branch_operand(&["git", "checkout", "origin/mian"], 1, &path_cwd),
            Some(2)
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
        let dir = tmp_dir("fixquoted");
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
    }

    #[test]
    fn leading_dash_path_is_rendered_as_a_path() {
        let dir = tmp_dir("fixdash");
        std::fs::create_dir_all(dir.join("-Downloads")).unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        let out = suggest("cd -downloads", &cwd, &[]);
        assert!(out.iter().any(|s| s == "cd ./-Downloads"), "got: {out:?}");
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

    fn git_ok(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn git_refs_cover_branches_tags_and_remotes() {
        if !git_available() {
            eprintln!("SKIP git_refs_cover_branches_tags_and_remotes: no git");
            return;
        }
        let dir = tmp_dir("gitrefs");
        if !git_ok(&dir, &["init", "-q"])
            || !git_ok(&dir, &["symbolic-ref", "HEAD", "refs/heads/main"])
            || !git_ok(&dir, &["commit", "--allow-empty", "-qm", "init"])
        {
            eprintln!("SKIP git_refs_cover_branches_tags_and_remotes: cannot seed repo");
            return;
        }
        assert!(git_ok(&dir, &["tag", "v1.0"]));
        assert!(git_ok(
            &dir,
            &["update-ref", "refs/remotes/origin/feature", "HEAD"]
        ));

        let cwd = dir.to_string_lossy().into_owned();
        let refs = git_refs(&cwd, true).expect("refs");
        assert!(refs.contains(&"main".to_string()), "{refs:?}");
        assert!(refs.contains(&"v1.0".to_string()), "{refs:?}");
        assert!(refs.contains(&"origin/feature".to_string()), "{refs:?}");

        let switch_refs = git_refs(&cwd, false).expect("refs");
        assert!(
            !switch_refs.contains(&"v1.0".to_string()),
            "{switch_refs:?}"
        );

        let out = suggest("git checkout mian", &cwd, &[]);
        assert!(out.iter().any(|s| s == "git checkout main"), "got: {out:?}");
        let out = suggest("git checkout v1.1", &cwd, &[]);
        assert!(out.iter().any(|s| s == "git checkout v1.0"), "got: {out:?}");
        let out = suggest("git switch mian", &cwd, &[]);
        assert!(out.iter().any(|s| s == "git switch main"), "got: {out:?}");
        let out = suggest("git switch v1.1", &cwd, &[]);
        assert!(
            !out.iter().any(|s| s.contains("v1.0")),
            "switch offered a tag: {out:?}"
        );
        let out = suggest("git checkout origin/featre", &cwd, &[]);
        assert!(
            out.iter().any(|s| s == "git checkout origin/feature"),
            "got: {out:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_kills_a_hung_child() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 5"]);
        let start = Instant::now();
        let err = run_with_timeout(cmd, Duration::from_millis(50)).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(2), "took too long");
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
    fn control_characters_never_reach_candidates() {
        let out = suggest("cd bad\u{1b}[31m", "/tmp", &[]);
        assert!(out.is_empty(), "control input produced {out:?}");
        let history = vec!["ls --co\u{1b}lor".to_string()];
        let out = suggest("ls --colr", "/tmp", &history);
        assert!(
            out.iter().all(|s| !s.chars().any(char::is_control)),
            "control leaked: {out:?}"
        );
        let history = vec!["l\u{1b}s".to_string()];
        let out = suggest("lz", "/tmp", &history);
        assert!(
            out.iter().all(|s| !s.chars().any(char::is_control)),
            "history verb control leaked: {out:?}"
        );
    }

    #[test]
    fn no_absurd_suggestions() {
        let out = suggest("rm -rf / tmp/foo", "/tmp", &[]);
        assert!(!out.iter().any(|s| s.contains("rm")), "got: {out:?}");
        let out2 = suggest("ls", "/tmp", &[]);
        assert!(out2.is_empty(), "got: {out2:?}");
    }

    // ---- Medium-finding regressions -------------------------------------

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("gl-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl std::ops::Deref for TempDir {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for TempDir {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp_dir(tag: &str) -> TempDir {
        TempDir::new(tag)
    }

    fn fake_bins(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn with_context(
        cmd: &str,
        cwd: &str,
        bins: &BTreeSet<String>,
        home: Option<&Path>,
        euid: u32,
    ) -> Vec<String> {
        suggest_with_context(cmd, cwd, &[], None, Some(bins), home, euid)
    }

    fn osa_reference(a: &str, b: &str) -> usize {
        let a: Vec<char> = a.chars().collect();
        let b: Vec<char> = b.chars().collect();
        let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
        for (i, row) in d.iter_mut().enumerate() {
            row[0] = i;
        }
        for (j, cell) in d[0].iter_mut().enumerate() {
            *cell = j;
        }
        for i in 1..=a.len() {
            for j in 1..=b.len() {
                let cost = usize::from(a[i - 1] != b[j - 1]);
                d[i][j] = (d[i - 1][j] + 1)
                    .min(d[i][j - 1] + 1)
                    .min(d[i - 1][j - 1] + cost);
                if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                    d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
                }
            }
        }
        d[a.len()][b.len()]
    }

    #[test]
    fn osa_rolling_rows_match_reference() {
        for (a, b) in [
            ("", ""),
            ("a", ""),
            ("", "a"),
            ("gti", "git"),
            ("chekout", "checkout"),
            ("ab", "ba"),
            ("abc", "cba"),
            ("aab", "aba"),
            ("é", "e"),
            ("downloads", "download"),
        ] {
            assert_eq!(osa(a, b), osa_reference(a, b), "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn threshold_counts_characters_not_bytes() {
        // Six chars: threshold is 2. As bytes (12) the old formula allowed 4.
        assert!(!within_threshold("éééééé", 3));
        assert!(within_threshold("éééééé", 2));
    }

    #[test]
    fn best_ties_are_deterministic_by_name() {
        let pref = HashSet::new();
        assert_eq!(best("fooo", ["foob", "fooa"], &pref), Some("fooa"));
        assert_eq!(best("fooo", ["fooa", "foob"], &pref), Some("fooa"));
        assert_eq!(best("fooo", ["foot", "foob", "fooa"], &pref), Some("fooa"));
    }

    #[test]
    fn best_skips_length_mismatches_and_overlong_words() {
        let pref = HashSet::new();
        assert_eq!(best("ls", ["lsaaaaaaaa"], &pref), None);
        let long = "a".repeat(MAX_WORD_LEN + 1);
        assert_eq!(best(&long, ["aa"], &pref), None);
        assert_eq!(best("aa", [long.as_str()], &pref), None);
    }

    #[test]
    fn path_bins_from_is_sorted_and_executable_only() {
        let dir = tmp_dir("bins");
        let exec = |name: &str| {
            let path = dir.join(name);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        };
        exec("foob");
        exec("fooa");
        std::fs::write(dir.join("plain.txt"), "x").unwrap();
        std::fs::create_dir_all(dir.join("adir")).unwrap();

        let bins = crate::shell::path_bins_from(dir.as_os_str());
        let names: Vec<&str> = bins.iter().map(String::as_str).collect();
        assert_eq!(names, ["fooa", "foob"]);
    }

    #[test]
    fn known_command_resolves_against_cwd_and_requires_executable_file() {
        let dir = tmp_dir("known");
        let tool = dir.join("tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        let plain = dir.join("plain");
        std::fs::write(&plain, "x").unwrap();
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let cwd = dir.to_string_lossy().into_owned();
        let empty = BTreeSet::new();
        assert!(known_command("./tool", &cwd, &empty));
        assert!(!known_command("./sub", &cwd, &empty), "directory accepted");
        assert!(
            !known_command("./plain", &cwd, &empty),
            "non-executable accepted"
        );
        assert!(!known_command("./missing", &cwd, &empty));
    }

    #[test]
    fn mkdir_repair_does_not_repeat_the_original() {
        let dir = tmp_dir("mkdir4");
        let cwd = dir.to_string_lossy().into_owned();
        let out = suggest("mkdir a/b", &cwd, &[]);
        assert_eq!(out, vec!["mkdir -p a/b"], "got: {out:?}");
        assert!(!out.iter().any(|s| s.contains("&&")), "duplicate: {out:?}");

        let out = suggest("mkdir -p a/b", &cwd, &[]);
        assert!(out.is_empty(), "already creates parents: {out:?}");

        let out = suggest("mkdir -m 700 a/b", &cwd, &[]);
        assert_eq!(out, vec!["mkdir -p -m 700 a/b"], "got: {out:?}");
    }

    #[test]
    fn cp_and_mv_target_directory_semantics() {
        let dir = tmp_dir("cptarget");
        std::fs::write(dir.join("file"), "x").unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        for (cmd, expected) in [
            ("cp file newdir/", "mkdir -p newdir && cp file newdir/"),
            ("cp -t newdir file", "mkdir -p newdir && cp -t newdir file"),
            ("cp -tnewdir file", "mkdir -p newdir && cp -tnewdir file"),
            (
                "cp --target-directory=newdir file",
                "mkdir -p newdir && cp --target-directory=newdir file",
            ),
            ("cp a b newdir", "mkdir -p newdir && cp a b newdir"),
            (
                "mv file newdir/file2",
                "mkdir -p newdir && mv file newdir/file2",
            ),
        ] {
            let out = suggest(cmd, &cwd, &[]);
            assert!(out.iter().any(|s| s == expected), "{cmd}: {out:?}");
        }
        // A plain rename needs no target directory.
        let out = suggest("cp file other", &cwd, &[]);
        assert!(out.is_empty(), "got: {out:?}");
        // `-t` here is the value of `-S`, not a target-directory option.
        let out = suggest("cp -S -t file", &cwd, &[]);
        assert!(out.is_empty(), "option value treated as target: {out:?}");
    }

    #[test]
    fn cd_requires_a_directory_and_expands_tilde() {
        let dir = tmp_dir("cdtype");
        std::fs::write(dir.join("Downloads"), "not a dir").unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        let out = suggest("cd downloads", &cwd, &[]);
        assert!(out.is_empty(), "file accepted as cd target: {out:?}");

        let home = tmp_dir("home");
        std::fs::create_dir_all(home.join("Downloads")).unwrap();
        let work = tmp_dir("work");
        let cwd = work.to_string_lossy().into_owned();
        let bins = BTreeSet::new();
        let out = with_context("cd ~/downloads", &cwd, &bins, Some(home.as_ref()), 1000);
        assert!(out.iter().any(|s| s == "cd ~/Downloads"), "got: {out:?}");
    }

    #[test]
    fn sudo_rule_is_token_aware_and_guarded() {
        let dir = tmp_dir("sudo");
        let cwd = dir.to_string_lossy().into_owned();
        let pool = fake_bins(&["sudo", "systemctl", "mkdir", "tee", "apt", "apt-get"]);

        let out = with_context("systemctl restart nginx", &cwd, &pool, None, 1000);
        assert!(
            out.iter().any(|s| s == "sudo systemctl restart nginx"),
            "got: {out:?}"
        );

        // Prefix collision: `systemctlfoo` is not `systemctl`.
        let out = with_context("systemctlfoo restart nginx", &cwd, &pool, None, 1000);
        assert!(
            !out.iter().any(|s| s.starts_with("sudo")),
            "prefix matched: {out:?}"
        );

        // `-p` form is recognized and never duplicated.
        let out = with_context("mkdir -p /etc/ghostline-review", &cwd, &pool, None, 1000);
        assert!(
            out.iter()
                .any(|s| s == "sudo mkdir -p /etc/ghostline-review"),
            "got: {out:?}"
        );
        assert!(!out.iter().any(|s| s.contains("&&")), "duplicate: {out:?}");

        // Root gets no sudo suggestion.
        let out = with_context("systemctl restart nginx", &cwd, &pool, None, 0);
        assert!(!out.iter().any(|s| s.starts_with("sudo")), "got: {out:?}");

        // No privilege tool installed.
        let no_sudo = fake_bins(&["systemctl", "mkdir", "tee"]);
        let out = with_context("systemctl restart nginx", &cwd, &no_sudo, None, 1000);
        assert!(!out.iter().any(|s| s.starts_with("sudo")), "got: {out:?}");

        // exit 127 means the command is missing, not that it needs root.
        let out = suggest_with_context(
            "systemctl restart nginx",
            &cwd,
            &[],
            Some(127),
            Some(&pool),
            None,
            1000,
        );
        assert!(!out.iter().any(|s| s.starts_with("sudo")), "got: {out:?}");
    }

    #[test]
    fn flag_index_skips_option_values_but_finds_short_flags() {
        assert_eq!(flag_index(&["grep", "-e", "-fo", "file"]), None);
        assert_eq!(flag_index(&["cat", "-N", "file"]), Some(1));
        assert_eq!(flag_index(&["ls", "--color=auto", "file"]), Some(1));
        let history = vec!["cat -n file".to_string()];
        let out = suggest("cat -N file", "/tmp", &history);
        assert!(out.iter().any(|s| s == "cat -n file"), "got: {out:?}");
    }

    #[test]
    fn history_option_values_do_not_become_flag_candidates() {
        let history = vec!["grep -f -colr file".to_string()];
        let out = suggest("grep --colr file", "/tmp", &history);
        assert!(
            !out.iter().any(|s| s.contains("-colr")),
            "option value became a flag: {out:?}"
        );
        assert!(out.is_empty(), "got: {out:?}");
    }

    #[test]
    fn path_kind_is_filtered_before_ranking() {
        let dir = tmp_dir("pathkind");
        std::fs::write(dir.join("Downloads"), "file").unwrap();
        std::fs::create_dir_all(dir.join("downloads")).unwrap();
        let cwd = dir.to_string_lossy().into_owned();

        // Case-fold exact matches a file and a directory; only the directory
        // can be a `cd` target.
        let out = suggest("cd DOWNlOADS", &cwd, &[]);
        assert!(out.iter().any(|s| s == "cd downloads"), "got: {out:?}");

        // `ls` and `cat` keep their own type rules.
        let out = suggest("ls downlods", &cwd, &[]);
        assert!(out.iter().any(|s| s == "ls Downloads"), "got: {out:?}");
        let out = suggest("cat downlods", &cwd, &[]);
        assert!(out.iter().any(|s| s == "cat Downloads"), "got: {out:?}");
        std::fs::create_dir_all(dir.join("reports")).unwrap();
        let out = suggest("cat repotrs", &cwd, &[]);
        assert!(out.is_empty(), "directory accepted for cat: {out:?}");
    }
}
