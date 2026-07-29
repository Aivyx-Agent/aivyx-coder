//! Real OS-level process confinement, replacing `NoopConfiner`: Landlock
//! (filesystem scoping) + a seccomp-bpf syscall denylist. See the Phase 5
//! plan for the policy rationale (informed by, but deliberately not
//! identical to, Codex CLI's current bubblewrap-based sandbox).

use std::io;
use std::path::{Path, PathBuf};

use landlock::{
    ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreated, RulesetCreatedAttr, RulesetStatus,
    path_beneath_rules,
};
use seccompiler::{BpfProgram, SeccompAction, SeccompFilter};

use crate::ExecutionConfiner;

const LANDLOCK_ABI: ABI = ABI::V7;

/// `LANDLOCK_CREATE_RULESET_VERSION` from the kernel's landlock UAPI header
/// — not re-exported by the `landlock` crate (its `uapi` module is
/// private), but stable and simple enough to inline directly rather than
/// pull in a second crate for one flag value.
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;

/// Common system/toolchain read paths granted by default, in addition to
/// the working directory and any configured `extra_read_paths`. Scoping
/// reads to just the working directory breaks real toolchains (compilers,
/// package managers reading outside the project) — deliberately not Codex
/// CLI's "read everything" default, though: Landlock has no negative/deny
/// rule, so excluding `deny_paths` entries from a broad `/` grant would
/// require enumerating and re-granting every sibling directory except the
/// denied ones. A bounded, explicit list mostly sidesteps that — and for
/// the one grant that can't stay narrow (the working directory, which must
/// be granted wholesale to be useful), `grant_paths_excluding` below does
/// the carve-out properly instead of ignoring the problem.
/// Nonexistent paths are silently skipped by `path_beneath_rules`, so it's
/// safe to list toolchain paths that may not exist on a given system.
const DEFAULT_READ_PATHS: &[&str] = &["/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc"];

/// Home-relative toolchain paths, joined against `$HOME` when set.
/// `.gitconfig`/`.config/git` matter beyond convenience: `git commit` needs
/// the user's identity from global config, and git treats an *unreadable*
/// (EACCES) existing config file as fatal — so without these grants every
/// confined `git` invocation on a machine with a global config would die.
const DEFAULT_HOME_READ_PATHS: &[&str] = &[".cargo", ".rustup", ".gitconfig", ".config/git"];

/// Harmless character devices granted read+write. `/dev` is deliberately
/// NOT granted wholesale (block devices, other users' ttys); but without at
/// least `/dev/null` every shell construct like `2>/dev/null` dies with
/// EACCES under the sandbox — a live-E2E finding, not a hypothetical.
const DEVICE_RW_PATHS: &[&str] = &["/dev/null", "/dev/zero", "/dev/urandom", "/dev/random"];

/// Syscalls with no legitimate use in a coding agent's shell commands,
/// blocked regardless of what Landlock's filesystem scoping already
/// prevents — defense in depth against confinement-escape/introspection
/// primitives (`ptrace`, `io_uring`, `perf_event_open`, the kernel keyring,
/// `userfaultfd`) and privileged operations that a namespace-based sandbox
/// (like Codex CLI's bubblewrap) would otherwise block for free via
/// capability dropping. This design doesn't use namespaces, so those need
/// to be explicit here instead. `unshare`/`setns` are blocked outright since
/// a coding agent's tools never need to create or join namespaces.
const BLOCKED_SYSCALLS: &[i64] = &[
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_reboot,
    libc::SYS_kexec_load,
    libc::SYS_kexec_file_load,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
    libc::SYS_pivot_root,
    libc::SYS_swapon,
    libc::SYS_swapoff,
    libc::SYS_acct,
    libc::SYS_bpf,
    libc::SYS_perf_event_open,
    libc::SYS_keyctl,
    libc::SYS_add_key,
    libc::SYS_request_key,
    libc::SYS_userfaultfd,
    libc::SYS_unshare,
    libc::SYS_setns,
    libc::SYS_personality,
];

/// Read-only probe of kernel Landlock support, safe to call from the parent
/// at any time — mirrors the `landlock` crate's own internal ABI-detection
/// logic (`landlock-0.4.5/src/compat.rs`, `LandlockStatus::current`): a
/// null ruleset-attr pointer and zero size, which the kernel documents as
/// just reporting the ABI version rather than creating a real ruleset.
/// Returns the raw syscall result: non-negative is the supported ABI
/// version, negative means unsupported (`ENOSYS`) or disabled (`EOPNOTSUPP`).
fn detect_landlock_abi() -> i64 {
    // SAFETY: read-only syscall with a null pointer and zero length, per the
    // kernel's own documented probing convention (see comment above).
    unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    }
}

pub struct LandlockConfiner {
    read_paths: Vec<PathBuf>,
    write_paths: Vec<PathBuf>,
    seccomp_program: BpfProgram,
    require_enforcement: bool,
}

impl LandlockConfiner {
    pub fn new(
        cwd: &Path,
        extra_read_paths: &[PathBuf],
        deny_paths: &[PathBuf],
        require_enforcement: bool,
    ) -> Self {
        if detect_landlock_abi() < 0 {
            tracing::warn!(
                require_enforcement,
                "Landlock is not supported or not enabled on this kernel; process-execution \
                 tools will {} until this is resolved",
                if require_enforcement {
                    "refuse to run (sandbox.require_enforcement is true)"
                } else {
                    "run unconfined (sandbox.require_enforcement is false)"
                }
            );
        }

        // Fixed system/toolchain read paths: never scanned for
        // basename-glob matches (see `find_basename_glob_matches`'s doc
        // comment for why — no project secret plausibly lives under
        // `/usr` etc., and recursively walking them would be substantial,
        // pointless work). Bare patterns already have zero effect on
        // `grant_paths_excluding`'s existing absolute-only checks, so
        // passing the plain `deny_paths` list here is a correctness
        // no-op, not a special case that needs its own logic.
        let mut system_read_candidates: Vec<PathBuf> =
            DEFAULT_READ_PATHS.iter().map(PathBuf::from).collect();
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            system_read_candidates.extend(DEFAULT_HOME_READ_PATHS.iter().map(|p| home.join(p)));
        }
        let mut read_paths: Vec<PathBuf> = system_read_candidates
            .iter()
            .flat_map(|root| grant_paths_excluding(root, deny_paths))
            .collect();

        // `cwd` is project-relevant — its own basename-glob matches are
        // computed once here and reused for both its read grant (below)
        // and its write grant (further down), since a project's own
        // secrets are exactly what this feature exists to protect.
        let mut cwd_deny_paths = deny_paths.to_vec();
        cwd_deny_paths.extend(find_basename_glob_matches(cwd, deny_paths));
        read_paths.extend(grant_paths_excluding(cwd, &cwd_deny_paths));

        // `extra_read_paths` entries are also project-relevant (they're
        // explicitly configured additional read locations), so each gets
        // its own basename-glob scan.
        for extra_root in extra_read_paths {
            let mut extra_deny_paths = deny_paths.to_vec();
            extra_deny_paths.extend(find_basename_glob_matches(extra_root, deny_paths));
            read_paths.extend(grant_paths_excluding(extra_root, &extra_deny_paths));
        }
        // Read side of the device grants below (read and write rules are
        // separate Landlock rule sets, so both lists need the entries).
        read_paths.extend(DEVICE_RW_PATHS.iter().map(PathBuf::from));

        let mut write_paths: Vec<PathBuf> = grant_paths_excluding(cwd, &cwd_deny_paths);
        // The OS temp directory and `TMPDIR` are not project-relevant —
        // same reasoning as the system read paths above — but `cwd` is
        // very commonly itself a subdirectory of the system tmp dir (e.g.
        // in tests or scratch working directories, as here), so this grant
        // is still filtered against `cwd_deny_paths`, not the plain
        // `deny_paths`: otherwise a resolved bare-pattern match like
        // `.env` would only be excluded from `cwd`'s own grant above,
        // while this wholesale `AccessFs::from_all` grant over an
        // ancestor directory would silently re-grant it (from_all
        // includes read, not just write).
        let mut system_write_candidates = vec![std::env::temp_dir()];
        if let Some(tmpdir) = std::env::var_os("TMPDIR") {
            system_write_candidates.push(PathBuf::from(tmpdir));
        }
        write_paths.extend(
            system_write_candidates
                .iter()
                .flat_map(|root| grant_paths_excluding(root, &cwd_deny_paths)),
        );
        // Individual device files, not subject to deny_paths carve-outs
        // (they're fixed, well-known, and content-free); `path_beneath_rules`
        // silently skips any that don't exist.
        write_paths.extend(DEVICE_RW_PATHS.iter().map(PathBuf::from));
        let seccomp_program = build_seccomp_filter();

        Self {
            read_paths,
            write_paths,
            seccomp_program,
            require_enforcement,
        }
    }

    /// Builds the full ruleset here, in the parent process, before `fork()`
    /// — all allocation (rule construction, path resolution) must happen
    /// before the `pre_exec` hook runs, since that closure executes in the
    /// forked child under async-signal-safety constraints (no allocation,
    /// no locks). `RulesetCreated::restrict_self()` itself is verified to
    /// be a thin syscall wrapper over this already-built state.
    fn build_ruleset(&self) -> Result<RulesetCreated, landlock::RulesetError> {
        Ruleset::default()
            .handle_access(AccessFs::from_all(LANDLOCK_ABI))?
            .create()?
            .add_rules(path_beneath_rules(
                &self.read_paths,
                AccessFs::from_read(LANDLOCK_ABI),
            ))?
            .add_rules(path_beneath_rules(
                &self.write_paths,
                AccessFs::from_all(LANDLOCK_ABI),
            ))
    }
}

/// Landlock has no negative/deny rule — a domain can only ever be *more*
/// restricted than ambient, never "grant X except Y". To grant `root`
/// wholesale while still excluding a `deny_paths` entry nested somewhere
/// inside it, enumerate `root`'s direct children and grant each
/// individually: recurse into any child that itself contains a denial
/// further down, and skip entirely any child that *is* a denial. When
/// nothing under `root` is denied (the common case), this returns `root`
/// unchanged with no extra filesystem work.
fn grant_paths_excluding(root: &Path, deny_paths: &[PathBuf]) -> Vec<PathBuf> {
    if deny_paths.iter().any(|denied| denied == root) {
        return Vec::new();
    }
    let relevant: Vec<&PathBuf> = deny_paths
        .iter()
        .filter(|denied| denied.starts_with(root))
        .collect();
    if relevant.is_empty() {
        return vec![root.to_path_buf()];
    }

    // Can't enumerate what's inside `root` — fail toward less access, not
    // more, rather than granting a directory whose contents are unknown.
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut grants = Vec::new();
    for entry in entries.flatten() {
        let child = entry.path();
        if relevant.iter().any(|denied| **denied == child) {
            continue;
        }
        if relevant.iter().any(|denied| denied.starts_with(&child)) {
            grants.extend(grant_paths_excluding(&child, deny_paths));
        } else {
            grants.push(child);
        }
    }
    grants
}

/// Recursively finds every path under `root` whose basename matches a
/// bare (single-component) `deny_paths` pattern — the concrete
/// file-level exclusions `LandlockConfiner::new` needs before granting
/// `root`, since `grant_paths_excluding` only understands specific
/// absolute paths to carve out, not "matches anywhere" patterns. Returns
/// immediately without touching the filesystem if `deny_paths` has no
/// bare entries at all.
fn find_basename_glob_matches(root: &Path, deny_paths: &[PathBuf]) -> Vec<PathBuf> {
    let bare_patterns: Vec<PathBuf> = deny_paths
        .iter()
        .filter(|p| crate::is_bare_pattern(p))
        .cloned()
        .collect();
    if bare_patterns.is_empty() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    walk_for_basename_matches(root, &bare_patterns, &mut matches);
    matches
}

fn walk_for_basename_matches(dir: &Path, bare_patterns: &[PathBuf], matches: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if bare_patterns
            .iter()
            .any(|pattern| crate::is_basename_glob_match(&path, pattern))
        {
            matches.push(path);
            continue; // matched — no need to recurse further into it
        }
        // `file_type()` reflects the entry itself, not a symlink's
        // target, so a symlinked directory is never recursed into —
        // this is what keeps a symlink cycle from causing unbounded
        // recursion here (unlike `grant_paths_excluding`, this function
        // recurses into *every* subdirectory by default, so this guard
        // matters more here).
        if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            walk_for_basename_matches(&path, bare_patterns, matches);
        }
    }
}

fn build_seccomp_filter() -> BpfProgram {
    let rules = BLOCKED_SYSCALLS
        .iter()
        .map(|&syscall| (syscall, vec![]))
        .collect();

    SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        std::env::consts::ARCH.try_into().expect("known arch"),
    )
    .expect("static seccomp policy is well-formed")
    .try_into()
    .expect("seccomp policy compiles to BPF")
}

impl ExecutionConfiner for LandlockConfiner {
    fn confine(&self, mut command: tokio::process::Command) -> tokio::process::Command {
        let require_enforcement = self.require_enforcement;

        let ruleset = match self.build_ruleset() {
            Ok(ruleset) => ruleset,
            Err(err) => {
                if require_enforcement {
                    // Fail closed: make the spawn itself fail rather than
                    // running unconfined. This closure runs in the parent
                    // (we haven't forked yet), so a detailed, allocated
                    // error message is fine here — the async-signal-safety
                    // constraint only applies inside `pre_exec` below.
                    tracing::warn!(
                        error = %err,
                        "failed to build Landlock ruleset; refusing to run unconfined \
                         (sandbox.require_enforcement is true)"
                    );
                    unsafe {
                        command.pre_exec(|| Err(io::Error::from(io::ErrorKind::PermissionDenied)));
                    }
                } else {
                    tracing::warn!(
                        error = %err,
                        "failed to build Landlock ruleset; running unconfined \
                         (sandbox.require_enforcement is false)"
                    );
                }
                return command;
            }
        };
        let mut ruleset = Some(ruleset);
        let seccomp_program = self.seccomp_program.clone();
        let mut seccomp_program = Some(seccomp_program);

        // SAFETY: every error path inside this closure uses an
        // `ErrorKind`-based `io::Error` (std's allocation-free "simple"
        // repr), never `io::Error::other`/`.to_string()` — both allocate,
        // which is unsound inside a forked, single-threaded child where
        // another thread's held malloc-arena lock at fork time can leave
        // the allocator permanently wedged from this process's point of
        // view. The tradeoff is losing the detailed underlying error
        // message from inside the child; that's the correct, honest
        // price of fork-safety here. All allocation (ruleset/filter
        // construction) already happened above, in the parent.
        unsafe {
            command.pre_exec(move || {
                if let Some(ruleset) = ruleset.take() {
                    let status = ruleset
                        .restrict_self()
                        .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
                    if require_enforcement && status.ruleset != RulesetStatus::FullyEnforced {
                        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
                    }
                }
                if let Some(program) = seccomp_program.take() {
                    seccompiler::apply_filter(&program)
                        .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
                }
                Ok(())
            });
        }
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    fn confiner_for(dir: &Path) -> LandlockConfiner {
        LandlockConfiner::new(dir, &[], &[], true)
    }

    async fn run(mut command: tokio::process::Command) -> (bool, String) {
        let output = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .expect("failed to run command");
        (
            output.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    }

    #[tokio::test]
    async fn write_inside_the_granted_root_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let confiner = confiner_for(dir.path());
        let target = dir.path().join("ok.txt");

        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", &format!("echo hi > {}", target.display())]);
        let command = confiner.confine(command);

        let (success, output) = run(command).await;
        assert!(success, "command failed: {output}");
        assert_eq!(std::fs::read_to_string(&target).unwrap().trim(), "hi");
    }

    #[tokio::test]
    async fn write_outside_the_granted_root_fails() {
        let dir = tempfile::tempdir().unwrap();
        // Deliberately not another `tempfile::tempdir()`: those resolve
        // under `/tmp`, which is itself write-granted (matching Codex's own
        // choice to allow `/tmp` broadly) — both dirs would land inside the
        // same grant. `/var/tmp` is a distinct, genuinely out-of-scope
        // system tmp directory.
        let outside = tempfile::Builder::new().tempdir_in("/var/tmp").unwrap();
        let confiner = confiner_for(dir.path());
        let target = outside.path().join("should-not-exist.txt");

        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", &format!("echo hi > {}", target.display())]);
        let command = confiner.confine(command);

        let (success, _output) = run(command).await;
        assert!(
            !success,
            "write outside the granted root should have failed"
        );
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn read_outside_the_allowlist_fails() {
        let dir = tempfile::tempdir().unwrap();
        // See `write_outside_the_granted_root_fails` for why `/var/tmp`
        // rather than another `tempfile::tempdir()`.
        let outside = tempfile::Builder::new().tempdir_in("/var/tmp").unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "top secret").unwrap();
        let confiner = confiner_for(dir.path());

        let mut command = tokio::process::Command::new("cat");
        command.arg(&secret);
        let command = confiner.confine(command);

        let (success, output) = run(command).await;
        assert!(!success, "read outside the allowlist should have failed");
        assert!(!output.contains("top secret"));
    }

    #[tokio::test]
    async fn read_of_an_allowlisted_path_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readable.txt"), "hello").unwrap();
        let confiner = confiner_for(dir.path());

        let mut command = tokio::process::Command::new("cat");
        command.arg(dir.path().join("readable.txt"));
        let command = confiner.confine(command);

        let (success, output) = run(command).await;
        assert!(success, "read of an allowlisted path should have succeeded");
        assert!(output.contains("hello"));
    }

    #[tokio::test]
    async fn a_normal_command_still_works_under_the_seccomp_filter() {
        let dir = tempfile::tempdir().unwrap();
        let confiner = confiner_for(dir.path());

        let mut command = tokio::process::Command::new("echo");
        command.arg("still works");
        let command = confiner.confine(command);

        let (success, output) = run(command).await;
        assert!(
            success,
            "a normal command should not be broken by the seccomp filter"
        );
        assert!(output.contains("still works"));
    }

    #[tokio::test]
    async fn deny_paths_entry_nested_inside_cwd_is_excluded_from_the_grant() {
        let dir = tempfile::tempdir().unwrap();
        let secret_dir = dir.path().join("secret");
        std::fs::create_dir(&secret_dir).unwrap();
        std::fs::write(secret_dir.join("id_rsa"), "top secret").unwrap();
        std::fs::write(dir.path().join("public.txt"), "hello").unwrap();

        let confiner =
            LandlockConfiner::new(dir.path(), &[], std::slice::from_ref(&secret_dir), true);

        let mut command = tokio::process::Command::new("cat");
        command.arg(secret_dir.join("id_rsa"));
        let command = confiner.confine(command);
        let (success, output) = run(command).await;
        assert!(!success, "denied subdirectory should not be readable");
        assert!(!output.contains("top secret"));

        let mut command = tokio::process::Command::new("cat");
        command.arg(dir.path().join("public.txt"));
        let command = confiner.confine(command);
        let (success, output) = run(command).await;
        assert!(
            success,
            "sibling of the denied subdirectory should still be readable"
        );
        assert!(output.contains("hello"));
    }

    #[test]
    fn grant_paths_excluding_returns_root_unchanged_when_nothing_is_denied() {
        let dir = tempfile::tempdir().unwrap();
        let grants = grant_paths_excluding(dir.path(), &[]);
        assert_eq!(grants, vec![dir.path().to_path_buf()]);
    }

    #[test]
    fn grant_paths_excluding_returns_empty_when_root_itself_is_denied() {
        let dir = tempfile::tempdir().unwrap();
        let grants = grant_paths_excluding(dir.path(), &[dir.path().to_path_buf()]);
        assert!(grants.is_empty());
    }

    #[test]
    fn find_basename_glob_matches_finds_a_nested_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/.env"), "SECRET=1").unwrap();
        std::fs::write(dir.path().join("public.txt"), "hello").unwrap();

        let matches = find_basename_glob_matches(dir.path(), &[PathBuf::from(".env")]);

        assert_eq!(matches, vec![dir.path().join("nested/.env")]);
    }

    #[test]
    fn find_basename_glob_matches_skips_the_scan_entirely_when_no_bare_patterns_are_configured() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "SECRET=1").unwrap();

        // Every entry here has a path separator, so `deny_paths` has no
        // bare patterns at all — the function must return empty without
        // needing to find (or miss) the very real `.env` file present.
        let matches = find_basename_glob_matches(dir.path(), &[PathBuf::from("/some/abs/path")]);

        assert!(matches.is_empty());
    }

    #[test]
    fn find_basename_glob_matches_does_not_follow_a_symlinked_directory() {
        let dir = tempfile::tempdir().unwrap();
        let real_target = tempfile::tempdir().unwrap();
        std::fs::write(real_target.path().join(".env"), "SECRET=1").unwrap();
        std::os::unix::fs::symlink(real_target.path(), dir.path().join("link")).unwrap();

        let matches = find_basename_glob_matches(dir.path(), &[PathBuf::from(".env")]);

        assert!(matches.is_empty(), "must not follow symlinked directories");
    }

    #[tokio::test]
    async fn a_bare_basename_pattern_nested_inside_cwd_is_excluded_from_the_grant() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "SECRET=1").unwrap();
        std::fs::write(dir.path().join("public.txt"), "hello").unwrap();

        let confiner = LandlockConfiner::new(dir.path(), &[], &[PathBuf::from(".env")], true);

        let mut command = tokio::process::Command::new("cat");
        command.arg(dir.path().join(".env"));
        let command = confiner.confine(command);
        let (success, output) = run(command).await;
        assert!(!success, "bare-pattern-matched file should not be readable");
        assert!(!output.contains("SECRET"));

        let mut command = tokio::process::Command::new("cat");
        command.arg(dir.path().join("public.txt"));
        let command = confiner.confine(command);
        let (success, output) = run(command).await;
        assert!(success, "non-matching sibling should still be readable");
        assert!(output.contains("hello"));
    }
}
