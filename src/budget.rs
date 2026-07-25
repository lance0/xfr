//! Shared byte budget for `-n` tests (transfer N bytes, then stop).
//!
//! All send loops for one test share a single [`ByteBudget`], so the total
//! transferred is exactly N regardless of how many streams are running and
//! how the bytes happen to divide between them. A stream that gets more
//! socket time simply claims more of the budget; slow streams don't hold
//! back a fixed share they can't deliver.
//!
//! Claims are two-phase because a write can be short: [`claim`] reserves
//! bytes before the write and [`refund`] returns whatever didn't make it
//! out, so a partial write doesn't silently shrink the transfer.
//!
//! [`claim`]: ByteBudget::claim
//! [`refund`]: ByteBudget::refund

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Remaining bytes for a byte-budget test, shared across every send loop.
#[derive(Debug)]
pub struct ByteBudget {
    remaining: AtomicU64,
}

impl ByteBudget {
    /// Create a budget of `total` bytes.
    pub fn new(total: u64) -> Arc<Self> {
        Arc::new(Self {
            remaining: AtomicU64::new(total),
        })
    }

    /// Reserve up to `want` bytes for one write. Returns how many were
    /// reserved, which is 0 once the budget is spent — the caller's cue to
    /// stop sending. Anything reserved but not written must be handed back
    /// with [`ByteBudget::refund`].
    pub fn claim(&self, want: usize) -> usize {
        let want = want as u64;
        let mut current = self.remaining.load(Ordering::Relaxed);
        loop {
            let granted = current.min(want);
            if granted == 0 {
                return 0;
            }
            match self.remaining.compare_exchange_weak(
                current,
                current - granted,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return granted as usize,
                Err(observed) => current = observed,
            }
        }
    }

    /// Return bytes that were claimed but never written (a short write, or
    /// a write that failed outright).
    pub fn refund(&self, bytes: usize) {
        if bytes > 0 {
            self.remaining.fetch_add(bytes as u64, Ordering::Relaxed);
        }
    }

    /// True once every byte has been claimed.
    pub fn is_spent(&self) -> bool {
        self.remaining.load(Ordering::Relaxed) == 0
    }
}

/// Claim from an optional budget: `None` means an unbounded (timed) test,
/// where the caller always writes the full chunk.
pub fn claim_or_full(budget: Option<&Arc<ByteBudget>>, want: usize) -> usize {
    match budget {
        Some(b) => b.claim(want),
        None => want,
    }
}

/// Hand back an unwritten remainder to an optional budget.
pub fn refund_unwritten(budget: Option<&Arc<ByteBudget>>, claimed: usize, written: usize) {
    if let Some(b) = budget {
        b.refund(claimed.saturating_sub(written));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_never_exceed_the_budget_in_total() {
        let budget = ByteBudget::new(1000);
        let mut total = 0;
        // Ask for more than remains on the last claim.
        for _ in 0..20 {
            total += budget.claim(64);
        }
        assert_eq!(total, 1000, "sum of claims must equal the budget exactly");
        assert_eq!(budget.claim(64), 0, "spent budget grants nothing");
        assert!(budget.is_spent());
    }

    #[test]
    fn final_claim_is_the_short_remainder() {
        let budget = ByteBudget::new(100);
        assert_eq!(budget.claim(64), 64);
        assert_eq!(budget.claim(64), 36, "last claim is clipped, not refused");
        assert_eq!(budget.claim(64), 0);
    }

    #[test]
    fn refund_returns_unwritten_bytes_to_the_pool() {
        let budget = ByteBudget::new(100);
        assert_eq!(budget.claim(100), 100);
        assert!(budget.is_spent());
        // A short write hands back what never left the socket.
        budget.refund(40);
        assert!(!budget.is_spent());
        assert_eq!(budget.claim(100), 40, "refunded bytes are claimable again");
    }

    #[test]
    fn optional_helpers_pass_through_when_untargeted() {
        assert_eq!(claim_or_full(None, 1500), 1500, "timed tests are unbounded");
        // Must not panic without a budget.
        refund_unwritten(None, 1500, 0);

        let budget = ByteBudget::new(1000);
        assert_eq!(claim_or_full(Some(&budget), 1500), 1000);
        refund_unwritten(Some(&budget), 1000, 600);
        assert_eq!(claim_or_full(Some(&budget), 1500), 400);
    }

    #[test]
    fn concurrent_claims_still_sum_to_the_budget() {
        // The whole point of sharing one budget across streams: no matter
        // how the claims interleave, the total handed out is exactly N.
        let budget = ByteBudget::new(100_000);
        let counted: usize = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let budget = &budget;
                    s.spawn(move || {
                        let mut got = 0;
                        loop {
                            let n = budget.claim(1000);
                            if n == 0 {
                                return got;
                            }
                            got += n;
                        }
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(counted, 100_000);
        assert!(budget.is_spent());
    }
}
