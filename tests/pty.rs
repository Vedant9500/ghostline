//! Pty end-to-end: real keypresses through a real bash+readline.
//! Catches readline macro recursion, which scripted function tests cannot.
//! Skips gracefully when python3 is unavailable.

fn python() -> Option<String> {
    for cand in ["python3", "python"] {
        if std::process::Command::new(cand).arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
            return Some(cand.into());
        }
    }
    None
}

#[test]
fn bash_ghost_pty() {
    let Some(py) = python() else {
        eprintln!("SKIP bash_ghost_pty: no python3");
        return;
    };
    let bin = env!("CARGO_BIN_EXE_ghostline");
    let bindir = std::path::Path::new(bin).parent().unwrap().to_string_lossy().into_owned();
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let mut workdir = std::env::temp_dir();
    workdir.push(format!("gl-pty-{}", std::process::id()));
    std::fs::create_dir_all(&workdir).unwrap();

    // fresh snippet from the binary under test
    let init = std::process::Command::new(bin).arg("init").arg("bash").output().expect("init bash");
    assert!(init.status.success());
    let snip = workdir.join("snip.bash");
    std::fs::write(&snip, &init.stdout).unwrap();

    let script = format!("{manifest}/tests/ghost_pty.py");
    let out = std::process::Command::new(py)
        .arg(&script)
        .arg(&snip)
        .arg(&bindir)
        .arg(&workdir)
        .output()
        .expect("run ghost_pty.py");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "ghost_pty.py failed:\n{stdout}\n{stderr}");
    assert!(stdout.contains("RESULT: OK"), "unexpected output:\n{stdout}");
    std::fs::remove_dir_all(&workdir).ok();
}
