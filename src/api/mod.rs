use std::time::Duration;

pub mod client;
pub mod error;
pub mod models;
pub mod stream;

/// Longest wait between reconnect attempts.
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Delay before reconnect attempt number `attempt` (0-based): 1 s, 2 s, 4 s …
/// capped at 30 s.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << attempt.min(5)).min(BACKOFF_CAP)
}
