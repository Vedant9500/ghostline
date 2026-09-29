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

/// True if cmd matches any pattern in GHOSTLINE_IGNORE (or legacy HISTIGNORE-style ` ` prefix).
pub fn is_ignored(cmd: &str, ignore_env: Option<&str>) -> bool {
    if cmd.starts_with(' ') {
        return true; // mirrors HISTCONTROL=ignorespace
    }
    if let Some(env) = ignore_env {
        for pat in env.split(|c| c == ',' || c == ':' || c == '\n') {
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
    fn ignore_glob_and_space_prefix() {
        assert!(is_ignored(" secret export FOO=1", None));
        assert!(is_ignored("export AWS_SECRET=1", Some("*SECRET*")));
        assert!(!is_ignored("git status", Some("*SECRET*")));
    }
}
