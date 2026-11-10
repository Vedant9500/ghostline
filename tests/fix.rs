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
