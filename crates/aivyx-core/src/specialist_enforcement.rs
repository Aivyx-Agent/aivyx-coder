//! Builds a specialist-scoped `PermissionGate`/`ExecutionConfiner` pair —
//! see `docs/superpowers/specs/2026-09-21-specialist-deny-paths-attenuation-design.md`.
//! Before this module existed, `delegate_to_specialist`/`spawn_specialist`
//! passed the lead's own shared gate/confiner straight through to a
//! specialist's `Agent`, so `TeamMember.extra_deny_paths` (part of the
//! schema since `aivyx-team`'s Foundation phase) was never actually
//! enforced anywhere.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aivyx_sandbox::{
    AutonomousMode, ConfirmationGate, ExecutionConfiner, InjectionTaint, PermissionGate,
    PermissionPrompter, PlanMode,
};

/// Everything needed to build a fresh, correctly-configured
/// `ConfirmationGate`/`ExecutionConfiner` pair, mirroring exactly what
/// `agent_builder.rs` already assembles once for the lead's own gate —
/// constructed once per process there, cloned cheaply (every field is an
/// `Arc`, a small `Vec`, or a `bool`) into each specialist-delegation
/// tool's config.
#[derive(Clone)]
pub struct SpecialistEnforcementIngredients {
    pub prompter: Arc<dyn PermissionPrompter>,
    /// The lead's own resolved `deny_paths` — unioned with each
    /// specialist's own `extra_deny_paths` at call time, never mutated
    /// here.
    pub base_deny_paths: Vec<PathBuf>,
    pub pre_approved_commands: Vec<(String, Vec<String>)>,
    pub plan_mode: PlanMode,
    pub autonomous_mode: AutonomousMode,
    pub editor_approval_enabled: bool,
    pub injection_taint: InjectionTaint,
    pub extra_read_paths: Vec<PathBuf>,
    pub require_enforcement: bool,
}

/// Builds a fresh `ConfirmationGate` + `ExecutionConfiner` pair scoped to
/// `member`'s own effective deny-paths (the lead's `base_deny_paths`
/// unioned with `member.extra_deny_paths`, deduplicated) — everything
/// else (`prompter`, `plan_mode`, `autonomous_mode`, `injection_taint`,
/// `pre_approved_commands`, `editor_approval_enabled`) matches the lead's
/// own construction exactly. `cwd` is taken per-call (from the caller's
/// own `ToolExecutionContext`) rather than stored in
/// `SpecialistEnforcementIngredients`, since it's already available at
/// every real call site and storing a second copy risks staleness.
pub fn scoped_gate_and_confiner(
    ingredients: &SpecialistEnforcementIngredients,
    member: &aivyx_team::TeamMember,
    cwd: &Path,
) -> (Arc<dyn PermissionGate>, Arc<dyn ExecutionConfiner>) {
    let mut deny_paths = ingredients.base_deny_paths.clone();
    for resolved in resolve_tilde_paths(&member.extra_deny_paths) {
        if !deny_paths.contains(&resolved) {
            deny_paths.push(resolved);
        }
    }

    let gate: Arc<dyn PermissionGate> = Arc::new(
        ConfirmationGate::new(
            Arc::clone(&ingredients.prompter),
            deny_paths.clone(),
            ingredients.pre_approved_commands.clone(),
            ingredients.plan_mode.clone(),
            ingredients.autonomous_mode.clone(),
            cwd.to_path_buf(),
            ingredients.editor_approval_enabled,
        )
        .with_injection_taint(ingredients.injection_taint.clone()),
    );
    let confiner = aivyx_sandbox::default_confiner(
        cwd,
        &ingredients.extra_read_paths,
        &deny_paths,
        ingredients.require_enforcement,
    );
    (gate, confiner)
}

/// Mirrors `aivyx-config`'s own private `resolve_tilde_paths` exactly
/// (tilde expansion + symlink canonicalization) — duplicated rather than
/// adding a new `aivyx-core` -> `aivyx-config` dependency for one small
/// helper, the same justified-duplication shape `aivyx-config` itself
/// already uses for `resolve_symlinks` (borrowed from `aivyx-tools`, per
/// its own doc comment). Keep the three in sync if any changes.
fn resolve_tilde_paths(raw_paths: &[String]) -> Vec<PathBuf> {
    let home_dir = directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf());

    raw_paths
        .iter()
        .filter_map(|raw| {
            let expanded = if let Some(rest) = raw.strip_prefix("~/") {
                home_dir.as_ref().map(|home| home.join(rest))
            } else if raw == "~" {
                home_dir.clone()
            } else if raw.starts_with('~') {
                tracing::warn!(
                    entry = %raw,
                    "unsupported ~username path syntax in extra_deny_paths; skipping this entry"
                );
                None
            } else {
                Some(PathBuf::from(raw))
            };
            expanded.map(|path| resolve_symlinks(&path))
        })
        .collect()
}

/// Canonicalizes as much of `path` as exists, then re-appends whatever
/// doesn't — mirrors `aivyx-config`'s own private `resolve_symlinks`
/// exactly (itself already a documented duplicate of `aivyx-tools`'s
/// `path_resolve::resolve_symlinks`).
fn resolve_symlinks(path: &Path) -> PathBuf {
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut current = path;

    loop {
        if let Ok(canonical) = current.canonicalize() {
            let mut result = canonical;
            for component in tail.into_iter().rev() {
                result.push(component);
            }
            return result;
        }

        match (current.file_name(), current.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                current = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_sandbox::{PermissionDecision, PermissionRequest, PermissionTarget, UserResponse};
    use async_trait::async_trait;

    struct AlwaysAllowPrompter;
    #[async_trait]
    impl PermissionPrompter for AlwaysAllowPrompter {
        async fn prompt(&self, _request: &PermissionRequest) -> UserResponse {
            UserResponse::Allow
        }
    }

    fn member(name: &str, extra_deny_paths: &[&str]) -> aivyx_team::TeamMember {
        aivyx_team::TeamMember {
            name: name.to_string(),
            role: "Specialist".to_string(),
            persona: "You specialize.".to_string(),
            tool_allowlist: vec![],
            extra_deny_paths: extra_deny_paths.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn ingredients(base_deny_paths: Vec<PathBuf>) -> SpecialistEnforcementIngredients {
        SpecialistEnforcementIngredients {
            prompter: Arc::new(AlwaysAllowPrompter),
            base_deny_paths,
            pre_approved_commands: vec![],
            plan_mode: PlanMode::new(),
            autonomous_mode: AutonomousMode::new(),
            editor_approval_enabled: false,
            injection_taint: InjectionTaint::new(),
            extra_read_paths: vec![],
            require_enforcement: false,
        }
    }

    #[tokio::test]
    async fn a_member_with_no_extra_deny_paths_only_gets_the_base_list_blocked() {
        let base = PathBuf::from("/tmp/base-denied");
        let ing = ingredients(vec![base.clone()]);
        let m = member("implementer", &[]);
        let (gate, _confiner) = scoped_gate_and_confiner(&ing, &m, Path::new("/tmp"));

        let decision = gate
            .check(&PermissionRequest {
                tool_name: "write_file".to_string(),
                action: aivyx_sandbox::ActionKind::Write,
                target: PermissionTarget::Path(base.join("secret.txt")),
                arguments_preview: serde_json::json!({}),
                preview: None,
                diff: None,
            })
            .await;
        assert!(
            matches!(decision, PermissionDecision::Deny(..)),
            "the base deny_paths entry must still be enforced for a member with no extras"
        );
    }

    #[tokio::test]
    async fn a_members_own_extra_deny_paths_are_hard_blocked() {
        let dir = std::env::temp_dir().join(format!(
            "specialist-enforcement-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let ing = ingredients(vec![]);
        let m = member("implementer", &[dir.to_string_lossy().as_ref()]);
        let (gate, _confiner) = scoped_gate_and_confiner(&ing, &m, Path::new("/tmp"));

        let decision = gate
            .check(&PermissionRequest {
                tool_name: "write_file".to_string(),
                action: aivyx_sandbox::ActionKind::Write,
                target: PermissionTarget::Path(dir.join("secret.txt")),
                arguments_preview: serde_json::json!({}),
                preview: None,
                diff: None,
            })
            .await;
        assert!(
            matches!(decision, PermissionDecision::Deny(..)),
            "a member's own extra_deny_paths entry must be hard-blocked, even though it's \
             absent from the lead's own base_deny_paths"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_path_outside_both_the_base_and_extra_lists_is_unaffected() {
        let base = PathBuf::from("/tmp/base-denied");
        let ing = ingredients(vec![base]);
        let m = member("implementer", &["/tmp/also-denied"]);
        let (gate, _confiner) = scoped_gate_and_confiner(&ing, &m, Path::new("/tmp"));

        let decision = gate
            .check(&PermissionRequest {
                tool_name: "write_file".to_string(),
                action: aivyx_sandbox::ActionKind::Write,
                target: PermissionTarget::Path(PathBuf::from("/tmp/totally-unrelated.txt")),
                arguments_preview: serde_json::json!({}),
                preview: None,
                diff: None,
            })
            .await;
        assert!(
            !matches!(decision, PermissionDecision::Deny(..)),
            "a path outside every deny list must not be blocked by this tier"
        );
    }

    #[test]
    fn different_members_of_the_same_call_get_independently_scoped_deny_lists() {
        // Regression guard against a shared-mutable-state bug: calling
        // scoped_gate_and_confiner for one member must never leak into
        // what a second, unrelated member (called against the same
        // ingredients) ends up with.
        let ing = ingredients(vec![PathBuf::from("/tmp/base")]);
        let alice = member("alice", &["/tmp/alice-only"]);
        let bob = member("bob", &["/tmp/bob-only"]);
        let (_alice_gate, _) = scoped_gate_and_confiner(&ing, &alice, Path::new("/tmp"));
        let (_bob_gate, _) = scoped_gate_and_confiner(&ing, &bob, Path::new("/tmp"));
        // ingredients.base_deny_paths itself must be unchanged after both calls.
        assert_eq!(ing.base_deny_paths, vec![PathBuf::from("/tmp/base")]);
    }
}
