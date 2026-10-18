use std::time::Duration;

pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
pub const LOW_CODEX_WEEKLY_REFRESH_INTERVAL: Duration = Duration::from_secs(15);
pub const FAILURE_RETRY_BACKOFF: Duration = Duration::from_secs(60);
pub const STALE_AFTER: chrono::Duration = chrono::Duration::minutes(10);
