//! Session persistence: the conversation history plus the task list,
//! serialized so a crash, quit, or slow-model interruption can be resumed.
//! Best-effort — a failure to save must never take down a turn.
//!
//! **Layout.** Each project gets its own directory,
//! `<state>/sessions/<project-key>/` (mode 0700, `<project-key>` exactly
//! the stem `session_file_path` used to name the single legacy file), and
//! each conversation within that project is its own file,
//! `<created_unix_ms>-<8 hex>.json` (mode 0600). `SessionStore` is the
//! handle onto one project's directory: `list`/`load`/`save`/`prune` and
//! `migrate_legacy` (which moves the old single-file layout in whenever
//! that legacy file exists -- not just the first time -- so a legacy
//! file that reappears, or survives an earlier failed migration, is never
//! stranded). The free `load`/`save` functions below
//! still operate on a single path each — `SessionStore` is built on top of
//! them, not a replacement for them, since existing call sites still use
//! the legacy single-file path directly (that path is now reached via
//! `sessions_root()` + `project_key()`, re-expressed so the key logic
//! exists exactly once).

use std::path::{Path, PathBuf};

use aivyx_types::{Message, Role};
pub use aivyx_types::{Task, TaskStatus};
use serde::{Deserialize, Serialize};

/// Bumped if the on-disk shape changes incompatibly; a mismatch is treated
/// as "no resumable session" (start fresh), not an error.
const SESSION_VERSION: u32 = 1;

/// Who opened a given specialist session -- the lead itself, or another
/// specialist (identified by ITS OWN session_id in the same pool).
/// `query_specialist`/`close_specialist` refuse to touch a session whose
/// `owner` doesn't match the calling `SpecialistSessionsConfig.caller`.
/// Defaults to `Lead` (via `#[serde(default)]` on the fields that use
/// it) so a `PersistedSpecialistSession` written before this feature
/// existed still loads correctly -- every session persisted then was
/// necessarily lead-opened, since specialists couldn't open sessions
/// before now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SessionOwner {
    #[default]
    Lead,
    Specialist(String),
}

/// One parked specialist session's persisted state -- just enough to
/// rebuild it: which member it is, its own conversation history, and who
/// opened it. Unlike `SessionState` (the lead's own persistence format),
/// there's no `last_active`: a dehydrated session doesn't expire from
/// inactivity, since nothing is consuming resources while it sits as
/// inert JSON -- only a *live* session (rebuilt via `query_specialist`)
/// is subject to the idle-timeout eviction `SpecialistSessionPool`
/// already has.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSpecialistSession {
    pub session_id: String,
    pub member: String,
    pub history: Vec<Message>,
    #[serde(default)]
    pub owner: SessionOwner,
}

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
    /// Every specialist session that was open (live or already
    /// dehydrated from an earlier restart) when this file was last
    /// saved. `#[serde(default)]` so a session file written before this
    /// field existed still loads -- as an empty list, the only behavior
    /// possible then. Seeded into `SpecialistSessionPool`'s dehydrated
    /// map at `--resume` time; each entry stays inert until
    /// `query_specialist` rebuilds it, or `close_specialist` discards it
    /// unused.
    #[serde(default)]
    pub specialist_sessions: Vec<PersistedSpecialistSession>,
    /// The `/undo` ledger: which turns changed files, and where to rewind
    /// each one to. `#[serde(default)]` so a session file written before
    /// this field existed still loads — as an empty ledger, the only state
    /// possible then (there's nothing to backfill: earlier checkpoint refs
    /// may already be pruned by the time this field shipped).
    #[serde(default)]
    pub undo: crate::undo::UndoLedger,
    /// Notes for the model about what the user did between turns (each
    /// `/undo` or `/redo`), not yet delivered: they prefix the next user
    /// message. `#[serde(default)]` so older session files load with none.
    #[serde(default)]
    pub pending_notes: Vec<String>,
    /// This conversation's header: id, timestamps, and a short preview --
    /// everything `/sessions` needs without loading (and deserializing)
    /// the full history of every other conversation in the project.
    /// `#[serde(default)]` so a session file written before `SessionMeta`
    /// existed still loads, as `SessionMeta::default()` (an empty id) --
    /// the legacy single-file layout never had one, since there was only
    /// ever one file per project and nothing to tell apart.
    #[serde(default)]
    pub meta: SessionMeta,
}

impl SessionState {
    pub fn new(
        history: Vec<Message>,
        tasks: Vec<Task>,
        plan_mode_active: bool,
        specialist_sessions: Vec<PersistedSpecialistSession>,
    ) -> Self {
        Self {
            version: SESSION_VERSION,
            history,
            tasks,
            plan_mode_active,
            specialist_sessions,
            undo: crate::undo::UndoLedger::default(),
            pending_notes: Vec::new(),
            meta: SessionMeta::default(),
        }
    }
}

/// One conversation's header -- enough for `/sessions` to list it without
/// loading the full history. `#[serde(default)]` on `SessionState::meta`
/// means an empty `id` (the `Default` value) marks a file that predates
/// this field, or a legacy single-file session not yet migrated.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub created_unix: i64,
    pub updated_unix: i64,
    pub first_user_text: String,
    pub turns: usize,
    /// How many times this conversation's file has been written. Each
    /// save writes one more than the revision the saving agent last
    /// loaded or wrote, so an agent that finds a higher revision on disk
    /// than it knows about can tell another process wrote the file in
    /// the meantime (see `Agent::persist`). `#[serde(default)]` so files
    /// written before this field existed load as revision 0.
    #[serde(default)]
    pub revision: u64,
}

/// How many conversations `SessionStore::prune` keeps per project, newest
/// by `updated_unix` first.
pub const SESSIONS_KEPT: usize = 20;

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
    let root = sessions_root()?;
    let key = project_key(cwd);
    Some(root.join(format!("{key}.json")))
}

/// The stable per-project key used both by the legacy single file
/// (`<key>.json`) and the new per-project directory (`<key>/`): the
/// canonicalized `cwd`'s last path component (human-readable, sanitized,
/// truncated), plus a hash of the full canonicalized path so distinct
/// directories that happen to share a last component never collide.
/// Unchanged from `session_file_path`'s own key logic before this
/// function existed -- re-expressing it here keeps that logic in exactly
/// one place.
pub fn project_key(cwd: &Path) -> String {
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

    format!(
        "{name}-{:016x}",
        fnv1a(canonical.to_string_lossy().as_bytes())
    )
}

/// `<state>/sessions`, the directory both the legacy single file
/// (`<key>.json`) and the new per-project directories (`<key>/`) live
/// under. `None` when no home/state directory can be determined at all.
pub fn sessions_root() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "aivyx-coder")?;
    let state_dir = dirs
        .state_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dirs.data_local_dir().to_path_buf());
    Some(state_dir.join("sessions"))
}

/// A short, single-line preview of a message's text for `/sessions`
/// listings: strips a leading note block (text produced by `/undo`/
/// `/redo` prefixing, which starts with `(` and contains `)\n\n`),
/// collapses whitespace runs (newlines included) to single spaces, and
/// caps at 60 chars, appending `…` when cut.
pub fn preview_text(text: &str) -> String {
    let stripped = if text.starts_with('(') {
        match text.find(")\n\n") {
            Some(idx) => &text[idx + ")\n\n".len()..],
            None => text,
        }
    } else {
        text
    };

    let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");

    const MAX: usize = 60;
    if collapsed.chars().count() > MAX {
        let truncated: String = collapsed.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        collapsed
    }
}

/// How many turns a conversation has had so far -- the number of
/// `Role::User` messages in its history (each user message starts one
/// turn; the assistant's reply and any tool round-trips complete it).
pub fn count_turns(history: &[Message]) -> usize {
    history.iter().filter(|m| m.role == Role::User).count()
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
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
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
/// Atomic: the JSON is written in full to a sibling temp file
/// (`<path>.tmp-<pid>`, so two processes racing on the same path -- e.g.
/// `aivyx-coder` and a specialist, or two instances started in the same
/// project -- never collide on the temp name), `fsync`'d, then `rename`d
/// over `path`. A reader (`load`) therefore never observes a partially
/// written file, and a crash mid-write leaves the previous contents (or
/// nothing) rather than a truncated one -- unlike the previous
/// truncate-in-place implementation, which had a real window where a
/// concurrent `load` (or a crash) could observe a half-written file.
///
/// The temp file is opened with mode `0600` set at `open()` time (via
/// `OpenOptions::mode`, Unix-only) rather than written with the process's
/// default umask and then `chmod`'d afterward -- the latter has a real
/// window where the file is observable at a wider mode, plus (as it was
/// previously written here) a silent failure mode if the `chmod` itself
/// errors. `rename` preserves the temp file's mode, so the final file is
/// `0600` too. Any I/O error -- including a failure to apply the mode --
/// now propagates as a real `Err` instead of being discarded.
pub fn save(path: &Path, state: &SessionState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let tmp_path = path.with_file_name(format!(
        "{}.tmp-{}",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));

    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?
    };
    #[cfg(not(unix))]
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp_path)?;

    use std::io::Write;
    let write_result = file.write_all(json.as_bytes()).and_then(|()| file.sync_all());
    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    drop(file);

    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

/// Creates `dir` (and any missing parents) if it doesn't exist, and sets
/// its mode to `0700` either way -- shared by `save` (the file's parent
/// directory) and `SessionStore::migrate_legacy` (the store's directory,
/// which may or may not already exist by the time migration runs).
fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// A handle onto one project's conversation directory
/// (`<state>/sessions/<project-key>/`). Each conversation is its own
/// `<id>.json` file (`id` = `<created_unix_ms>-<8 hex>`); `save`/`load`
/// address a conversation by its `meta.id`/`id`, `list` enumerates every
/// conversation's header without loading full histories unnecessarily
/// (it still loads each whole file today -- headers aren't split into
/// separate files -- but callers only see the `SessionMeta`), and `prune`
/// deletes everything past the newest `SESSIONS_KEPT`.
#[derive(Debug, Clone)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The store for `cwd`'s project, rooted at `sessions_root()/<project_key>`.
    /// `None` under the same conditions `sessions_root()` returns `None`.
    pub fn for_project(cwd: &Path) -> Option<Self> {
        let root = sessions_root()?;
        Some(Self::new(root.join(project_key(cwd))))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A new conversation id: `<now_ms>-<8 hex>`, where the hex suffix is
    /// derived from the pid and a monotonic clock reading so two ids
    /// minted in the same millisecond (even in the same process) still
    /// differ -- there's no other source of entropy available here, and
    /// collisions would silently merge two unrelated conversations into
    /// one file.
    pub fn new_id(now_ms: i64) -> String {
        let salt = format!(
            "{now_ms}-{}-{:?}",
            std::process::id(),
            std::time::Instant::now()
        );
        let suffix = (fnv1a(salt.as_bytes()) & 0xffff_ffff) as u32;
        format!("{now_ms}-{suffix:08x}")
    }

    /// The path a conversation with this id would be saved at: not
    /// guaranteed to exist.
    pub fn path_for(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Every conversation's header, newest `updated_unix` first (ties
    /// broken by `id` descending, so two conversations saved in the same
    /// second still sort deterministically). A file that doesn't parse as
    /// a `SessionState` -- including any lingering `*.tmp-<pid>` file, by
    /// extension alone, not by attempting to load it -- is skipped rather
    /// than failing the whole listing. A loaded file whose `meta.id` is
    /// empty (predates `SessionMeta`, or a hand-copied file) takes its id
    /// from the file's own stem instead, so it's still addressable by
    /// `load`/`path_for`.
    pub fn list(&self) -> Vec<SessionMeta> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };

        let mut metas: Vec<SessionMeta> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .filter_map(|path| {
                let mut meta = load(&path)?.meta;
                if meta.id.is_empty() {
                    meta.id = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                }
                Some(meta)
            })
            .collect();

        metas.sort_by(|a, b| b.updated_unix.cmp(&a.updated_unix).then_with(|| b.id.cmp(&a.id)));
        metas
    }

    /// Loads the conversation `id` names. A loaded `meta.id` of `""`
    /// (a file that predates `SessionMeta`, or a hand-copied file) is
    /// filled in with the requested `id` -- the same substitution `list()`
    /// already makes from the file's stem, applied here from the caller's
    /// own request instead, so a loaded state's `meta.id` always names the
    /// file it actually came from. This matters to callers like
    /// `Agent::restore_session`, which trusts `state.meta.id` to decide
    /// whether it's resuming a real, addressable conversation or starting
    /// fresh -- an empty id there would be misread as the latter even
    /// though a real file, at a real path, was just loaded.
    pub fn load(&self, id: &str) -> Option<SessionState> {
        let mut state = load(&self.path_for(id))?;
        if state.meta.id.is_empty() {
            state.meta.id = id.to_string();
        }
        Some(state)
    }

    /// Just the header of conversation `id`, or `None` when its file is
    /// missing or unusable. Loads the whole file (headers aren't stored
    /// separately) but hands back only `meta`, with the same empty-id
    /// substitution `load` makes.
    pub fn load_meta(&self, id: &str) -> Option<SessionMeta> {
        self.load(id).map(|state| state.meta)
    }

    /// Saves `state` to the path its own `meta.id` names -- callers set
    /// `state.meta.id` (and the rest of `meta`) before calling this, the
    /// same way the free `save(path, state)` takes its path from the
    /// caller rather than inferring one.
    pub fn save(&self, state: &SessionState) -> std::io::Result<()> {
        save(&self.path_for(&state.meta.id), state)
    }

    /// Deletes every conversation past the newest `keep` (by `list`'s
    /// order). A failure to delete one file is swallowed and pruning
    /// continues with the rest -- this runs after every persist and must
    /// never turn into a reason a turn fails.
    pub fn prune(&self, keep: usize) {
        for meta in self.list().into_iter().skip(keep) {
            let _ = std::fs::remove_file(self.path_for(&meta.id));
        }
    }

    /// One-time-per-reappearance migration from the legacy single-file
    /// layout (`<project-key>.json`) into this store's directory. Does
    /// nothing (`Ok(false)`) unless `legacy` is a regular file --
    /// deliberately *not* conditioned on whether `self.dir` already
    /// exists.
    ///
    /// Earlier, this also required `!self.dir.exists()`, on the
    /// assumption that the directory's existence alone proved migration
    /// had already happened. That assumption broke if `self.save` below
    /// ever failed after `self.dir` was created (disk full, permissions):
    /// the legacy file would survive, but every later call would see the
    /// directory already there and return `Ok(false)` forever --
    /// stranding that conversation for good. Checking only `legacy`
    /// itself fixes that (a failed save simply gets retried next time,
    /// since the legacy file is still there to find), and also means a
    /// legacy file that *reappears* after migration -- e.g. an older
    /// aivyx-coder build, or an ACP process still on one, writing it again
    /// -- gets migrated too, instead of being silently ignored because
    /// the directory already exists. The cost is a theoretical duplicate
    /// entry if a save succeeds but the subsequent `remove_file` fails;
    /// that's acceptable, since nothing is lost, unlike the stranding
    /// above.
    ///
    /// A `legacy` file that parses as a `SessionState` is loaded, given a
    /// derived `SessionMeta` (timestamps from the file's own mtime, since
    /// the legacy format never recorded them; `first_user_text` and
    /// `turns` from its history), saved into the store, and then removed.
    /// A `legacy` file that *doesn't* parse is not discarded -- its raw
    /// bytes are renamed into the store directory unchanged, under a
    /// mtime-derived id, so a corrupt-but-maybe-recoverable file is never
    /// silently lost.
    pub fn migrate_legacy(&self, legacy: &Path) -> std::io::Result<bool> {
        if !legacy.is_file() {
            return Ok(false);
        }

        let metadata = std::fs::metadata(legacy)?;
        let mtime = metadata
            .modified()
            .unwrap_or_else(|_| std::time::SystemTime::now());
        let mtime_ms = mtime
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let mtime_secs = mtime_ms / 1000;
        let id = Self::new_id(mtime_ms);

        ensure_private_dir(&self.dir)?;

        match load(legacy) {
            Some(mut state) => {
                state.meta = SessionMeta {
                    id: id.clone(),
                    created_unix: mtime_secs,
                    updated_unix: mtime_secs,
                    first_user_text: state
                        .history
                        .iter()
                        .find(|m| m.role == Role::User)
                        .map(|m| preview_text(&m.text_content()))
                        .unwrap_or_default(),
                    turns: count_turns(&state.history),
                    revision: 0,
                };
                self.save(&state)?;
                std::fs::remove_file(legacy)?;
            }
            None => {
                let dest = self.path_for(&id);
                std::fs::rename(legacy, &dest)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600))?;
                }
            }
        }

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            vec![PersistedSpecialistSession {
                session_id: "abc-123".to_string(),
                member: "implementer".to_string(),
                history: vec![Message::text(Role::User, "implement the thing")],
                owner: SessionOwner::Specialist("orchestrator-session-id".to_string()),
            }],
        );

        save(&path, &state).unwrap();
        let loaded = load(&path).expect("should load what we just saved");

        assert_eq!(loaded.history.len(), 1);
        assert_eq!(loaded.history[0].text_content(), "hello");
        assert_eq!(loaded.tasks, state.tasks);
        assert!(loaded.plan_mode_active);
        assert_eq!(loaded.specialist_sessions.len(), 1);
        assert_eq!(loaded.specialist_sessions[0].session_id, "abc-123");
        assert_eq!(loaded.specialist_sessions[0].member, "implementer");
        assert_eq!(loaded.specialist_sessions[0].history.len(), 1);
        assert_eq!(
            loaded.specialist_sessions[0].history[0].text_content(),
            "implement the thing"
        );
        assert_eq!(
            loaded.specialist_sessions[0].owner,
            SessionOwner::Specialist("orchestrator-session-id".to_string())
        );
    }

    #[test]
    fn a_persisted_specialist_session_predating_owner_still_loads_as_lead_owned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "history": [],
                "tasks": [],
                "specialist_sessions": [
                    { "session_id": "old-one", "member": "implementer", "history": [] }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let loaded = load(&path).expect("pre-existing-field-free session should still load");

        assert_eq!(loaded.specialist_sessions.len(), 1);
        assert_eq!(loaded.specialist_sessions[0].owner, SessionOwner::Lead);
    }

    #[test]
    fn a_session_file_predating_specialist_sessions_still_loads_with_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(
            &path,
            serde_json::json!({ "version": 1, "history": [], "tasks": [] }).to_string(),
        )
        .unwrap();

        let loaded = load(&path).expect("pre-existing-field-free session should still load");

        assert!(loaded.specialist_sessions.is_empty());
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
        save(&path, &SessionState::new(vec![], vec![], false, vec![])).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // A once-planned second test here bracketed the process umask
    // (`libc::umask(0o022)` around the `save` call, restored after) to try
    // to exercise the write-time window under a permissive umask, rather
    // than just the final mode `saved_file_is_owner_only` above checks.
    // Removed (Task 9 review) for two independent reasons, either one
    // sufficient on its own:
    //
    // 1. It doesn't actually test anything `saved_file_is_owner_only`
    //    doesn't already cover: `open()`'s `mode(0o600)` argument sets an
    //    *absolute* mode, not one relative to umask, and umask can only
    //    *clear* bits from a requested mode -- `0o600` has no group/other
    //    bits to clear. So the ambient umask during the call is provably
    //    irrelevant to the outcome; even the old, vulnerable write-then-
    //    chmod code would have passed this exact assertion by the time
    //    `save` returned (chmod also sets an absolute mode). It bought no
    //    real regression coverage.
    // 2. Process umask is genuinely global, mutable, per-process state --
    //    this exact pattern (`libc::umask` bracketed around one call, no
    //    synchronization) already broke a real, unrelated, pre-existing
    //    test in the sibling `aivyx-pa` repo's equivalent security-audit
    //    fix (Task 7, daemon socket bind), reproduced and root-caused to
    //    racing against concurrent filesystem-touching tests in the same
    //    binary. This crate's own test suite has dozens of concurrent
    //    `tempfile`/`fs::write` call sites in the same binary and no test
    //    isolation config, so the same class of flake was live here too --
    //    it simply hadn't surfaced yet, since this machine's ambient umask
    //    already happened to equal the bracketed value.
    //
    // The atomicity guarantee (no window at a wider mode, ever, regardless
    // of ambient umask) is structural -- `mode(0o600)` is passed straight
    // to `open(2)`'s `O_CREAT` argument -- and is verified by code
    // inspection, not by a umask-varying test. `saved_file_is_owner_only`
    // above remains the correct, sufficient regression test: it proves the
    // resulting mode is exactly `0o600`, which is all `open()`-time mode
    // assignment can ever produce.

    fn meta_state(id: &str, updated: i64, first: &str) -> SessionState {
        let mut s = SessionState::new(vec![Message::text(Role::User, first)], vec![], false, vec![]);
        s.meta = SessionMeta {
            id: id.into(),
            created_unix: updated,
            updated_unix: updated,
            first_user_text: first.into(),
            turns: 1,
            revision: 0,
        };
        s
    }

    #[test]
    fn store_lists_newest_first_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().join("proj"));
        store.save(&meta_state("1000-aaaaaaaa", 10, "old")).unwrap();
        store.save(&meta_state("2000-bbbbbbbb", 30, "newest")).unwrap();
        store.save(&meta_state("1500-cccccccc", 20, "middle")).unwrap();
        let ids: Vec<String> = store.list().into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec!["2000-bbbbbbbb", "1500-cccccccc", "1000-aaaaaaaa"]);
        let loaded = store.load("1500-cccccccc").unwrap();
        assert_eq!(loaded.history[0].text_content(), "middle");
    }

    #[cfg(unix)]
    #[test]
    fn store_files_are_0600_in_a_0700_dir_and_leave_no_temp_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().join("proj"));
        store.save(&meta_state("1-aaaaaaaa", 1, "x")).unwrap();
        store.save(&meta_state("1-aaaaaaaa", 2, "x")).unwrap();
        let dir_mode = std::fs::metadata(store.dir()).unwrap().permissions().mode() & 0o777;
        let file_mode = std::fs::metadata(store.path_for("1-aaaaaaaa")).unwrap().permissions().mode() & 0o777;
        assert_eq!((dir_mode, file_mode), (0o700, 0o600));
        let names: Vec<String> = std::fs::read_dir(store.dir()).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["1-aaaaaaaa.json".to_string()]);
    }

    #[test]
    fn prune_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().join("proj"));
        for i in 0..25 {
            store.save(&meta_state(&format!("{i}-00000000"), i, "t")).unwrap();
        }
        store.prune(SESSIONS_KEPT);
        let list = store.list();
        assert_eq!(list.len(), 20);
        assert_eq!(list[0].updated_unix, 24);
        assert_eq!(list[19].updated_unix, 5);
    }

    #[test]
    fn migrate_moves_the_legacy_file_in_and_derives_its_header() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("proj-0000000000000001.json");
        let old = SessionState::new(
            vec![
                Message::text(Role::User, "(You undid the last turn.)\n\nthe tests in\n  test_stats.py fail"),
                Message::text(Role::Assistant, "ok"),
                Message::text(Role::User, "again"),
            ],
            vec![],
            false,
            vec![],
        );
        save(&legacy, &old).unwrap();
        let store = SessionStore::new(dir.path().join("proj-0000000000000001"));
        assert!(store.migrate_legacy(&legacy).unwrap());
        assert!(!legacy.exists());
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].first_user_text, "the tests in test_stats.py fail");
        assert_eq!(list[0].turns, 2);
        assert!(list[0].updated_unix > 0);
        assert_eq!(store.load(&list[0].id).unwrap().history.len(), 3);
        // Second run: the legacy file is gone (migrated away above), so
        // there's nothing left to migrate -- Ok(false), regardless of the
        // directory now existing.
        assert!(!store.migrate_legacy(&legacy).unwrap());
    }

    #[test]
    fn migrate_runs_again_if_the_legacy_file_reappears_even_though_the_directory_exists() {
        // An older aivyx-coder build (or an ACP process still on one) can
        // write the legacy single file again after this project's
        // directory already exists. Migration must still pick it up --
        // not strand it forever just because `self.dir` is no longer
        // empty -- and the result is both conversations listed, not one
        // silently dropped.
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("proj.json");
        let store = SessionStore::new(dir.path().join("proj"));
        store.save(&meta_state("1-aaaaaaaa", 1, "existing")).unwrap();

        let reappeared = SessionState::new(
            vec![Message::text(Role::User, "a reappeared legacy conversation")],
            vec![],
            false,
            vec![],
        );
        save(&legacy, &reappeared).unwrap();

        assert!(store.migrate_legacy(&legacy).unwrap());
        assert!(!legacy.exists());

        let list = store.list();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|m| m.id == "1-aaaaaaaa" && m.first_user_text == "existing"));
        assert!(
            list.iter()
                .any(|m| m.first_user_text == "a reappeared legacy conversation")
        );
    }

    #[test]
    fn migrate_keeps_an_unparseable_legacy_file() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("p.json");
        std::fs::write(&legacy, "{not json").unwrap();
        let store = SessionStore::new(dir.path().join("p"));
        assert!(store.migrate_legacy(&legacy).unwrap());
        assert!(!legacy.exists());
        let kept: Vec<_> = std::fs::read_dir(store.dir()).unwrap().collect();
        assert_eq!(kept.len(), 1, "the bytes are kept, even if unlistable");
    }

    #[test]
    fn old_files_load_with_a_default_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        std::fs::write(&path, r#"{"version":1,"history":[],"tasks":[]}"#).unwrap();
        assert_eq!(load(&path).unwrap().meta, SessionMeta::default());
    }

    #[test]
    fn preview_text_strips_notes_collapses_space_and_caps_at_60() {
        assert_eq!(preview_text("(note one) (note two)\n\nfix  the\nbug"), "fix the bug");
        let long = "x".repeat(70);
        assert_eq!(preview_text(&long), format!("{}…", "x".repeat(60)));
        assert_eq!(preview_text("(not a note) just text"), "(not a note) just text");
    }

    #[test]
    fn new_ids_sort_by_time_and_differ() {
        let a = SessionStore::new_id(1000);
        let b = SessionStore::new_id(1000);
        assert!(a.starts_with("1000-") && a.len() == "1000-".len() + 8);
        assert_ne!(a, b);
    }

    #[test]
    fn store_load_fills_in_an_empty_meta_id_from_the_requested_id() {
        // A file with no `SessionMeta` (or a hand-copied one) has
        // `meta.id == ""` on disk -- `list()` already papers over this by
        // substituting the file's stem (see its own doc comment); `load`
        // must do the same, since a caller like `restore_session` trusts
        // the returned `state.meta.id` to be the id of the file it asked
        // for, not an empty string that would be misread as "no id yet".
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().join("proj"));
        let state = SessionState::new(vec![Message::text(Role::User, "hi")], vec![], false, vec![]);
        assert!(state.meta.id.is_empty());
        save(&store.path_for("stem-id"), &state).unwrap();

        let loaded = store.load("stem-id").expect("file should load");
        assert_eq!(loaded.meta.id, "stem-id");
    }
}
