//! Session persistence: the conversation history plus the task list,
//! serialized so a crash, quit, or slow-model interruption can be resumed.
//! Best-effort — a failure to save must never take down a turn.

use std::path::{Path, PathBuf};

use aivyx_types::Message;
pub use aivyx_types::{Task, TaskStatus};
use serde::{Deserialize, Serialize};

/// Bumped if the on-disk shape changes incompatibly; a mismatch is treated
/// as "no resumable session" (start fresh), not an error.
const SESSION_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub version: u32,
    pub history: Vec<Message>,
    pub tasks: Vec<Task>,
    /// Whether Plan mode was active when this session was last persisted.
    /// `--resume` restores it (`Agent::restore`) so quitting mid-review
    /// doesn't silently drop back into Act mode on the next run.
    /// `#[serde(default)]` so a session file written before this field
    /// existed still loads — as `false`, the only behavior possible then.
    #[serde(default)]
    pub plan_mode_active: bool,
}

impl SessionState {
    pub fn new(history: Vec<Message>, tasks: Vec<Task>, plan_mode_active: bool) -> Self {
        Self {
            version: SESSION_VERSION,
            history,
            tasks,
            plan_mode_active,
        }
    }
}

/// Where this project's session is persisted: one file per project
/// directory, keyed by a stable hash of the canonicalized `cwd` (plus its
/// last component for human readability), under the platform state dir
/// (`~/.local/state/aivyx-coder/sessions/` on Linux). Deliberately *not*
/// inside the project itself — the session contains file contents and
/// command output read during the conversation, which must not end up
/// committed to the project's own git history.
///
/// `None` when no home/state directory can be determined at all.
pub fn session_file_path(cwd: &Path) -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "aivyx-coder")?;
    let state_dir = dirs
        .state_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dirs.data_local_dir().to_path_buf());

    // Canonicalize so `/home/u/proj` and `/home/u/./proj` (or a symlinked
    // path) key to the same session; fall back to the raw path if the
    // directory can't be canonicalized rather than failing resume outright.
    let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let name: String = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".to_string())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(40)
        .collect();

    let key = format!(
        "{name}-{:016x}",
        fnv1a(canonical.to_string_lossy().as_bytes())
    );
    Some(state_dir.join("sessions").join(format!("{key}.json")))
}

/// Where cross-session memory (`memory_write`/`memory_read`/
/// `memory_forget`, via `aivyx-recall`'s `FileRecall`) is persisted: one
/// shared directory under the platform state dir, parallel to
/// `sessions/`. Unlike `session_file_path`, this directory isn't itself
/// project-keyed — `aivyx-tools`' topic-rewriting layer embeds a
/// project-scoping hash *inside* the topic string for `project:`-prefixed
/// topics instead (see `crates/aivyx-tools/src/memory_topic.rs`), so all
/// topics — global and per-project alike — share one directory of
/// per-topic files.
pub fn memory_dir_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "aivyx-coder")?;
    let state_dir = dirs
        .state_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dirs.data_local_dir().to_path_buf());
    Some(state_dir.join("memory"))
}

/// FNV-1a, inlined because the session key must be stable across program
/// versions — `std`'s `DefaultHasher` explicitly does not guarantee that.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Loads a resumable session, or `None` if there's nothing usable to resume
/// (no file, unreadable, unparseable, or an unrecognized version) — every
/// one of those is a "start fresh" case, not a hard error.
pub fn load(path: &Path) -> Option<SessionState> {
    let raw = std::fs::read_to_string(path).ok()?;
    let state: SessionState = serde_json::from_str(&raw).ok()?;
    (state.version == SESSION_VERSION).then_some(state)
}

/// Writes the session owner-only (it can contain file contents / command
/// output read during the session, so it's treated as sensitive like
/// `config.toml`). Best-effort at the call site.
///
/// The file is opened with mode `0600` set at `open()` time (via
/// `OpenOptions::mode`, Unix-only) rather than written with the process's
/// default umask and then `chmod`'d afterward -- the latter has a real
/// window where the file is observable at a wider mode, plus (as it was
/// previously written here) a silent failure mode if the `chmod` itself
/// errors. Any I/O error -- including a failure to apply the mode -- now
/// propagates as a real `Err` instead of being discarded.
pub fn save(path: &Path, state: &SessionState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?
    };
    #[cfg(not(unix))]
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;

    use std::io::Write;
    file.write_all(json.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_types::Role;

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        let state = SessionState::new(
            vec![Message::text(Role::User, "hello")],
            vec![Task {
                id: 1,
                text: "do the thing".to_string(),
                status: TaskStatus::InProgress,
            }],
            true,
        );

        save(&path, &state).unwrap();
        let loaded = load(&path).expect("should load what we just saved");

        assert_eq!(loaded.history.len(), 1);
        assert_eq!(loaded.history[0].text_content(), "hello");
        assert_eq!(loaded.tasks, state.tasks);
        assert!(loaded.plan_mode_active);
    }

    #[test]
    fn a_session_file_predating_plan_mode_active_still_loads_as_act_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(
            &path,
            serde_json::json!({ "version": 1, "history": [], "tasks": [] }).to_string(),
        )
        .unwrap();

        let loaded = load(&path).expect("pre-existing-field-free session should still load");

        assert!(!loaded.plan_mode_active);
    }

    #[test]
    fn missing_file_is_a_fresh_start_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("nope.json")).is_none());
    }

    #[test]
    fn an_unrecognized_version_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(
            &path,
            serde_json::json!({ "version": 999, "history": [], "tasks": [] }).to_string(),
        )
        .unwrap();
        assert!(load(&path).is_none());
    }

    #[test]
    fn session_path_is_stable_and_distinct_per_directory() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();

        let a1 = session_file_path(dir_a.path()).expect("state dir should exist in tests");
        let a2 = session_file_path(dir_a.path()).unwrap();
        let b = session_file_path(dir_b.path()).unwrap();

        assert_eq!(a1, a2, "same directory must key to the same session file");
        assert_ne!(a1, b, "different directories must not collide");
        assert!(a1.extension().is_some_and(|e| e == "json"));
    }

    #[test]
    fn session_path_survives_a_non_canonical_spelling_of_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        let dotted = dir.path().join(".");

        assert_eq!(
            session_file_path(dir.path()).unwrap(),
            session_file_path(&dotted).unwrap()
        );
    }

    #[test]
    fn memory_dir_path_is_a_memory_subdirectory_of_the_state_dir() {
        let path = memory_dir_path().expect("should resolve on any platform with a home dir");
        assert_eq!(path.file_name().unwrap(), "memory");
    }

    #[test]
    fn memory_dir_path_is_a_sibling_of_the_sessions_dir() {
        let memory = memory_dir_path().unwrap();
        let sessions = session_file_path(&std::env::current_dir().unwrap())
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        assert_eq!(memory.parent(), sessions.parent());
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        save(&path, &SessionState::new(vec![], vec![], false)).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// Unlike `saved_file_is_owner_only`, this forces a permissive umask
    /// (`0o022`, i.e. what a plain `std::fs::write` + separate `chmod`
    /// would briefly leave the file at) before saving, so it actually
    /// exercises the write-time window rather than just the final mode --
    /// a `write` + racy `chmod` could still pass the other test by the time
    /// `save` returns.
    #[cfg(unix)]
    #[test]
    fn session_file_is_never_observable_at_a_wider_mode_than_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");

        // SAFETY: umask is process-global; no other thread in this test
        // binary touches file creation permissions concurrently with the
        // single `save` call below.
        let old_umask = unsafe { libc::umask(0o022) };
        let result = save(&path, &SessionState::new(vec![], vec![], false));
        unsafe {
            libc::umask(old_umask);
        }
        result.unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "session file must never be created at a mode wider than 0600, \
             even under a permissive umask"
        );
    }
}
