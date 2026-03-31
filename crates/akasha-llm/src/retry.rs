use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    None,
    Transient,
    RateLimited,
}

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            base_delay_ms: 500,
            max_delay_ms: 16_000,
            jitter_ms: 250,
        }
    }
}

impl RetryPolicy {
    pub fn classify_provider_error(err: &crate::provider::ProviderError) -> RetryClass {
        match err {
            crate::provider::ProviderError::Timeout => RetryClass::Transient,
            crate::provider::ProviderError::RateLimit => RetryClass::RateLimited,
            _ => RetryClass::None,
        }
    }

    pub fn delay_for_attempt(&self, attempt_index: u32) -> Duration {
        let exp = 1u64 << attempt_index.min(10);
        let raw = self.base_delay_ms.saturating_mul(exp).min(self.max_delay_ms);
        // deterministic low-cost jitter from attempt index; avoids synchronized retries.
        let jitter = if self.jitter_ms == 0 {
            0
        } else {
            ((attempt_index as u64 * 1103515245 + 12345) % (self.jitter_ms + 1)).min(self.jitter_ms)
        };
        Duration::from_millis(raw.saturating_add(jitter))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_increases_with_attempts() {
        let p = RetryPolicy::default();
        let d0 = p.delay_for_attempt(0);
        let d1 = p.delay_for_attempt(1);
        assert!(d1 >= d0);
    }
}
