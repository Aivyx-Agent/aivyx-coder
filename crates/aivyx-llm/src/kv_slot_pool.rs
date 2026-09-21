//! A pure numeric slot-id pool -- tracks which of `0..total_slots` are
//! currently checked out. No I/O, no knowledge of `aivyx-kvcache` at all;
//! `aivyx-core::Agent` (the caller) decides what a checked-out slot id is
//! actually used for. Mirrors `aivyx-mcp-server`'s `SessionMap::take`/
//! `put_back` checkout/release shape.

use std::collections::HashSet;
use std::sync::Mutex;

pub struct KvSlotPool {
    total_slots: u32,
    offset: u32,
    checked_out: Mutex<HashSet<u32>>,
}

impl KvSlotPool {
    /// Equivalent to `with_offset(total_slots, 0)` — the pool's original,
    /// single-process behavior (always scans starting from slot 0).
    pub fn new(total_slots: u32) -> Self {
        Self::with_offset(total_slots, 0)
    }

    /// Like `new`, but `checkout()` scans starting from `offset` instead
    /// of always from slot 0 — lets a caller (see `SlotPoolLock`) give
    /// concurrent processes sharing one physical `llama-server` distinct
    /// starting points, so they land on different slots instead of all
    /// racing for slot 0 first. Full slot coverage is unaffected: every
    /// slot is still tried exactly once per full scan, only the order
    /// changes. `offset` wraps via `%`, so any `u32` value (including one
    /// `>= total_slots`) is safe to pass.
    pub fn with_offset(total_slots: u32, offset: u32) -> Self {
        Self {
            total_slots,
            offset: if total_slots == 0 {
                0
            } else {
                offset % total_slots
            },
            checked_out: Mutex::new(HashSet::new()),
        }
    }

    /// Returns the lowest-numbered free slot id starting from `offset`
    /// (wrapping), or `None` if every slot is already checked out.
    /// Deterministic ordering makes pool behavior predictable in tests;
    /// no particular ordering is required for correctness beyond full
    /// coverage.
    pub fn checkout(&self) -> Option<u32> {
        let mut checked_out = self.checked_out.lock().unwrap();
        (0..self.total_slots)
            .map(|i| (i + self.offset) % self.total_slots)
            .find(|id| checked_out.insert(*id))
    }

    /// Returns `slot_id` to the pool. A `slot_id` that was never checked
    /// out (or already released) is a silent no-op -- release is called
    /// from `Drop` impls, where panicking or erroring is not an option.
    pub fn release(&self, slot_id: u32) {
        self.checked_out.lock().unwrap().remove(&slot_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_returns_lowest_free_id_first() {
        let pool = KvSlotPool::new(4);
        assert_eq!(pool.checkout(), Some(0));
        assert_eq!(pool.checkout(), Some(1));
    }

    #[test]
    fn checkout_returns_none_once_the_pool_is_full() {
        let pool = KvSlotPool::new(2);
        assert_eq!(pool.checkout(), Some(0));
        assert_eq!(pool.checkout(), Some(1));
        assert_eq!(
            pool.checkout(),
            None,
            "pool of size 2 must reject a third concurrent checkout"
        );
    }

    #[test]
    fn release_makes_a_slot_available_again() {
        let pool = KvSlotPool::new(1);
        let id = pool.checkout().expect("pool of size 1 has a free slot");
        assert_eq!(
            pool.checkout(),
            None,
            "the only slot is already checked out"
        );
        pool.release(id);
        assert_eq!(
            pool.checkout(),
            Some(id),
            "release must make the slot checkoutable again"
        );
    }

    #[test]
    fn releasing_a_never_checked_out_id_is_a_silent_no_op() {
        let pool = KvSlotPool::new(4);
        pool.release(99); // never checked out -- must not panic
        assert_eq!(
            pool.checkout(),
            Some(0),
            "pool must still function normally after a no-op release"
        );
    }

    #[test]
    fn with_offset_starts_checkout_from_the_given_slot() {
        let pool = KvSlotPool::with_offset(4, 2);
        assert_eq!(pool.checkout(), Some(2));
        assert_eq!(pool.checkout(), Some(3));
        assert_eq!(pool.checkout(), Some(0), "must wrap around past the end");
        assert_eq!(pool.checkout(), Some(1));
        assert_eq!(
            pool.checkout(),
            None,
            "still rejects a 5th concurrent checkout"
        );
    }

    #[test]
    fn with_offset_zero_matches_new_exactly() {
        let via_new = KvSlotPool::new(4);
        let via_offset = KvSlotPool::with_offset(4, 0);
        assert_eq!(via_new.checkout(), Some(0));
        assert_eq!(via_offset.checkout(), Some(0));
        assert_eq!(via_new.checkout(), Some(1));
        assert_eq!(via_offset.checkout(), Some(1));
    }

    #[test]
    fn with_offset_wraps_an_offset_greater_than_total_slots() {
        // offset 6 against 4 total slots must behave identically to
        // offset 2 (6 % 4 == 2), not panic or index out of range.
        let pool = KvSlotPool::with_offset(4, 6);
        assert_eq!(pool.checkout(), Some(2));
    }
}
