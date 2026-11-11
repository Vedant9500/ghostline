use regex::Regex;
use std::sync::OnceLock;

static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();

fn rules() -> &'static Vec<(Regex, &'static str)> {
    RULES.get_or_init(|| {
        let pats: &[(&str, &str)] = &[
            // --password secret  / --token xyz
            (r#"(?i)(--?(?:password|passwd|pwd|secret|token|api[_-]?key)\s+)\S+"#, "$1[REDACTED]"),
            // key=value  (password=foo, token: bar)
            (r#"(?i)((?:password|passwd|pwd|secret|token|api[_-]?key|auth)[\s"']*[:=][\s"']*)[^\s"']+"#, "${1}[REDACTED]"),
            // Bearer tokens
            (r#"(?i)\b(Bearer\s+)[^\s]+"#, "${1}[REDACTED]"),
            // OpenAI / Anthropic / GitHub / AWS / Google / Slack / Stripe-ish
            (r"\bsk-[A-Za-z0-9]{20,}\b", "[REDACTED]"),
            (r"\bsk-ant-[A-Za-z0-9\-_]{20,}\b", "[REDACTED]"),
            (r"\b(?:ghp|gho|ghu|ghs)_[A-Za-z0-9]{20,}\b", "[REDACTED]"),
            (r"\bgithub_pat_[A-Za-z0-9_]{20,}\b", "[REDACTED]"),
            (r"\bAKIA[A-Z0-9]{16}\b", "[REDACTED]"),
            (r"\bAIza[0-9A-Za-z\-_]{35}\b", "[REDACTED]"),
            (r"\bxox[bpa]-[A-Za-z0-9\-]{10,}\b", "[REDACTED]"),
            (r"\b[rs]k_(?:live|test)_[A-Za-z0-9]{10,}\b", "[REDACTED]"),
            // JWT (three base64url segments)
            (r"\bey[A-Za-z0-9_\-=]{10,}\.[A-Za-z0-9_\-=]{10,}\.[A-Za-z0-9_\-=]{10,}", "[REDACTED]"),
            // PEM private keys inlined
            (r"-----BEGIN [A-Z ]*PRIVATE KEY-----[^-]*-----END [A-Z ]*PRIVATE KEY-----", "[REDACTED-KEY]"),
            // DB URLs with password: scheme://user:pass@
            (r#"(?i)\b((?:postgres|postgresql|mysql|mongodb|redis)(?:\+[a-z]+)?://[^:/@\s]+:)[^@\s]+@"#, "${1}[REDACTED]@"),
            // curl -u user:pass
            (r#"(?i)(\bcurl\b[^\n]*?\s-u\s+[^\s:]+:)[^\s]+"#, "${1}[REDACTED]"),
        ];
        pats.iter()
            .map(|(p, r)| (Regex::new(p).expect("redact regex"), *r))
            .collect()
    })
}

/// Returns (redacted_cmd, was_sensitive).
pub fn redact(cmd: &str) -> (String, bool) {
    let mut out = cmd.to_string();
    let mut sensitive = false;
    for (re, rep) in rules() {
        if re.is_match(&out) {
            sensitive = true;
            out = re.replace_all(&out, *rep).into_owned();
        }
    }
    (out, sensitive)
}

/// Remove C0/C1 controls (ESC, BEL, newline, DEL, ...). Stored commands are
/// later replayed to a terminal by `search`/`suggest`, so untrusted history
/// (HISTFILE imports, pasted commands) must not carry escape sequences.
pub fn strip_controls(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Normalize whitespace and strip terminal controls from a captured command.
/// Normalize first so tabs/newlines still separate tokens, then normalize
/// again to close gaps left by removed non-whitespace controls.
pub fn clean_command(cmd: &str) -> String {
    let cmd = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    strip_controls(&cmd)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Truncate to at most `max_bytes` without splitting a UTF-8 character.
/// `String::truncate` panics on a non-boundary, so never call it with a raw
/// byte count taken from `len()`.
pub fn truncate_utf8(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// Tiny glob matcher supporting `*` (used for GHOSTLINE_IGNORE, comma/space separated).
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<u8> = pattern.bytes().collect();
    let t: Vec<u8> = text.bytes().collect();
    let (mut pi, mut ti, mut star, mut mark) = (0usize, 0usize, None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// True for commands whose suggestion would be a footgun (docs/07). The list
/// is deliberately conservative but over-matching is safer than offering a
/// destructive command: these are only suppressed from ghost suggestions,
/// never removed from history or search.
pub fn is_destructive(cmd: &str) -> bool {
    let lower = cmd.to_ascii_lowercase();
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    let lower_tokens: Vec<String> = tokens.iter().map(|t| t.to_ascii_lowercase()).collect();
    let has = |needle: &str| lower_tokens.iter().any(|t| t == needle);
    let has_prefix = |prefix: &str| lower_tokens.iter().any(|t| t.starts_with(prefix));

    if lower.contains(":(){") || lower.contains(":|:&") {
        return true; // fork bomb
    }
    if has_prefix("mkfs") || has("shutdown") || has("reboot") || has("poweroff") || has("halt") {
        return true;
    }

    for i in 0..tokens.len() {
        match lower_tokens[i].as_str() {
            "git" => {
                let rest = &lower_tokens[i + 1..];
                let mut j = 0;
                while j < rest.len() && rest[j].starts_with('-') {
                    // Options that take a separate value.
                    if matches!(rest[j].as_str(), "-c" | "-C" | "--git-dir" | "--work-tree") {
                        j += 1;
                    }
                    j += 1;
                }
                if j >= rest.len() {
                    continue;
                }
                let args = &rest[j + 1..];
                match rest[j].as_str() {
                    "push" => {
                        if args.iter().any(|a| {
                            a == "-f"
                                || a == "-d"
                                || a.starts_with("--force")
                                || matches!(
                                    a.as_str(),
                                    "--mirror" | "--delete" | "--prune" | "--prune=now"
                                )
                                // `git push origin +main` force-updates a ref.
                                || a.starts_with('+')
                        }) {
                            return true;
                        }
                    }
                    "reset" => {
                        if args.iter().any(|a| a == "--hard") {
                            return true;
                        }
                    }
                    "clean" => {
                        if args.iter().any(|a| {
                            a == "--force"
                                || a.strip_prefix('-')
                                    .map(|f| !f.starts_with('-') && f.contains('f'))
                                    .unwrap_or(false)
                        }) {
                            return true;
                        }
                    }
                    "branch" => {
                        let raw = &tokens[i + 1 + j + 1..];
                        if raw.contains(&"-D")
                            || (args.iter().any(|a| a == "--delete")
                                && args.iter().any(|a| a == "--force"))
                        {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
            "rm" | "rmdir" => {
                if lower_tokens[i].as_str() == "rmdir" {
                    return true;
                }
                let mut recursive = false;
                let mut force = false;
                for arg in &tokens[i + 1..] {
                    if let Some(flags) = arg.strip_prefix('-') {
                        if let Some(long) = flags.strip_prefix('-') {
                            recursive |= long == "recursive";
                            force |= long == "force";
                        } else {
                            recursive |= flags.contains('r') || flags.contains('R');
                            force |= flags.contains('f');
                        }
                    }
                }
                if recursive || force {
                    return true;
                }
            }
            "chmod" => {
                let args = &tokens[i + 1..];
                let recursive = args.iter().any(|a| {
                    *a == "--recursive"
                        || a.strip_prefix('-')
                            .map(|f| !f.starts_with('-') && (f.contains('R') || f.contains('r')))
                            .unwrap_or(false)
                });
                if recursive && args.iter().any(|a| *a == "777" || *a == "0777") {
                    return true;
                }
            }
            "dd" if lower_tokens[i + 1..].iter().any(|a| a.starts_with("of=")) => return true,
            _ => {}
        }
    }
    false
}

/// True if cmd matches any pattern in GHOSTLINE_IGNORE (or legacy HISTIGNORE-style ` ` prefix).
pub fn is_ignored(cmd: &str, ignore_env: Option<&str>) -> bool {
    if cmd.starts_with(' ') {
        return true; // mirrors HISTCONTROL=ignorespace
    }
    if let Some(env) = ignore_env {
        for pat in env.split([',', ':', '\n']) {
            let pat = pat.trim();
            if pat.is_empty() {
                continue;
            }
            // space-separated also allowed
            for p in pat.split_whitespace() {
                if glob_match(p, cmd) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_password_assignments() {
        let (out, s) = redact("deploy --password hunter2 --user bob");
        assert!(s);
        assert!(!out.contains("hunter2"));
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_bearer_and_github() {
        let (out, s) = redact(
            "curl -H 'Authorization: Bearer abc123XYZ' api; export GH=ghp_abcdef0123456789ABCD",
        );
        assert!(s);
        assert!(!out.contains("abc123XYZ"));
        assert!(!out.contains("ghp_abcdef"));
    }

    #[test]
    fn redacts_db_url_and_keeps_host() {
        let (out, s) = redact("psql postgres://bob:s3cret@db:5432/app");
        assert!(s);
        assert!(!out.contains("s3cret"));
        assert!(out.contains("db:5432"));
    }

    #[test]
    fn clean_cmd_not_flagged() {
        let (out, s) = redact("git status --short");
        assert!(!s);
        assert_eq!(out, "git status --short");
    }

    #[test]
    fn strips_terminal_controls() {
        assert_eq!(
            strip_controls("echo \x1b[7mBOLD\x1b[0m\x07 ok"),
            "echo [7mBOLD[0m ok"
        );
        assert_eq!(strip_controls("a\tb\nc\u{7f}d\u{9b}e"), "abcde");
        assert_eq!(strip_controls("git status"), "git status");
    }

    #[test]
    fn clean_command_normalizes_after_stripping() {
        assert_eq!(clean_command("  echo \x1b  hi  "), "echo hi");
        assert_eq!(clean_command("echo\tfoo\nbar"), "echo foo bar");
        assert_eq!(clean_command("\x1b\x07"), "");
    }

    #[test]
    fn ignore_glob_and_space_prefix() {
        assert!(is_ignored(" secret export FOO=1", None));
        assert!(is_ignored("export AWS_SECRET=1", Some("*SECRET*")));
        assert!(!is_ignored("git status", Some("*SECRET*")));
    }

    #[test]
    fn truncate_utf8_never_splits_a_character() {
        let mut s = format!("{}é{}", "a".repeat(8191), "z".repeat(10));
        truncate_utf8(&mut s, 8192);
        assert_eq!(s, "a".repeat(8191));
        let mut s = String::from("ééé");
        truncate_utf8(&mut s, 5);
        assert_eq!(s, "éé");
        let mut s = String::from("short");
        truncate_utf8(&mut s, 99);
        assert_eq!(s, "short");
    }

    #[test]
    fn destructive_suggestions_are_detected() {
        assert!(is_destructive("git push --force origin main"));
        assert!(is_destructive("git push -f origin main"));
        assert!(is_destructive("git push --force-with-lease"));
        assert!(is_destructive("git push origin +main"));
        assert!(is_destructive("git push --mirror origin"));
        assert!(is_destructive("git push -d origin old"));
        assert!(is_destructive("git reset --hard HEAD~1"));
        assert!(is_destructive("git clean -fdx"));
        assert!(is_destructive("git branch -D old"));
        assert!(is_destructive("rm -rf /tmp/x"));
        assert!(is_destructive("rm -fr /tmp/x"));
        assert!(is_destructive("rm -f notes.txt"));
        assert!(is_destructive("sudo rm -rf /tmp/x"));
        assert!(is_destructive("chmod -R 777 /"));
        assert!(is_destructive("dd if=/dev/zero of=/dev/sda"));
        assert!(is_destructive("shutdown -h now"));
        assert!(is_destructive(":(){ :|:& };:"));
        assert!(!is_destructive("git push origin main"));
        assert!(!is_destructive("git reset HEAD file"));
        assert!(!is_destructive("git clean -n"));
        assert!(!is_destructive("git branch -d merged"));
        assert!(!is_destructive("rm notes.txt"));
        assert!(!is_destructive("chmod 644 notes.txt"));
    }
}
