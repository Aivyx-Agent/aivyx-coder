use std::process::Command;
use tempfile::tempdir;

/// In headless mode an error that stops the run from starting must exit 3
/// ("an error"); status 1 is reserved for "goal achieved, a check failed".
#[test]
fn headless_setup_error_exits_3() {
    let cwd = tempdir().unwrap();
    let config_home = tempdir().unwrap();
    let dir = config_home.path().join("aivyx-coder");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), "this is [not valid toml").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_aivyx-coder"))
        .args(["--auto", "x", "--headless"])
        .current_dir(cwd.path())
        .env("XDG_CONFIG_HOME", config_home.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "stderr: {stderr}");
    assert!(stderr.contains("Error:"), "stderr: {stderr}");
}
