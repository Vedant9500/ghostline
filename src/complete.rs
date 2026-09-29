//! Command-name completion fallback for ghost suggestions.
//!
//! When history has no prefix match, ghost falls back to the same pool bash
//! Tab-completion draws from: executables on `PATH` + shell builtins. This
//! mirrors the `completion` strategy of zsh-style autosuggesters (first
//! completion result becomes the ghost). History always wins when present.
//!
//! Scope (v1): first-token position only (buffer without whitespace).
//! Argument/flag/file completion is deferred.

use std::collections::BTreeSet;

/// Bash builtins + reserved words (static; aliases/functions need the live
/// shell and are unavailable out-of-process).
const BUILTINS: &[&str] = &[
    "alias", "bg", "bind", "break", "builtin", "caller", "case", "cd", "command", "compgen",
    "complete", "compopt", "continue", "coproc", "declare", "dirs", "disown", "do", "done",
    "echo", "elif", "else", "enable", "esac", "eval", "exec", "exit", "export", "false", "fc",
    "fg", "fi", "for", "function", "getopts", "hash", "help", "history", "if", "in", "jobs",
    "kill", "let", "local", "logout", "mapfile", "popd", "printf", "pushd", "pwd", "read",
    "readarray", "return", "select", "set", "shift", "shopt", "source", "suspend", "test",
    "then", "time", "times", "trap", "true", "type", "typeset", "ulimit", "umask", "unalias",
    "unset", "until", "wait", "while",
];

fn is_executable(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
    }
}

/// All known command names: `PATH` executables ∪ builtins, sorted + deduped.
/// Empty `PATH` entries (meaning cwd) are skipped — never suggest random cwd files.
/// `path_override` is for tests (avoids mutating process-wide `PATH`, which
/// cargo shares across parallel test threads); production passes `None`.
pub fn command_names_from(path_override: Option<&std::ffi::OsStr>) -> Vec<String> {
    let mut set = BTreeSet::new();
    let path = path_override.map(|s| s.to_os_string()).or_else(|| std::env::var_os("PATH"));
    if let Some(path) = path {
        for dir in std::env::split_paths(&path) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if name.is_empty() || set.contains(name) {
                    continue;
                }
                if is_executable(&entry.path()) {
                    set.insert(name.to_string());
                }
                if set.len() >= 20_000 {
                    break;
                }
            }
        }
    }
    for b in BUILTINS {
        set.insert(b.to_string());
    }
    set.into_iter().collect()
}

/// Full command-name candidates for `prefix` (excludes the exact match —
/// ghost needs a suffix). Sorted; history layer filters/dedupes first.
pub fn complete_command(prefix: &str) -> Vec<String> {
    complete_command_in(prefix, None)
}

/// Same, with an explicit `PATH` value (tests; see `command_names_from`).
pub fn complete_command_in(prefix: &str, path: Option<&std::ffi::OsStr>) -> Vec<String> {
    if prefix.is_empty() || prefix.len() > 200 {
        return vec![];
    }
    command_names_from(path)
        .into_iter()
        .filter(|c| c.len() > prefix.len() && c.starts_with(prefix))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_bin(names: &[&str]) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("gl-comp-{}-{}", std::process::id(), names.len()));
        std::fs::create_dir_all(&dir).unwrap();
        for n in names {
            let p = dir.join(n);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        // Non-executable decoy must never be suggested.
        let decoy = dir.join("cosecret.txt");
        std::fs::write(&decoy, "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&decoy, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        dir
    }

    #[test]
    fn completes_path_executables_not_decoys() {
        let dir = fake_bin(&["codex", "codiff", "other"]);
        let got = complete_command_in("co", Some(dir.as_os_str()));
        assert_eq!(&got[..2], ["codex", "codiff"]);
        assert!(!got.iter().any(|c| c == "cosecret.txt"), "decoy excluded: {got:?}");
        assert!(!got.iter().any(|c| c == "other"), "prefix filter: {got:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn completes_builtins() {
        let dir = fake_bin(&[]);
        let got = complete_command_in("comp", Some(dir.as_os_str()));
        assert!(got.contains(&"complete".to_string()), "builtins included: {got:?}");
        assert!(got.contains(&"compgen".to_string()), "builtins included: {got:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exact_match_excluded_and_empty_guarded() {
        let dir = fake_bin(&["codex"]);
        let got = complete_command_in("codex", Some(dir.as_os_str()));
        assert!(!got.contains(&"codex".to_string()));
        assert!(complete_command_in("", Some(dir.as_os_str())).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
