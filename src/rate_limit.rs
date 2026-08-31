//! Per-IP rate limiting
//!
//! Limits the number of concurrent tests per IP address.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// Rate limiter state for a single IP
struct IpState {
    count: u32,
    last_access: Instant,
}

/// Per-IP rate limiter
pub struct RateLimiter {
    limits: DashMap<IpAddr, IpState>,
    max_per_ip: u32,
    window: Duration,
}

impl RateLimiter {
    /// Create a new rate limiter
    ///
    /// # Arguments
    /// * `max_per_ip` - Maximum concurrent tests per IP
    /// * `window` - Time window for tracking (entries older than this are cleaned up)
    pub fn new(max_per_ip: u32, window: Duration) -> Self {
        Self {
            limits: DashMap::new(),
            max_per_ip,
            window,
        }
    }

    /// Check if a new test from this IP is allowed
    ///
    /// Returns Ok(()) if allowed, Err with current count if rate limited
    pub fn check(&self, ip: IpAddr) -> Result<(), RateLimitError> {
        // Keep the DashMap entry guard through the admission update. Cleanup
        // therefore cannot unlink a zero-count state between lookup and
        // increment, and release will always find the state that was admitted.
        let mut state = self.limits.entry(ip).or_insert_with(|| IpState {
            count: 0,
            last_access: Instant::now(),
        });

        if state.count >= self.max_per_ip {
            return Err(RateLimitError {
                ip,
                current: state.count,
                max: self.max_per_ip,
            });
        }

        state.count += 1;
        state.last_access = Instant::now();
        Ok(())
    }

    /// Release a slot when a test completes
    pub fn release(&self, ip: IpAddr) {
        if let Some(mut state) = self.limits.get_mut(&ip) {
            state.count = state.count.saturating_sub(1);
        }
    }

    /// Get current count for an IP
    pub fn current_count(&self, ip: IpAddr) -> u32 {
        self.limits.get(&ip).map(|state| state.count).unwrap_or(0)
    }

    /// Clean up stale entries
    pub fn cleanup(&self) {
        let now = Instant::now();
        self.limits.retain(|_, state| {
            // Keep if active (count > 0) or recently accessed
            state.count > 0 || now.duration_since(state.last_access) < self.window
        });
    }

    /// Start a background cleanup task
    pub fn start_cleanup_task(self: Arc<Self>) {
        let limiter = self.clone();
        let interval = self.window / 2;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                limiter.cleanup();
            }
        });
    }
}

/// Error returned when rate limit is exceeded
#[derive(Debug, Clone)]
pub struct RateLimitError {
    pub ip: IpAddr,
    pub current: u32,
    pub max: u32,
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Rate limit exceeded for {}: {} concurrent tests (max {})",
            self.ip, self.current, self.max
        )
    }
}

impl std::error::Error for RateLimitError {}

/// RAII guard that releases rate limit slot on drop
/// Ensures cleanup happens even if the task panics
pub struct RateLimitGuard {
    limiter: Arc<RateLimiter>,
    ip: IpAddr,
}

impl RateLimitGuard {
    /// Create a new guard that will release the rate limit slot on drop
    pub fn new(limiter: Arc<RateLimiter>, ip: IpAddr) -> Self {
        Self { limiter, ip }
    }
}

impl Drop for RateLimitGuard {
    fn drop(&mut self) {
        self.limiter.release(self.ip);
    }
}

/// Rate limiter configuration
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub max_per_ip: Option<u32>,
    pub window_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_per_ip: None, // Disabled by default
            window_secs: 60,
        }
    }
}

impl RateLimitConfig {
    /// Build a rate limiter from this configuration
    pub fn build(&self) -> Option<Arc<RateLimiter>> {
        self.max_per_ip
            .map(|max| Arc::new(RateLimiter::new(max, Duration::from_secs(self.window_secs))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn test_allows_under_limit() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        assert!(limiter.check(ip).is_ok());
        assert!(limiter.check(ip).is_ok());
        assert_eq!(limiter.current_count(ip), 2);
    }

    #[test]
    fn test_blocks_over_limit() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        assert!(limiter.check(ip).is_ok());
        assert!(limiter.check(ip).is_ok());
        assert!(limiter.check(ip).is_err());
    }

    #[test]
    fn test_release_allows_new() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        assert!(limiter.check(ip).is_ok());
        assert!(limiter.check(ip).is_err());
        limiter.release(ip);
        assert!(limiter.check(ip).is_ok());
    }

    #[test]
    fn test_different_ips_independent() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let ip1 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let ip2 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));

        assert!(limiter.check(ip1).is_ok());
        assert!(limiter.check(ip2).is_ok());
        assert!(limiter.check(ip1).is_err());
        assert!(limiter.check(ip2).is_err());
    }

    #[test]
    fn test_cleanup_preserves_active_admission() {
        let limiter = RateLimiter::new(1, Duration::ZERO);
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        limiter.check(ip).unwrap();
        limiter.cleanup();
        assert_eq!(limiter.current_count(ip), 1);

        limiter.release(ip);
        limiter.cleanup();
        assert_eq!(limiter.current_count(ip), 0);
    }

    #[test]
    fn test_concurrent_cleanup_cannot_unlink_admission() {
        const ITERATIONS: usize = 5_000;

        let limiter = Arc::new(RateLimiter::new(1, Duration::ZERO));
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let start = Arc::new(Barrier::new(3));
        let finish = Arc::new(Barrier::new(3));
        let admitted = Arc::new(AtomicBool::new(false));
        let mut violations = 0usize;

        std::thread::scope(|scope| {
            let checker_limiter = limiter.clone();
            let checker_start = start.clone();
            let checker_finish = finish.clone();
            let checker_admitted = admitted.clone();
            scope.spawn(move || {
                for _ in 0..ITERATIONS {
                    checker_start.wait();
                    checker_admitted.store(checker_limiter.check(ip).is_ok(), Ordering::SeqCst);
                    checker_finish.wait();
                }
            });

            let cleaner_limiter = limiter.clone();
            let cleaner_start = start.clone();
            let cleaner_finish = finish.clone();
            scope.spawn(move || {
                for _ in 0..ITERATIONS {
                    cleaner_start.wait();
                    cleaner_limiter.cleanup();
                    cleaner_finish.wait();
                }
            });

            for _ in 0..ITERATIONS {
                // Seed a stale zero-count entry so check and cleanup contend
                // on precisely the state that the old Arc-based design could
                // unlink after check dropped its map guard.
                if limiter.check(ip).is_ok() {
                    limiter.release(ip);
                } else {
                    violations += 1;
                }

                admitted.store(false, Ordering::SeqCst);
                start.wait();
                finish.wait();

                let was_admitted = admitted.load(Ordering::SeqCst);
                let count = limiter.current_count(ip);
                if !was_admitted || count != 1 {
                    violations += 1;
                }
                for _ in 0..count {
                    limiter.release(ip);
                }
                limiter.cleanup();
            }
        });

        assert_eq!(violations, 0, "cleanup unlinked an admission in progress");
    }
}
