//! Per-source polling schedule with failure backoff.
//!
//! Polite by default: the interval is the source's own (minimum 5 min,
//! enforced by the schema), and failures back off exponentially up to a day,
//! with deterministic jitter so sources added together do not poll together.

/// Next poll time after a successful fetch.
pub fn after_success(now_ms: i64, interval_secs: i64, source_id: i64) -> i64 {
    now_ms + interval_secs * 1000 + jitter_ms(source_id, interval_secs)
}

/// Next poll time after the `failures`-th consecutive failure (1-based):
/// interval × 2^(failures-1), capped at 24 h.
pub fn after_failure(now_ms: i64, interval_secs: i64, failures: i64, source_id: i64) -> i64 {
    let exp = (failures - 1).clamp(0, 16) as u32;
    let delay = interval_secs.saturating_mul(1 << exp).min(24 * 3600);
    now_ms + delay * 1000 + jitter_ms(source_id, delay)
}

/// Up to 10 % of the interval, derived from the source id.
fn jitter_ms(source_id: i64, interval_secs: i64) -> i64 {
    let spread = interval_secs * 100; // 10 % in ms
    if spread <= 0 {
        return 0;
    }
    (source_id.wrapping_mul(2_654_435_761) & i64::MAX) % spread
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_waits_one_interval_plus_bounded_jitter() {
        let next = after_success(0, 3600, 7);
        assert!((3_600_000..3_960_000).contains(&next));
    }

    #[test]
    fn failures_back_off_exponentially_and_cap_at_a_day() {
        let d = |n| after_failure(0, 3600, n, 0);
        assert_eq!(d(1), 3_600_000);
        assert_eq!(d(2), 7_200_000);
        assert_eq!(d(3), 14_400_000);
        assert_eq!(d(20), 86_400_000);
    }
}
