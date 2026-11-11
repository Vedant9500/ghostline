use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ghostline")
}

/// Best-effort fixture cleanup even when an assertion panics.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("gl-fix-{tag}-{}", std::process::id()));
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

fn log(db: &std::path::Path, cwd: &str, exit: &str, cmd: &str) {
    let out = Command::new(bin())
        .env_remove("GHOSTLINE_IGNORE")
        .env_remove("GHOSTLINE_QUEUE")
        .args(["log", "--db"])
        .arg(db)
        .args([
            "--shell",
            "bash",
            "--cwd",
            cwd,
            "--exit",
            exit,
            "--session",
            "s",
            "--",
            cmd,
        ])
        .output()
        .expect("run ghostline log");
    assert!(
        out.status.success(),
        "log failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The latest-failure query is timestamp-ordered; keep the two rows distinct.
    std::thread::sleep(Duration::from_millis(2));
}

#[test]
fn fix_last_refuses_sensitive_history() {
    let dir = TempDir::new("sensitive");
    let db = dir.join("history.db");
    let cwd = dir.to_string_lossy().into_owned();

    log(
        &db,
        &cwd,
        "1",
        "curl --password hunter2 https://example.test",
    );

    let out = Command::new(bin())
        .env_remove("GHOSTLINE_IGNORE")
        .args(["fix", "--last", "--db"])
        .arg(&db)
        .output()
        .expect("run ghostline fix");
    assert!(!out.status.success(), "sensitive fix should fail closed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stdout.contains("hunter2"));
    assert!(!stderr.contains("hunter2"));
    assert!(!stdout.contains("did you mean"), "got: {stdout}");
}

#[test]
fn fix_refuses_a_sensitive_explicit_command() {
    let dir = TempDir::new("sensitive-explicit");
    let db = dir.join("missing.db");
    let cwd = dir.to_string_lossy().into_owned();

    let out = Command::new(bin())
        .env_remove("GHOSTLINE_IGNORE")
        .args(["fix", "--db"])
        .arg(&db)
        .args([
            "--cwd",
            &cwd,
            "--",
            "curl",
            "--password",
            "hunter2",
            "https://example.test",
        ])
        .output()
        .expect("run ghostline fix");
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stdout.contains("hunter2"));
    assert!(!stderr.contains("hunter2"));
    assert!(!stdout.contains("did you mean"), "got: {stdout}");
}

#[test]
fn fix_does_not_correct_a_successful_command() {
    let dir = TempDir::new("exit0");
    std::fs::create_dir_all(dir.join("Downloads")).unwrap();
    let db = dir.join("missing.db");
    let cwd = dir.to_string_lossy().into_owned();

    let out = Command::new(bin())
        .env_remove("GHOSTLINE_IGNORE")
        .args(["fix", "--db"])
        .arg(&db)
        .args(["--cwd", &cwd, "--exit", "0", "--", "ls", "-la", "downloads"])
        .output()
        .expect("run ghostline fix");
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("did you mean"), "got: {stdout}");
}

#[test]
fn fix_last_does_not_learn_from_its_own_failure() {
    let dir = TempDir::new("last");
    let db = dir.join("history.db");
    let cwd = dir.to_string_lossy().into_owned();

    log(&db, &cwd, "0", "echo 12345");
    log(&db, &cwd, "127", "ecoh 12345");

    let out = Command::new(bin())
        .env_remove("GHOSTLINE_IGNORE")
        .args(["fix", "--last", "--db"])
        .arg(&db)
        .arg("-q")
        .output()
        .expect("run ghostline fix");
    assert!(
        out.status.success(),
        "fix failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "echo 12345",
        "the failed row must not be its own suggestion"
    );
}

fn run_fix(dir: &Path, args: &[&str]) -> std::process::Output {
    let db = std::env::temp_dir().join(format!(
        "gl-fix-cli-db-{}-{}",
        std::process::id(),
        dir.file_name().unwrap().to_string_lossy()
    ));
    Command::new(bin())
        .env_remove("GHOSTLINE_IGNORE")
        .env_remove("GHOSTLINE_QUEUE")
        .args(["fix", "--db"])
        .arg(&db)
        .args(["--cwd", &dir.to_string_lossy()])
        .args(args)
        .output()
        .expect("run ghostline fix")
}

#[test]
fn fix_mkdir_does_not_repeat_the_original_command() {
    let dir = TempDir::new("mkdir-cli");

    let out = run_fix(&dir, &["--", "mkdir", "a/b"]);
    assert!(out.status.success(), "fix failed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("mkdir -p a/b"), "got: {stdout}");
    assert!(!stdout.contains("&&"), "duplicate command: {stdout}");
}

#[test]
fn fix_cp_trailing_slash_target_creates_the_directory() {
    let dir = TempDir::new("cp-cli");
    std::fs::write(dir.join("file"), "x").unwrap();

    let out = run_fix(&dir, &["--", "cp", "file", "newdir/"]);
    assert!(out.status.success(), "fix failed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("mkdir -p newdir && cp file newdir/"),
        "got: {stdout}"
    );
}

#[test]
fn fix_cd_rejects_a_regular_file() {
    let dir = TempDir::new("cd-file");
    std::fs::write(dir.join("Downloads"), "not a directory").unwrap();

    let out = run_fix(&dir, &["--", "cd", "downloads"]);
    assert!(!out.status.success(), "file should not be suggested");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("did you mean"), "got: {stdout}");
}

#[test]
fn fix_ties_are_deterministic_across_processes() {
    let dir = TempDir::new("ties-cli");
    let bins = dir.join("bin");
    std::fs::create_dir_all(&bins).unwrap();
    for name in ["fooa", "foob"] {
        let path = bins.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let db = dir.join("missing.db");
    let mut answers = std::collections::BTreeSet::new();
    for _ in 0..8 {
        let out = Command::new(bin())
            .env_remove("GHOSTLINE_IGNORE")
            .env_remove("GHOSTLINE_QUEUE")
            .env("PATH", bins.as_os_str())
            .args(["fix", "--db"])
            .arg(&db)
            .args(["--cwd", &dir.to_string_lossy(), "-q", "--", "fooo"])
            .output()
            .expect("run ghostline fix");
        assert!(out.status.success(), "fix failed");
        answers.insert(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    assert_eq!(answers.len(), 1, "nondeterministic answers: {answers:?}");
    assert_eq!(answers.first().map(String::as_str), Some("fooa"));
}

#[test]
fn fix_rejects_malformed_or_unknown_options() {
    for args in [
        vec!["fix", "--exit", "nope"],
        vec!["fix", "--exit", "999"],
        vec!["fix", "--cwd"],
        vec!["fix", "--db"],
        vec!["fix", "-x"],
    ] {
        let out = Command::new(bin())
            .env_remove("GHOSTLINE_IGNORE")
            .args(&args)
            .output()
            .expect("run ghostline fix");
        assert!(!out.status.success(), "accepted {args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("error:"),
            "{args:?} produced no error: {stderr}"
        );
    }
}

#[test]
fn fix_rejects_last_combined_with_a_command_or_exit() {
    let dir = TempDir::new("last-conflict");
    let db = dir.join("missing.db");
    for args in [
        vec![
            "fix",
            "--last",
            "--db",
            db.to_str().unwrap(),
            "--",
            "cd",
            "x",
        ],
        vec!["fix", "--last", "--exit", "1"],
    ] {
        let out = Command::new(bin())
            .env_remove("GHOSTLINE_IGNORE")
            .args(&args)
            .output()
            .expect("run ghostline fix");
        assert!(!out.status.success(), "accepted {args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("cannot be combined"), "got: {stderr}");
    }
}

#[test]
fn fix_degrades_when_the_history_database_is_unreadable() {
    let dir = TempDir::new("bad-db");
    std::fs::create_dir_all(dir.join("Downloads")).unwrap();
    let db = dir.join("history.db");
    std::fs::write(&db, "this is not a sqlite database").unwrap();
    let cwd = dir.to_string_lossy().into_owned();

    let out = Command::new(bin())
        .env_remove("GHOSTLINE_IGNORE")
        .args(["fix", "--db"])
        .arg(&db)
        .args(["--cwd", &cwd, "--", "cd", "downlods"])
        .output()
        .expect("run ghostline fix");
    assert!(out.status.success(), "filesystem repair was aborted");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("did you mean: cd Downloads?"),
        "got: {stdout}"
    );
    assert!(
        stderr.contains("warning"),
        "missing degradation warning: {stderr}"
    );
    assert!(!stderr.contains("sensitive"), "leaked: {stderr}");

    let out = Command::new(bin())
        .env_remove("GHOSTLINE_IGNORE")
        .args(["fix", "--last", "--db"])
        .arg(&db)
        .output()
        .expect("run ghostline fix");
    assert!(!out.status.success(), "--last must require history");
    assert!(String::from_utf8_lossy(&out.stderr).contains("open db"));
}

#[test]
fn fix_does_not_advertise_unimplemented_actions() {
    let dir = TempDir::new("affordance");
    let db = dir.join("missing.db");
    let cwd = dir.to_string_lossy().into_owned();
    let out = Command::new(bin())
        .env_remove("GHOSTLINE_IGNORE")
        .args(["fix", "--db"])
        .arg(&db)
        .args(["--cwd", &cwd, "--", "mkdir", "a/b"])
        .output()
        .expect("run ghostline fix");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for unavailable in ["Enter run", "e edit", "d dismiss"] {
        assert!(!stdout.contains(unavailable), "got: {stdout}");
    }
}

#[test]
fn fix_accepts_arguments_after_the_command_word() {
    let dir = TempDir::new("args-after");
    std::fs::create_dir_all(dir.join("Downloads")).unwrap();
    let db = dir.join("missing.db");
    let cwd = dir.to_string_lossy().into_owned();
    let out = Command::new(bin())
        .env_remove("GHOSTLINE_IGNORE")
        .args(["fix", "--db"])
        .arg(&db)
        .args(["--cwd", &cwd, "ls", "-la", "downloads"])
        .output()
        .expect("run ghostline fix");
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("did you mean: ls -la Downloads?"),
        "got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
