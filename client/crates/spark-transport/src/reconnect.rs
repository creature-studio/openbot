//! Reconnection configuration with exponential backoff.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub max_attempts: u32,
    pub multiplier: f32,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            max_attempts: 20,
            multiplier: 2.0,
        }
    }
}

impl ReconnectConfig {
    /// Compute delay for a given attempt number (0-indexed).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let base = self.initial_delay.as_millis() as f32;
        let delay_ms = base * self.multiplier.powi(attempt as i32);
        let capped = delay_ms.min(self.max_delay.as_millis() as f32);
        Duration::from_millis(capped as u64)
    }
}
