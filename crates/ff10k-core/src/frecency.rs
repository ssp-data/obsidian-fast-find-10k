//! Frecency: recency-weighted access count (spec §6.4).

const DAY_SECS: f64 = 86_400.0;
const WINDOW_DAYS: f64 = 30.0;
/// `exp(-DECAY × days)` halves every 10 days.
const DECAY: f64 = 0.0693;
const SOFT_CAP: f64 = 10.0;

/// Sum of `exp(-0.0693 × days_since)` over accesses in the last 30 days; above 10 it grows as
/// `10 + sqrt(x - 10)`. Timestamps in the future count as "now".
pub fn frecency_score(timestamps_secs: &[f64], now_secs: f64) -> f32 {
    let x: f64 = timestamps_secs
        .iter()
        .map(|&t| ((now_secs - t) / DAY_SECS).max(0.0))
        .filter(|&days| days <= WINDOW_DAYS)
        .map(|days| (-DECAY * days).exp())
        .sum();
    (if x > SOFT_CAP { SOFT_CAP + (x - SOFT_CAP).sqrt() } else { x }) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 10_000_000.0;

    #[test]
    fn frecency_formula() {
        assert!((frecency_score(&[NOW], NOW) - 1.0).abs() < 1e-4);
        assert!((frecency_score(&[NOW - 10.0 * DAY_SECS], NOW) - 0.5).abs() < 0.01);
        assert_eq!(frecency_score(&[NOW - 40.0 * DAY_SECS], NOW), 0.0);
        let thirty = vec![NOW; 30];
        let want = 10.0 + 20f32.sqrt();
        assert!((frecency_score(&thirty, NOW) - want).abs() < 1e-3);
        assert!((frecency_score(&[NOW + 5.0 * DAY_SECS], NOW) - 1.0).abs() < 1e-4);
        assert_eq!(frecency_score(&[], NOW), 0.0);
    }
}
