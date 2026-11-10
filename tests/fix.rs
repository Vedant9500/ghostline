use std::process::Command;
use std::time::Duration;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ghostline")
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
    let dir = std::env::temp_dir().join(format!("gl-fix-sensitive-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
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

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_refuses_a_sensitive_explicit_command() {
    let dir =
        std::env::temp_dir().join(format!("gl-fix-sensitive-explicit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
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

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_does_not_correct_a_successful_command() {
    let dir = std::env::temp_dir().join(format!("gl-fix-exit0-{}", std::process::id()));
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

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_last_does_not_learn_from_its_own_failure() {
    let dir = std::env::temp_dir().join(format!("gl-fix-last-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
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

    std::fs::remove_dir_all(&dir).ok();
}

fn run_fix(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
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
    let dir = std::env::temp_dir().join(format!("gl-fix-mkdir-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let out = run_fix(&dir, &["--", "mkdir", "a/b"]);
    assert!(out.status.success(), "fix failed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("mkdir -p a/b"), "got: {stdout}");
    assert!(!stdout.contains("&&"), "duplicate command: {stdout}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_cp_trailing_slash_target_creates_the_directory() {
    let dir = std::env::temp_dir().join(format!("gl-fix-cp-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("file"), "x").unwrap();

    let out = run_fix(&dir, &["--", "cp", "file", "newdir/"]);
    assert!(out.status.success(), "fix failed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("mkdir -p newdir && cp file newdir/"),
        "got: {stdout}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_cd_rejects_a_regular_file() {
    let dir = std::env::temp_dir().join(format!("gl-fix-cd-file-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Downloads"), "not a directory").unwrap();

    let out = run_fix(&dir, &["--", "cd", "downloads"]);
    assert!(!out.status.success(), "file should not be suggested");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("did you mean"), "got: {stdout}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_ties_are_deterministic_across_processes() {
    let dir = std::env::temp_dir().join(format!("gl-fix-ties-cli-{}", std::process::id()));
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

    std::fs::remove_dir_all(&dir).ok();
}
