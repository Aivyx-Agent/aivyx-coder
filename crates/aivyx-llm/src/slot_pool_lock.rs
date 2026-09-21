//! Cross-process coordination for `KvSlotPool`'s starting offset (see that
//! module's own doc comment for why this lives in a separate file --
//! `KvSlotPool` itself stays pure, no I/O). Uses real OS-level advisory
//! file locks (`fs2`), so a claimed offset is automatically released on
//! process exit *or crash* -- no stale-lock cleanup needed, unlike a
//! PID-file-based scheme.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

use fs2::FileExt;

/// Holds an exclusive OS-level lock on one `<offset>.lock` file inside a
/// lock directory, for as long as this value is alive -- dropping it
/// releases the lock (`std::fs::File`'s own `Drop` closes the descriptor,
/// which releases an `flock`-style advisory lock automatically). Callers
/// that need the claim to last the whole process should `Box::leak` the
/// returned value rather than let it drop early -- see `agent_builder.rs`.
pub struct SlotPoolLock {
    _file: File,
    offset: u32,
}

impl SlotPoolLock {
    /// Tries `0.lock`, `1.lock`, ... up to `slot_count - 1` inside
    /// `lock_dir` (created if it doesn't exist), taking the first index
    /// this process can exclusively lock. Returns an error if every index
    /// is already claimed by another process, or if the lock directory
    /// can't be created/a lock file can't be opened -- callers should
    /// treat any error here as "fall back to offset 0," matching today's
    /// un-coordinated behavior, since this is a cache-efficiency
    /// optimization, not something worth failing agent startup over.
    pub fn acquire(lock_dir: &Path, slot_count: u32) -> io::Result<Self> {
        fs::create_dir_all(lock_dir)?;
        for offset in 0..slot_count.max(1) {
            let path = lock_dir.join(format!("{offset}.lock"));
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&path)?;
            if file.try_lock_exclusive().is_ok() {
                return Ok(Self {
                    _file: file,
                    offset,
                });
            }
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "every slot-pool lock index is already claimed by another process",
        ))
    }

    pub fn offset(&self) -> u32 {
        self.offset
    }
}

/// FNV-1a, inlined because the lock-directory name must be stable across
/// program versions -- `std`'s `DefaultHasher` explicitly does not
/// guarantee that. Duplicated (not shared) from `aivyx-core::session`'s
/// identical private function, since the two live in different crates and
/// this one value doesn't warrant a new shared-utility crate.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_claim_gets_offset_zero() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let lock = SlotPoolLock::acquire(dir.path(), 4).expect("first claim must succeed");
        assert_eq!(lock.offset(), 0);
    }

    #[test]
    fn a_second_concurrent_claim_gets_a_different_offset() {
        // `flock`-style advisory locks are held per OPEN FILE DESCRIPTION,
        // not per-process -- two separate `File::open`/lock calls, even
        // from the same test process, genuinely contend with each other.
        // This is documented `flock()` behavior on Linux/macOS (this
        // project's real supported platforms); if this test fails, that
        // assumption -- not the test itself -- is the first thing to
        // re-check.
        let dir = tempfile::tempdir().expect("must create temp dir");
        let first = SlotPoolLock::acquire(dir.path(), 4).expect("first claim must succeed");
        let second = SlotPoolLock::acquire(dir.path(), 4).expect("second claim must succeed");
        assert_eq!(first.offset(), 0);
        assert_eq!(second.offset(), 1);
    }

    #[test]
    fn releasing_a_claim_frees_its_offset_for_reuse() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let first = SlotPoolLock::acquire(dir.path(), 2).expect("first claim must succeed");
        assert_eq!(first.offset(), 0);
        drop(first);
        let second = SlotPoolLock::acquire(dir.path(), 2).expect("second claim must succeed");
        assert_eq!(
            second.offset(),
            0,
            "dropping the first lock must release offset 0"
        );
    }

    #[test]
    fn errors_once_every_index_is_already_claimed() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let _first = SlotPoolLock::acquire(dir.path(), 1).expect("first claim must succeed");
        let second = SlotPoolLock::acquire(dir.path(), 1);
        assert!(
            second.is_err(),
            "with slot_count 1, a second concurrent claim must fail, not silently reuse offset 0"
        );
    }

    #[test]
    fn fnv1a_is_deterministic_for_the_same_input() {
        assert_eq!(
            fnv1a(b"http://localhost:8080"),
            fnv1a(b"http://localhost:8080")
        );
        assert_ne!(
            fnv1a(b"http://localhost:8080"),
            fnv1a(b"http://localhost:8081")
        );
    }
}
