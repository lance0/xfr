//! Per-IP rate limiting
//!
//! Limits the number of concurrent tests per IP address.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::Mutex;

/// Rate limiter state for a single IP
struct IpState {
    count: AtomicU32,
    last_access: Mutex<Instant>,
}

/// Per-IP rate limiter
pub struct RateLimiter {
    limits: DashMap<IpAddr, Arc<IpState>>,
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
        let state = self
            .limits
            .entry(ip)
            .or_insert_with(|| {
                Arc::new(IpState {
                    count: AtomicU32::new(0),
                    last_access: Mutex::new(Instant::now()),
                })
            })
            .clone();

        let current = state.count.load(Ordering::SeqCst);
        if current >= self.max_per_ip {
            return Err(RateLimitError {
                ip,
                current,
                max: self.max_per_ip,
            });
        }

        state.count.fetch_add(1, Ordering::SeqCst);
        *state.last_access.lock() = Instant::now();
        Ok(())
    }

    /// Release a slot when a test completes
    pub fn release(&self, ip: IpAddr) {
        if let Some(state) = self.limits.get(&ip) {
            let prev = state.count.fetch_sub(1, Ordering::SeqCst);
            // Prevent underflow
            if prev == 0 {
                state.count.store(0, Ordering::SeqCst);
            }
        }
    }

    /// Get current count for an IP
    pub fn current_count(&self, ip: IpAddr) -> u32 {
        self.limits
            .get(&ip)
            .map(|s| s.count.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    /// Clean up stale entries
    pub fn cleanup(&self) {
        let now = Instant::now();
        self.limits.retain(|_, state| {
            let last = *state.last_access.lock();
            let count = state.count.load(Ordering::SeqCst);
            // Keep if active (count > 0) or recently accessed
            count > 0 || now.duration_since(last) < self.window
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
}
