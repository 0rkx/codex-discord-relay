use std::{collections::VecDeque, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;

use super::Clock;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct AttemptKey {
    pub owner_id: u64,
    pub guild_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptStatus {
    Allowed,
    Locked { retry_after_seconds: i64 },
}

#[derive(Debug, Default)]
struct AttemptState {
    failures: VecDeque<DateTime<Utc>>,
    locked_until: Option<DateTime<Utc>>,
}

pub struct AttemptLimiter {
    states: DashMap<AttemptKey, AttemptState>,
    max_failures: usize,
    window: Duration,
    lockout: Duration,
    clock: Arc<dyn Clock>,
}

impl AttemptLimiter {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            states: DashMap::new(),
            max_failures: 5,
            window: Duration::minutes(5),
            lockout: Duration::minutes(15),
            clock,
        }
    }

    #[must_use]
    pub fn status(&self, key: AttemptKey) -> AttemptStatus {
        let now = self.clock.now();
        let Some(mut state) = self.states.get_mut(&key) else {
            return AttemptStatus::Allowed;
        };
        if let Some(locked_until) = state.locked_until {
            if now < locked_until {
                return AttemptStatus::Locked {
                    retry_after_seconds: (locked_until - now).num_seconds().max(1),
                };
            }
            state.locked_until = None;
            state.failures.clear();
        }
        prune(&mut state.failures, now, self.window);
        AttemptStatus::Allowed
    }

    #[must_use]
    pub fn record_failure(&self, key: AttemptKey) -> AttemptStatus {
        let now = self.clock.now();
        let mut state = self.states.entry(key).or_default();
        if let Some(locked_until) = state.locked_until {
            if now < locked_until {
                return AttemptStatus::Locked {
                    retry_after_seconds: (locked_until - now).num_seconds().max(1),
                };
            }
            state.locked_until = None;
            state.failures.clear();
        }
        prune(&mut state.failures, now, self.window);
        state.failures.push_back(now);
        if state.failures.len() >= self.max_failures {
            let locked_until = now + self.lockout;
            state.locked_until = Some(locked_until);
            state.failures.clear();
            return AttemptStatus::Locked {
                retry_after_seconds: self.lockout.num_seconds(),
            };
        }
        AttemptStatus::Allowed
    }

    pub fn reset(&self, key: AttemptKey) {
        self.states.remove(&key);
    }
}

fn prune(failures: &mut VecDeque<DateTime<Utc>>, now: DateTime<Utc>, window: Duration) {
    let cutoff = now - window;
    while failures.front().is_some_and(|failure| *failure <= cutoff) {
        failures.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct ManualClock(Mutex<DateTime<Utc>>);

    impl ManualClock {
        fn advance(&self, delta: Duration) {
            let mut now = self.0.lock().unwrap();
            *now += delta;
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
    }

    fn limiter() -> (AttemptLimiter, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock(Mutex::new(Utc::now())));
        (AttemptLimiter::new(Arc::clone(&clock) as _), clock)
    }

    const KEY: AttemptKey = AttemptKey {
        owner_id: 1,
        guild_id: 2,
    };

    #[test]
    fn fifth_failure_in_window_locks_for_fifteen_minutes() {
        let (limiter, _) = limiter();
        for _ in 0..4 {
            assert_eq!(limiter.record_failure(KEY), AttemptStatus::Allowed);
        }
        assert_eq!(
            limiter.record_failure(KEY),
            AttemptStatus::Locked {
                retry_after_seconds: 15 * 60
            }
        );
        assert!(matches!(limiter.status(KEY), AttemptStatus::Locked { .. }));
    }

    #[test]
    fn sliding_window_prunes_old_failures() {
        let (limiter, clock) = limiter();
        for _ in 0..4 {
            assert_eq!(limiter.record_failure(KEY), AttemptStatus::Allowed);
        }
        // Once the first four failures age out of the 5-minute window they no
        // longer count toward the lockout threshold.
        clock.advance(Duration::minutes(5) + Duration::seconds(1));
        assert_eq!(limiter.record_failure(KEY), AttemptStatus::Allowed);
        assert_eq!(limiter.status(KEY), AttemptStatus::Allowed);
    }

    #[test]
    fn lockout_expires_and_clears_failure_history() {
        let (limiter, clock) = limiter();
        for _ in 0..5 {
            let _ = limiter.record_failure(KEY);
        }
        clock.advance(Duration::minutes(14));
        let AttemptStatus::Locked {
            retry_after_seconds,
        } = limiter.status(KEY)
        else {
            panic!("expected the lock to still be active after 14 minutes");
        };
        assert!(retry_after_seconds <= 60, "{retry_after_seconds}");

        clock.advance(Duration::minutes(1) + Duration::seconds(1));
        assert_eq!(limiter.status(KEY), AttemptStatus::Allowed);
        // The expired lock also discards prior failures: one new failure must
        // not re-lock immediately.
        assert_eq!(limiter.record_failure(KEY), AttemptStatus::Allowed);
    }

    #[test]
    fn reset_clears_lock_and_failures() {
        let (limiter, _) = limiter();
        for _ in 0..5 {
            let _ = limiter.record_failure(KEY);
        }
        limiter.reset(KEY);
        assert_eq!(limiter.status(KEY), AttemptStatus::Allowed);
        assert_eq!(limiter.record_failure(KEY), AttemptStatus::Allowed);
    }

    #[test]
    fn keys_are_isolated_per_owner_and_guild() {
        let (limiter, _) = limiter();
        for _ in 0..5 {
            let _ = limiter.record_failure(KEY);
        }
        let other = AttemptKey {
            owner_id: 1,
            guild_id: 3,
        };
        assert_eq!(limiter.status(other), AttemptStatus::Allowed);
        assert_eq!(limiter.record_failure(other), AttemptStatus::Allowed);
    }
}
