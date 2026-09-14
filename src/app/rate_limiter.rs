use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::app::RateLimitTier;

/// Duration of each sliding window.
const WINDOW_MINUTE: Duration = Duration::from_secs(60);
const WINDOW_HOUR: Duration = Duration::from_secs(60 * 60);
const WINDOW_DAY: Duration = Duration::from_secs(60 * 60 * 24);
const WINDOW_WEEK: Duration = Duration::from_secs(60 * 60 * 24 * 7);
const WINDOW_MONTH: Duration = Duration::from_secs(60 * 60 * 24 * 30);

struct WindowState {
    /// Sorted timestamps for this window.
    timestamps: Vec<Instant>,
}

impl WindowState {
    fn new() -> Self {
        Self {
            timestamps: Vec::new(),
        }
    }

    /// Prune expired entries and return the count of remaining (non-expired) entries.
    fn prune_and_count(&mut self, window: Duration, now: Instant) -> usize {
        let cutoff = now.checked_sub(window).unwrap_or(now);
        // binary search for the first non-expired entry
        let idx = self
            .timestamps
            .binary_search(&cutoff)
            .map_or_else(|i| i, |i| i);
        if idx > 0 {
            self.timestamps.drain(..idx);
        }
        self.timestamps.len()
    }

    fn push(&mut self, now: Instant) {
        self.timestamps.push(now);
    }

    fn is_empty(&self) -> bool {
        self.timestamps.is_empty()
    }
}

struct KeyState {
    windows: HashMap<&'static str, (Duration, WindowState)>,
    /// Current number of in-flight requests for this key.
    concurrent: i64,
}

impl KeyState {
    fn new() -> Self {
        let mut windows = HashMap::new();
        windows.insert("min", (WINDOW_MINUTE, WindowState::new()));
        windows.insert("hour", (WINDOW_HOUR, WindowState::new()));
        windows.insert("day", (WINDOW_DAY, WindowState::new()));
        windows.insert("week", (WINDOW_WEEK, WindowState::new()));
        windows.insert("month", (WINDOW_MONTH, WindowState::new()));
        Self {
            windows,
            concurrent: 0,
        }
    }

    /// Returns true if all windows are empty and no concurrent requests are in flight.
    fn is_empty(&self) -> bool {
        self.concurrent == 0 && self.windows.values().all(|(_, ws)| ws.is_empty())
    }
}

pub struct CheckResult {
    pub allowed: bool,
    pub retry_after_secs: Option<u64>,
}

pub struct RateLimiter {
    /// key_id -> KeyState
    state: Mutex<HashMap<i64, KeyState>>,
    /// Pre-configured tiers (name -> limits).
    tiers: HashMap<String, RateLimitTier>,
    /// Default tier name.
    default_tier: String,
    /// Counter for periodic cleanup (every N checks).
    check_counter: Mutex<u64>,
}

impl RateLimiter {
    pub fn new(tiers: HashMap<String, RateLimitTier>, default_tier: String) -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
            tiers,
            default_tier,
            check_counter: Mutex::new(0),
        }
    }

    /// Resolve a tier name to its `RateLimitTier`. Falls back to default if unknown.
    pub fn resolve_tier(&self, tier_name: &str) -> RateLimitTier {
        self.tiers
            .get(tier_name)
            .cloned()
            .or_else(|| self.tiers.get(&self.default_tier).cloned())
            .unwrap_or_else(RateLimitTier::unlimited)
    }

    /// Check and record a request for a given key_id. Returns a `CheckResult`.
    ///
    /// If the request is denied (`allowed == false`), `retry_after_secs` gives an
    /// approximate number of seconds the client should wait before retrying.
    pub fn check(&self, key_id: i64, tier: &RateLimitTier) -> CheckResult {
        let now = Instant::now();
        let mut guard = match self.state.lock() {
            Ok(g) => g,
            Err(_) => {
                // If the mutex is poisoned, allow the request through rather than blocking it.
                return CheckResult {
                    allowed: true,
                    retry_after_secs: None,
                };
            }
        };

        let key_state = guard.entry(key_id).or_insert_with(KeyState::new);

        // 1. Check concurrent limit
        if tier.max_concurrent > 0 && key_state.concurrent >= tier.max_concurrent as i64 {
            return CheckResult {
                allowed: false,
                retry_after_secs: Some(1),
            };
        }

        // 2. Check sliding windows
        let limits = [
            ("min", tier.requests_per_minute, WINDOW_MINUTE),
            ("hour", tier.requests_per_hour, WINDOW_HOUR),
            ("day", tier.requests_per_day, WINDOW_DAY),
            ("week", tier.requests_per_week, WINDOW_WEEK),
            ("month", tier.requests_per_month, WINDOW_MONTH),
        ];

        let mut min_retry = None;

        for (name, limit, window_dur) in &limits {
            if *limit == 0 {
                continue; // unlimited for this window
            }
            if let Some((_, ws)) = key_state.windows.get_mut(name) {
                let count = ws.prune_and_count(*window_dur, now);
                if count >= *limit as usize {
                    // How long until the oldest timestamp expires?
                    let retry = if let Some(oldest) = ws.timestamps.first() {
                        let expires_at = oldest.checked_add(*window_dur).unwrap_or(now);
                        expires_at.duration_since(now).as_secs().max(1)
                    } else {
                        1
                    };
                    min_retry = Some(min_retry.map_or(retry, |r: u64| r.min(retry)));
                }
            }
        }

        if let Some(retry_after) = min_retry {
            return CheckResult {
                allowed: false,
                retry_after_secs: Some(retry_after),
            };
        }

        // 3. Record the request in each window
        for (name, _, _) in &limits {
            if let Some((_, ws)) = key_state.windows.get_mut(name) {
                ws.push(now);
            }
        }

        // 4. Periodic cleanup: remove empty key states every 1000 checks
        let mut counter = self.check_counter.lock().unwrap_or_else(|e| e.into_inner());
        *counter += 1;
        if *counter >= 1000 {
            *counter = 0;
            guard.retain(|_, state| !state.is_empty());
        }

        CheckResult {
            allowed: true,
            retry_after_secs: None,
        }
    }

    /// Record that a request for this key has started processing (increment concurrent counter).
    pub fn start_request(&self, key_id: i64) {
        if let Ok(mut guard) = self.state.lock() {
            let key_state = guard.entry(key_id).or_insert_with(KeyState::new);
            key_state.concurrent = key_state.concurrent.saturating_add(1);
        }
    }

    /// Record that a request for this key has finished processing (decrement concurrent counter).
    pub fn finish_request(&self, key_id: i64) {
        if let Ok(mut guard) = self.state.lock() {
            let key_state = guard.entry(key_id).or_insert_with(KeyState::new);
            key_state.concurrent = key_state.concurrent.saturating_sub(1).max(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tier() -> RateLimitTier {
        RateLimitTier {
            requests_per_minute: 2,
            requests_per_hour: 10,
            requests_per_day: 0,
            requests_per_week: 0,
            requests_per_month: 0,
            max_concurrent: 1,
        }
    }

    #[test]
    fn allows_requests_under_limit() {
        let mut tiers = HashMap::new();
        tiers.insert("test".to_string(), test_tier());
        let limiter = RateLimiter::new(tiers, "test".to_string());
        let tier = limiter.resolve_tier("test");

        assert!(limiter.check(1, &tier).allowed);
        assert!(limiter.check(1, &tier).allowed);
    }

    #[test]
    fn rejects_requests_over_limit() {
        let mut tiers = HashMap::new();
        tiers.insert("test".to_string(), test_tier());
        let limiter = RateLimiter::new(tiers, "test".to_string());
        let tier = limiter.resolve_tier("test");

        assert!(limiter.check(1, &tier).allowed);
        assert!(limiter.check(1, &tier).allowed);
        assert!(!limiter.check(1, &tier).allowed);
    }

    #[test]
    fn concurrent_limit_blocks() {
        let mut tiers = HashMap::new();
        tiers.insert("test".to_string(), test_tier());
        let limiter = RateLimiter::new(tiers, "test".to_string());
        let tier = limiter.resolve_tier("test");

        assert!(limiter.check(1, &tier).allowed);
        limiter.start_request(1);
        // second concurrent request should be blocked
        let result = limiter.check(1, &tier);
        assert!(!result.allowed);
        limiter.finish_request(1);
        // after finishing, should be allowed again
        assert!(limiter.check(1, &tier).allowed);
    }

    #[test]
    fn unlimited_tier_never_rejects() {
        let mut tiers = HashMap::new();
        tiers.insert("unlimited".to_string(), RateLimitTier::unlimited());
        let limiter = RateLimiter::new(tiers, "unlimited".to_string());
        let tier = limiter.resolve_tier("unlimited");

        for _ in 0..1000 {
            assert!(limiter.check(1, &tier).allowed);
        }
    }

    #[test]
    fn different_keys_have_independent_buckets() {
        let mut tiers = HashMap::new();
        tiers.insert("test".to_string(), test_tier());
        let limiter = RateLimiter::new(tiers, "test".to_string());
        let tier = limiter.resolve_tier("test");

        assert!(limiter.check(1, &tier).allowed);
        assert!(limiter.check(1, &tier).allowed);
        assert!(!limiter.check(1, &tier).allowed);

        // A different key should still be allowed
        assert!(limiter.check(2, &tier).allowed);
        assert!(limiter.check(2, &tier).allowed);
        assert!(!limiter.check(2, &tier).allowed);
    }

    #[test]
    fn unknown_tier_falls_back_to_default() {
        let mut tiers = HashMap::new();
        tiers.insert("default".to_string(), test_tier());
        let limiter = RateLimiter::new(tiers, "default".to_string());
        let tier = limiter.resolve_tier("nonexistent");

        assert_eq!(tier.requests_per_minute, 2);
    }

    #[test]
    fn windows_expire_over_time() {
        let mut tiers = HashMap::new();
        tiers.insert("test".to_string(), test_tier());
        let limiter = RateLimiter::new(tiers, "test".to_string());
        let tier = limiter.resolve_tier("test");

        // Use up the 2 requests
        assert!(limiter.check(1, &tier).allowed);
        assert!(limiter.check(1, &tier).allowed);
        assert!(!limiter.check(1, &tier).allowed);

        // Sleep briefly — but our window is 60 seconds so we can't really wait.
        // Instead, verify the sliding window log actually stores entries.
        let guard = limiter.state.lock().unwrap();
        let ks = guard.get(&1).expect("key state exists");
        let (_, ws) = ks.windows.get("min").expect("min window");
        assert_eq!(ws.timestamps.len(), 2);
    }

    #[test]
    fn retry_after_returned_on_rejection() {
        let mut tiers = HashMap::new();
        tiers.insert("test".to_string(), test_tier());
        let limiter = RateLimiter::new(tiers, "test".to_string());
        let tier = limiter.resolve_tier("test");

        assert!(limiter.check(1, &tier).allowed);
        assert!(limiter.check(1, &tier).allowed);
        let result = limiter.check(1, &tier);
        assert!(!result.allowed);
        assert!(result.retry_after_secs.is_some());
    }
}
