//! Command-name completion fallback for ghost suggestions.
//!
//! When history has no prefix match, ghost falls back to the same pool bash
//! Tab-completion draws from: executables on `PATH` + shell builtins. This
//! mirrors the `completion` strategy of zsh-style autosuggesters (first
//! completion result becomes the ghost). History always wins when present.
//!
//! Scope (v1): first-token position only (buffer without whitespace).
//! Argument/flag/file completion is deferred.

use std::ffi::OsStr;

/// All known command names: `PATH` executables ∪ builtins ∪ reserved words,
/// sorted + deduped. Shared with typo repair via [`crate::shell`].
/// `path_override` is for tests (avoids mutating process-wide `PATH`, which
/// cargo shares across parallel test threads); production passes `None`.
pub fn command_names_from(path_override: Option<&OsStr>) -> Vec<String> {
    crate::shell::command_names_from(path_override)
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

    struct TempDir(std::path::PathBuf);

    impl std::ops::Deref for TempDir {
        type Target = std::path::Path;

        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fake_bin(names: &[&str]) -> TempDir {
        let mut dir = std::env::temp_dir();
        dir.push(format!("gl-comp-{}-{}", std::process::id(), names.len()));
        let _ = std::fs::remove_dir_all(&dir);
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
        TempDir(dir)
    }

    #[test]
    fn completes_path_executables_not_decoys() {
        let dir = fake_bin(&["codex", "codiff", "other"]);
        let got = complete_command_in("co", Some(dir.as_os_str()));
        assert_eq!(&got[..2], ["codex", "codiff"]);
        assert!(
            !got.iter().any(|c| c == "cosecret.txt"),
            "decoy excluded: {got:?}"
        );
        assert!(!got.iter().any(|c| c == "other"), "prefix filter: {got:?}");
    }

    #[test]
    fn completes_builtins() {
        let dir = fake_bin(&[]);
        let got = complete_command_in("comp", Some(dir.as_os_str()));
        assert!(
            got.contains(&"complete".to_string()),
            "builtins included: {got:?}"
        );
        assert!(
            got.contains(&"compgen".to_string()),
            "builtins included: {got:?}"
        );
    }

    #[test]
    fn exact_match_excluded_and_empty_guarded() {
        let dir = fake_bin(&["codex"]);
        let got = complete_command_in("codex", Some(dir.as_os_str()));
        assert!(!got.contains(&"codex".to_string()));
        assert!(complete_command_in("", Some(dir.as_os_str())).is_empty());
    }
}
