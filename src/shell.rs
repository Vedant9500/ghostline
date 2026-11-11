//! Shared shell vocabulary and PATH command discovery.
//!
//! Command completion and typo repair must agree on what counts as a shell
//! builtin, a reserved word, and an executable on `PATH`. Keeping one list
//! here avoids the drift that previously made `fix` reject `typeset` while
//! completion offered it (and vice versa).

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::Path;

/// Upper bound on names collected from `PATH`, shared by completion and fix.
pub const MAX_PATH_BINS: usize = 20_000;

/// Bash builtins. Reserved words are deliberately kept separate: completion
/// may offer them, but a typo repair must never suggest `if` as a command.
pub const BUILTINS: &[&str] = &[
    ":",
    ".",
    "[",
    "alias",
    "bg",
    "bind",
    "break",
    "builtin",
    "caller",
    "cd",
    "command",
    "compgen",
    "complete",
    "compopt",
    "continue",
    "declare",
    "dirs",
    "disown",
    "echo",
    "enable",
    "eval",
    "exec",
    "exit",
    "export",
    "false",
    "fc",
    "fg",
    "getopts",
    "hash",
    "help",
    "history",
    "jobs",
    "kill",
    "let",
    "local",
    "logout",
    "mapfile",
    "popd",
    "printf",
    "pushd",
    "pwd",
    "read",
    "readarray",
    "readonly",
    "return",
    "set",
    "shift",
    "shopt",
    "source",
    "suspend",
    "test",
    "times",
    "trap",
    "true",
    "type",
    "typeset",
    "ulimit",
    "umask",
    "unalias",
    "unset",
    "wait",
];

/// Bash reserved words and compound-command punctuation.
pub const RESERVED: &[&str] = &[
    "!", "{", "}", "[[", "]]", "case", "coproc", "do", "done", "elif", "else", "esac", "fi", "for",
    "function", "if", "in", "select", "then", "time", "until", "while",
];

pub fn is_builtin(word: &str) -> bool {
    BUILTINS.contains(&word)
}

pub fn is_reserved(word: &str) -> bool {
    RESERVED.contains(&word)
}

pub fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path)
            .map(|m| m.is_file())
            .unwrap_or(false)
    }
}

/// Executable names on the explicit `PATH` value, sorted and deduped.
/// Empty `PATH` entries (meaning cwd) are skipped: never suggest random cwd
/// files.
pub fn path_bins_from(path: &OsStr) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.is_empty() || out.contains(&name) {
                continue;
            }
            if is_executable(&entry.path()) {
                out.insert(name);
            }
            if out.len() >= MAX_PATH_BINS {
                return out;
            }
        }
    }
    out
}

/// Executable names on the process `PATH`.
pub fn path_bins() -> BTreeSet<String> {
    std::env::var_os("PATH")
        .map(|path| path_bins_from(&path))
        .unwrap_or_default()
}

/// All known command names: `PATH` executables ∪ builtins ∪ reserved words,
/// sorted + deduped. `path_override` is for tests (avoids mutating the
/// process-wide `PATH`, which cargo shares across parallel test threads).
pub fn command_names_from(path_override: Option<&OsStr>) -> Vec<String> {
    let mut set = match path_override {
        Some(path) => path_bins_from(path),
        None => path_bins(),
    };
    set.extend(BUILTINS.iter().map(|s| (*s).to_string()));
    set.extend(RESERVED.iter().map(|s| (*s).to_string()));
    set.into_iter().collect()
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

    fn fake_bin(tag: &str, names: &[&str]) -> TempDir {
        let dir = std::env::temp_dir().join(format!("gl-shell-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in names {
            let path = dir.join(name);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let decoy = dir.join("decoy.txt");
        std::fs::write(&decoy, "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&decoy, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        TempDir(dir)
    }

    #[test]
    fn vocabulary_separates_builtins_from_reserved_words() {
        for word in ["typeset", "readonly", "mapfile", ":", "."] {
            assert!(is_builtin(word), "{word} should be builtin");
            assert!(!is_reserved(word), "{word} should not be reserved");
        }
        for word in ["if", "time", "coproc", "[[", "!"] {
            assert!(is_reserved(word), "{word} should be reserved");
            assert!(!is_builtin(word), "{word} should not be builtin");
        }
    }

    #[test]
    fn path_bins_are_sorted_capped_and_executable_only() {
        let dir = fake_bin("bins", &["zeta", "alpha"]);
        let bins = path_bins_from(dir.as_os_str());
        let names: Vec<&str> = bins.iter().map(String::as_str).collect();
        assert_eq!(names, ["alpha", "zeta"]);
        assert!(
            !bins.contains("decoy.txt"),
            "non-executable leaked: {bins:?}"
        );
    }

    #[test]
    fn command_names_union_builtins_and_reserved() {
        let dir = fake_bin("names", &["zeta"]);
        let names = command_names_from(Some(dir.as_os_str()));
        for expected in ["zeta", "typeset", "if", ":"] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
        assert!(names.windows(2).all(|w| w[0] < w[1]), "not sorted");
    }
}
