//! Reconnect policy for machine transports.
//!
//! The schedule is fixed by the architecture (§二十六): `1s, 2s, 4s, 8s, 16s,
//! 30s` and then 30s forever, **plus jitter** so a fleet of clients reconnecting
//! after a network blip does not stampede the same machine.
//!
//! Two things must never happen:
//!
//! * a **changed host key** or an **auth failure** must not retry in a loop —
//!   no amount of waiting fixes a wrong key, and a retry loop is how a client
//!   ends up hammering someone else's machine;
//! * a machine that is merely **unreachable** must keep trying, because that is
//!   the case that resolves itself when the laptop wakes up.

use std::time::Duration;

use crate::ssh::ConnectError;

/// Backoff schedule, in seconds.
pub const BACKOFF_SECONDS: [u64; 6] = [1, 2, 4, 8, 16, 30];

#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub max_attempts: u32,
    pub multiplier: f32,
    /// Fraction of the delay to randomize (0.25 == ±25%).
    pub jitter: f32,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            max_attempts: u32::MAX,
            multiplier: 2.0,
            jitter: 0.25,
        }
    }
}

impl ReconnectConfig {
    /// Delay for a given attempt (0-indexed), before jitter.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let base = self.initial_delay.as_millis() as f32;
        let delay_ms = base * self.multiplier.powi(attempt as i32);
        let capped = delay_ms.min(self.max_delay.as_millis() as f32);
        Duration::from_millis(capped as u64)
    }

    /// Delay with jitter applied, derived from a caller-supplied entropy byte
    /// (tests pass a fixed value; production passes a clock or RNG).
    pub fn delay_with_jitter(&self, attempt: u32, entropy: u64) -> Duration {
        let base = self.delay_for_attempt(attempt).as_millis() as f64;
        let jitter_fraction = self.jitter as f64;
        // Map entropy into [-1, 1).
        let unit = ((entropy % 10_000) as f64 / 5_000.0) - 1.0;
        let factor = 1.0 + unit * jitter_fraction;
        Duration::from_millis((base * factor).max(1.0) as u64)
    }

    /// Should we stop retrying?
    pub fn exhausted(&self, attempt: u32) -> bool {
        attempt >= self.max_attempts
    }
}

/// What the manager should do after a failed attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconnectOutcome {
    /// Wait this long, then try again (attempt number increases).
    RetryAfter(Duration),
    /// Stop and ask the user (host key changed, auth failed, protocol skew).
    RequiresUserAction(String),
    /// Stop: attempts exhausted.
    GiveUp(String),
}

/// Decide what to do with a connection error.
pub fn plan(err: &ConnectError, attempt: u32, config: &ReconnectConfig, entropy: u64) -> ReconnectOutcome {
    if !err.is_retryable() {
        return ReconnectOutcome::RequiresUserAction(err.message());
    }
    if config.exhausted(attempt) {
        return ReconnectOutcome::GiveUp(format!(
            "reconnect gave up after {attempt} attempts: {}",
            err.message()
        ));
    }
    ReconnectOutcome::RetryAfter(config.delay_with_jitter(attempt, entropy))
}

/// Human label for the machine status line, e.g. "reconnecting in 4s".
pub fn describe_wait(delay: Duration) -> String {
    format!("reconnecting in {}s", delay.as_secs().max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_matches_the_architecture() {
        let config = ReconnectConfig::default();
        let seconds: Vec<u64> = (0..6)
            .map(|attempt| config.delay_for_attempt(attempt).as_secs())
            .collect();
        assert_eq!(seconds, BACKOFF_SECONDS.to_vec());
        // And it stays capped afterwards.
        assert_eq!(config.delay_for_attempt(9).as_secs(), 30);
    }

    #[test]
    fn jitter_stays_within_the_band_and_is_not_always_identical() {
        let config = ReconnectConfig::default();
        let base = config.delay_for_attempt(3); // 8s
        let low = config.delay_with_jitter(3, 0);
        let high = config.delay_with_jitter(3, 9_999);
        assert!(low < base, "low jitter should shorten the wait: {low:?}");
        assert!(high > base, "high jitter should lengthen the wait: {high:?}");
        // Never below one millisecond, never more than +25%.
        assert!(low >= Duration::from_millis(1));
        assert!(high <= Duration::from_millis(10_000));
    }

    #[test]
    fn host_key_change_never_retries() {
        let issue = crate::ssh::hostkey::HostKeyIssue::Changed {
            host: "devbox".into(),
            port: 22,
            key_type: "ED25519".into(),
            fingerprint: "SHA256:abc".into(),
            key_line: "devbox ssh-ed25519 AAAA".into(),
        };
        let err = ConnectError::HostKeyChanged(issue);
        match plan(&err, 0, &ReconnectConfig::default(), 0) {
            ReconnectOutcome::RequiresUserAction(message) => {
                assert!(message.contains("已改变"), "got {message}");
            }
            other => panic!("a changed host key must stop and ask: {other:?}"),
        }
    }

    #[test]
    fn auth_failure_never_retries() {
        let err = ConnectError::AuthFailed("Permission denied (publickey)".into());
        match plan(&err, 3, &ReconnectConfig::default(), 1234) {
            ReconnectOutcome::RequiresUserAction(_) => {}
            other => panic!("auth failure must not retry: {other:?}"),
        }
    }

    #[test]
    fn unreachable_retries_with_increasing_delays() {
        let err = ConnectError::Unreachable("Connection timed out".into());
        let config = ReconnectConfig::default();
        let first = match plan(&err, 0, &config, 0) {
            ReconnectOutcome::RetryAfter(delay) => delay,
            other => panic!("expected retry, got {other:?}"),
        };
        let second = match plan(&err, 1, &config, 0) {
            ReconnectOutcome::RetryAfter(delay) => delay,
            other => panic!("expected retry, got {other:?}"),
        };
        assert!(second > first);
        assert_eq!(describe_wait(first), "reconnecting in 1s");
    }

    #[test]
    fn attempts_can_be_bounded() {
        let config = ReconnectConfig {
            max_attempts: 2,
            ..Default::default()
        };
        let err = ConnectError::Unreachable("timeout".into());
        assert!(matches!(
            plan(&err, 0, &config, 0),
            ReconnectOutcome::RetryAfter(_)
        ));
        assert!(matches!(
            plan(&err, 2, &config, 0),
            ReconnectOutcome::GiveUp(_)
        ));
    }
}
