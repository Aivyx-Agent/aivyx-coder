//! Real OS-level process confinement, replacing `NoopConfiner`: Landlock
//! (filesystem scoping) + a seccomp-bpf syscall denylist. See the Phase 5
//! plan for the policy rationale (informed by, but deliberately not
//! identical to, Codex CLI's current bubblewrap-based sandbox).

use std::io;
use std::path::{Path, PathBuf};

use landlock::{
    ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreated, RulesetCreatedAttr,
    path_beneath_rules,
};
use seccompiler::{BpfProgram, SeccompAction, SeccompFilter};

use crate::ExecutionConfiner;

const LANDLOCK_ABI: ABI = ABI::V7;

/// Common system/toolchain read paths granted by default, in addition to
/// the working directory and any configured `extra_read_paths`. Scoping
/// reads to just the working directory breaks real toolchains (compilers,
/// package managers reading outside the project) — deliberately not Codex
/// CLI's "read everything" default, though: Landlock has no negative/deny
/// rule, so excluding `deny_paths` entries (`~/.ssh`, `~/.aws`) from a broad
/// `/` grant would require enumerating and re-granting every sibling
/// directory except the denied ones. A bounded, explicit list sidesteps
/// that entirely — denied paths are simply never granted, full stop.
/// Nonexistent paths are silently skipped by `path_beneath_rules`, so it's
/// safe to list toolchain paths that may not exist on a given system.
const DEFAULT_READ_PATHS: &[&str] = &["/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc"];

/// Home-relative toolchain paths, joined against `$HOME` when set.
const DEFAULT_HOME_READ_PATHS: &[&str] = &[".cargo", ".rustup"];

/// Syscalls with no legitimate use in a coding agent's shell commands,
/// blocked regardless of what Landlock's filesystem scoping already
/// prevents — defense in depth against confinement-escape/introspection
/// primitives (`ptrace`, `io_uring`) and privileged operations that a
/// namespace-based sandbox (like Codex CLI's bubblewrap) would otherwise
/// block for free via capability dropping. This design doesn't use
/// namespaces, so those need to be explicit here instead.
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
];

pub struct LandlockConfiner {
    read_paths: Vec<PathBuf>,
    write_paths: Vec<PathBuf>,
    seccomp_program: BpfProgram,
}

impl LandlockConfiner {
    pub fn new(cwd: &Path, extra_read_paths: &[PathBuf]) -> Self {
        let mut read_paths: Vec<PathBuf> = DEFAULT_READ_PATHS.iter().map(PathBuf::from).collect();
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            read_paths.extend(DEFAULT_HOME_READ_PATHS.iter().map(|p| home.join(p)));
        }
        read_paths.push(cwd.to_path_buf());
        read_paths.extend(extra_read_paths.iter().cloned());

        let mut write_paths = vec![cwd.to_path_buf(), std::env::temp_dir()];
        if let Some(tmpdir) = std::env::var_os("TMPDIR") {
            write_paths.push(PathBuf::from(tmpdir));
        }

        let seccomp_program = build_seccomp_filter();

        Self {
            read_paths,
            write_paths,
            seccomp_program,
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
        let ruleset = match self.build_ruleset() {
            Ok(ruleset) => ruleset,
            Err(err) => {
                // Fail open on the ruleset itself, matching the crate's own
                // best-effort philosophy: a coding agent that stops working
                // because sandboxing couldn't be constructed is a worse
                // outcome than running this one command unconfined.
                tracing::warn!(error = %err, "failed to build Landlock ruleset; running unconfined");
                return command;
            }
        };
        let mut ruleset = Some(ruleset);
        let seccomp_program = self.seccomp_program.clone();
        let mut seccomp_program = Some(seccomp_program);

        // SAFETY: the closure only calls `RulesetCreated::restrict_self()`
        // and `seccompiler::apply_filter()`, both verified against crate
        // source to be thin wrappers around a handful of raw syscalls
        // (`landlock_restrict_self`, `prctl`, `seccomp`) with no heap
        // allocation at the call site — the async-signal-safety contract
        // `pre_exec` requires. All allocation (ruleset/filter construction)
        // already happened above, in the parent, before this runs.
        unsafe {
            command.pre_exec(move || {
                if let Some(ruleset) = ruleset.take() {
                    ruleset
                        .restrict_self()
                        .map_err(|err| io::Error::other(err.to_string()))?;
                }
                if let Some(program) = seccomp_program.take() {
                    seccompiler::apply_filter(&program)
                        .map_err(|err| io::Error::other(err.to_string()))?;
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
        LandlockConfiner::new(dir, &[])
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
}
