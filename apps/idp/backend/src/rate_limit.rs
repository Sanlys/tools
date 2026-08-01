//! Fixed-window in-memory rate limiter for the passkey and token endpoints
//! -- a single homelab IDP has no need for a distributed limiter.

use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct RateLimiter {
    windows: Arc<DashMap<String, (u32, Instant)>>,
    max_requests: u32,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max_requests: u32, window_secs: u64) -> Self {
        Self {
            windows: Arc::new(DashMap::new()),
            max_requests,
            window: Duration::from_secs(window_secs),
        }
    }

    /// Returns `true` if the request is allowed, `false` if rate-limited.
    pub fn check(&self, key: &str) -> bool {
        let mut entry = self
            .windows
            .entry(key.to_string())
            .or_insert((0, Instant::now()));
        let (count, window_start) = entry.value_mut();

        if window_start.elapsed() >= self.window {
            *count = 1;
            *window_start = Instant::now();
            return true;
        }

        if *count < self.max_requests {
            *count += 1;
            true
        } else {
            false
        }
    }

    /// Remove all expired windows. Call periodically to avoid unbounded
    /// memory growth.
    pub fn gc(&self) {
        self.windows
            .retain(|_, (_, start)| start.elapsed() < self.window * 2);
    }
}
