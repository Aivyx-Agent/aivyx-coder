//! `kill_all_process_groups` acts on a process-wide registry, so it gets a
//! test binary of its own: in the unit tests it would kill the groups of
//! every other test running alongside it.

use std::time::Duration;

use aivyx_tools::{ProcessGroup, kill_all_process_groups};

/// A group leader whose background job writes `late.txt` after a second.
fn spawn_late_writer(dir: &std::path::Path) -> tokio::process::Child {
    tokio::process::Command::new("sh")
        .arg("-c")
        .arg("(sleep 1; echo late > late.txt) & wait")
        .current_dir(dir)
        .process_group(0)
        .spawn()
        .unwrap()
}

#[tokio::test]
async fn kill_all_kills_the_groups_still_running_and_only_those() {
    // Live-test finding (2026-10-20): coder killed by SIGTERM or a terminal
    // hang-up never drops its ProcessGroups, so a running `cargo test` (and
    // a test binary stuck in a loop) outlived it indefinitely.
    let abandoned_dir = tempfile::tempdir().unwrap();
    let mut abandoned = spawn_late_writer(abandoned_dir.path());
    // As if the owner died without unwinding: the group is never dropped.
    std::mem::forget(ProcessGroup::of(&abandoned));

    let finished_dir = tempfile::tempdir().unwrap();
    let mut finished = spawn_late_writer(finished_dir.path());
    let mut finished_group = ProcessGroup::of(&finished);
    finished_group.kill();
    finished.wait().await.unwrap();

    kill_all_process_groups();

    tokio::time::timeout(Duration::from_secs(5), abandoned.wait())
        .await
        .expect("the abandoned group's leader must be killed")
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !abandoned_dir.path().join("late.txt").exists(),
        "the abandoned group's background job outlived kill_all_process_groups"
    );
    assert!(!finished_dir.path().join("late.txt").exists());
}
