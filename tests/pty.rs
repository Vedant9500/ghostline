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

/// Native auto-ghost via loadable readline hook: auto-paints per key with
/// ~0 full-line redraws. Builds the .so with cc when available, else skips
/// gracefully (like the python skip above). Uses a fake install dir (binary
/// + .so side by side) so the snippet takes its real pure-C path, exactly
/// like a user install. Proves auto without flicker, including fast bursts.
#[test]
fn bash_ghost_pty_auto() {
    let Some(py) = python() else {
        eprintln!("SKIP bash_ghost_pty_auto: no python3");
        return;
    };
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let mut workdir = std::env::temp_dir();
    workdir.push(format!("gl-pty-auto-{}", std::process::id()));
    std::fs::create_dir_all(&workdir).unwrap();
    let install = workdir.join("install");
    std::fs::create_dir_all(&install).unwrap();
    let so = install.join("ghostline_autosuggest.so");
    let cc = std::process::Command::new("cc")
        .args([
            "-shared",
            "-fPIC",
            "-o",
            so.to_str().unwrap(),
            &format!("{manifest}/builtin/ghostline_autosuggest.c"),
            "-I/usr/include/bash",
            "-I/usr/include/bash/include",
        ])
        .output();
    let Ok(cc_out) = cc else {
        eprintln!("SKIP bash_ghost_pty_auto: no cc");
        std::fs::remove_dir_all(&workdir).ok();
        return;
    };
    if !cc_out.status.success() || !so.exists() {
        eprintln!(
            "SKIP bash_ghost_pty_auto: cc failed: {}",
            String::from_utf8_lossy(&cc_out.stderr)
        );
        std::fs::remove_dir_all(&workdir).ok();
        return;
    }
    let bin = env!("CARGO_BIN_EXE_ghostline");
    // Fake user install: binary + .so side by side, snippet resolves it.
    #[cfg(unix)]
    std::os::unix::fs::symlink(bin, install.join("ghostline")).unwrap();
    #[cfg(not(unix))]
    std::fs::copy(bin, install.join("ghostline")).unwrap();
    let install_s = install.to_string_lossy().into_owned();
    let init = std::process::Command::new(bin)
        .arg("init")
        .arg("bash")
        .env("PATH", format!("{install_s}:{}", std::env::var("PATH").unwrap_or_default()))
        .output()
        .expect("init bash");
    assert!(init.status.success());
    let snip = workdir.join("snip.bash");
    std::fs::write(&snip, &init.stdout).unwrap();

    let script = format!("{manifest}/tests/ghost_pty_auto.py");
    let out = std::process::Command::new(py)
        .arg(&script)
        .arg(&snip)
        .arg(&install)
        .arg(&workdir)
        .arg(&so)
        .output()
        .expect("run ghost_pty_auto.py");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "ghost_pty_auto.py failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("RESULT: OK") || stdout.contains("SKIP"),
        "unexpected output:\n{stdout}"
    );
    std::fs::remove_dir_all(&workdir).ok();
}
