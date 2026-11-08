//! The session file format is part of the 1.x promise (`--resume` works
//! across 1.x; docs/COMPATIBILITY.md). `fixtures/session-v1.json` is a
//! session file exactly as 1.0 writes it (`session::save`'s output, byte
//! for byte): a short conversation with a tool call and its result, a
//! task list, an `/undo` mark and a pending note. It is never regenerated:
//! every later version must still load it.

use std::path::{Path, PathBuf};

use aivyx_core::session::{self, SessionStore, TaskStatus};
use aivyx_types::{ContentBlock, Role};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/session-v1.json")
}

#[test]
fn a_v1_session_file_loads() {
    let state = session::load(&fixture()).expect("the v1 fixture must load");

    assert_eq!(state.version, 1);
    let roles: Vec<Role> = state.history.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool, Role::Assistant]);
    assert_eq!(state.history[0].text_content(), "Add a --version flag to the CLI.");

    let call = state.history[1]
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
        .expect("the assistant's tool call");
    assert_eq!(call.id.0, "call_1");
    assert_eq!(call.name, "read_file");
    assert_eq!(call.arguments["path"], "src/main.rs");
    assert_eq!(state.history[2].tool_call_id.as_ref().map(|id| id.0.as_str()), Some("call_1"));
    assert!(matches!(
        &state.history[2].content[0],
        ContentBlock::ToolResult(result) if result.call_id.0 == "call_1"
    ));

    assert_eq!(state.tasks.len(), 2);
    assert_eq!(state.tasks[0].status, TaskStatus::Done);
    assert_eq!(state.tasks[1].status, TaskStatus::InProgress);
    assert!(!state.plan_mode_active);
    assert_eq!(state.undo.marks.len(), 1);
    assert_eq!(state.pending_notes, vec!["The user ran /undo.".to_string()]);
    assert_eq!(state.meta.id, "1793145600000-0a1b2c3d");
    assert_eq!(state.meta.turns, 1);
    assert_eq!(state.meta.revision, 3);
}

#[test]
fn a_v1_session_file_resumes_from_a_store() {
    // The way `--resume` reaches it: listed, then loaded by id.
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().join("proj"));
    std::fs::create_dir_all(store.dir()).unwrap();
    std::fs::copy(fixture(), store.path_for("1793145600000-0a1b2c3d")).unwrap();

    let list = store.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].first_user_text, "Add a --version flag to the CLI.");
    let state = store.load(&list[0].id).expect("resumable");
    assert_eq!(state.history.len(), 4);
    assert!(store.unreadable().is_empty());
}
