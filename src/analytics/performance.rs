//! Account performance time-series (issue #36).
//!
//! Snapshots every active account's equity (cash + collateral + unrealised
//! mark-to-market) and derives the equity curve, max drawdown, a Sharpe-like
//! ratio, win rate by strategy type and P&L attribution by Greek from the
//! stored snapshots.
//!
//! Snapshots are written in chunked transactions (see [`SNAPSHOT_CHUNK_SIZE`])
//! so the background job never holds write locks across the whole account set.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Accounts per transaction when writing snapshots. Keeps the job from holding
/// write locks for the entire account set at once.
pub const SNAPSHOT_CHUNK_SIZE: usize = 500;

/// Maximum number of points returned for any range; longer ranges are
/// downsampled so the payload stays bounded.
pub const MAX_POINTS: usize = 500;

/// Supported `range` query values for `GET /api/v1/account/performance`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    H24,
    D7,
    D30,
    All,
}

impl Range {
    /// Parse the `range` query parameter, defaulting to `24h`.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some("7d") => Range::D7,
            Some("30d") => Range::D30,
            Some("all") => Range::All,
            _ => Range::H24,
        }
    }

    /// Window length in hours, or `None` for the unbounded `all` range.
    pub fn window_hours(self) -> Option<i64> {
        match self {
            Range::H24 => Some(24),
            Range::D7 => Some(24 * 7),
            Range::D30 => Some(24 * 30),
            Range::All => None,
        }
    }
}

/// A single stored equity snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquitySnapshot {
    pub ts: DateTime<Utc>,
    pub cash: f64,
    pub collateral: f64,
    pub unrealized: f64,
    pub equity: f64,
}

/// One point on the returned equity curve.
#[derive(Debug, Clone, Serialize)]
pub struct EquityPoint {
    pub ts: DateTime<Utc>,
    pub equity: f64,
    pub drawdown: f64,
}

/// P&L attribution by Greek over the requested window.
#[derive(Debug, Clone, Default, Serialize)]
pub struct GreekAttribution {
    pub delta: f64,
    pub gamma: f64,
    pub theta: f64,
    pub vega: f64,
}

/// Win rate for a single strategy type.
#[derive(Debug, Clone, Serialize)]
pub struct StrategyWinRate {
    pub strategy: String,
    pub wins: u64,
    pub total: u64,
    pub win_rate: f64,
}

/// Response body for `GET /api/v1/account/performance`.
#[derive(Debug, Clone, Serialize)]
pub struct PerformanceResponse {
    pub range: String,
    pub points: Vec<EquityPoint>,
    pub max_drawdown: f64,
    pub sharpe: f64,
    pub win_rates: Vec<StrategyWinRate>,
    pub attribution: GreekAttribution,
}

/// Greeks captured at the start of an interval, used for attribution.
#[derive(Debug, Clone, Copy, Default)]
pub struct Greeks {
    pub delta: f64,
    pub gamma: f64,
    pub theta: f64,
    pub vega: f64,
}

/// Inputs whose change over an interval drives Greek attribution.
#[derive(Debug, Clone, Copy, Default)]
pub struct GreekInputs {
    pub spot: f64,
    pub iv: f64,
    pub time: f64,
}

/// Attribute the P&L of one interval using the Greeks at the start of the
/// interval multiplied by the change in the underlying inputs.
///
/// `theta` is the per-unit-time decay, so it is scaled by the elapsed time.
pub fn attribute_interval(start: Greeks, from: GreekInputs, to: GreekInputs) -> GreekAttribution {
    let d_spot = to.spot - from.spot;
    let d_iv = to.iv - from.iv;
    let d_time = to.time - from.time;

    GreekAttribution {
        delta: start.delta * d_spot,
        gamma: 0.5 * start.gamma * d_spot * d_spot,
        theta: start.theta * d_time,
        vega: start.vega * d_iv,
    }
}

/// Maximum peak-to-trough decline of an equity series, as a positive fraction.
///
/// Returns `0.0` for series with fewer than two points or a non-positive peak.
pub fn max_drawdown(equity: &[f64]) -> f64 {
    let mut peak = f64::NEG_INFINITY;
    let mut worst = 0.0_f64;
    for &value in equity {
        if value > peak {
            peak = value;
        }
        if peak > 0.0 {
            let dd = (peak - value) / peak;
            if dd > worst {
                worst = dd;
            }
        }
    }
    worst
}

/// Per-interval drawdown series aligned with `equity`.
pub fn drawdown_series(equity: &[f64]) -> Vec<f64> {
    let mut peak = f64::NEG_INFINITY;
    equity
        .iter()
        .map(|&value| {
            if value > peak {
                peak = value;
            }
            if peak > 0.0 {
                (peak - value) / peak
            } else {
                0.0
            }
        })
        .collect()
}

/// Annualised Sharpe-like ratio of a per-interval return series.
///
/// Uses a zero risk-free rate and annualises by the number of intervals per
/// year implied by `interval_hours`. Returns `0.0` when there is no variance.
pub fn sharpe(returns: &[f64], interval_hours: f64) -> f64 {
    if returns.len() < 2 || interval_hours <= 0.0 {
        return 0.0;
    }
    let n = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / n;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let std_dev = variance.sqrt();
    if std_dev <= f64::EPSILON {
        return 0.0;
    }
    let periods_per_year = (365.0 * 24.0) / interval_hours;
    (mean / std_dev) * periods_per_year.sqrt()
}

/// Per-interval simple returns of an equity series.
pub fn returns(equity: &[f64]) -> Vec<f64> {
    equity
        .windows(2)
        .filter_map(|w| {
            if w[0] > 0.0 {
                Some((w[1] - w[0]) / w[0])
            } else {
                None
            }
        })
        .collect()
}

/// Downsample a series to at most `max_points`, always keeping the first and
/// last points so the curve endpoints stay accurate.
pub fn downsample<T: Clone>(points: &[T], max_points: usize) -> Vec<T> {
    if points.len() <= max_points || max_points < 2 {
        return points.to_vec();
    }
    let step = (points.len() - 1) as f64 / (max_points - 1) as f64;
    (0..max_points)
        .map(|i| {
            let idx = (i as f64 * step).round() as usize;
            points[idx.min(points.len() - 1)].clone()
        })
        .collect()
}

/// Build the performance response from stored snapshots.
///
/// Accounts with fewer than two snapshots yield an empty curve and zeroed
/// statistics. Epoch resets (a curve that restarts) are handled naturally
/// because drawdown is computed from the running peak of the supplied series.
pub fn build_response(
    range: Range,
    snapshots: &[EquitySnapshot],
    win_rates: Vec<StrategyWinRate>,
    attribution: GreekAttribution,
) -> PerformanceResponse {
    let range_label = match range {
        Range::H24 => "24h",
        Range::D7 => "7d",
        Range::D30 => "30d",
        Range::All => "all",
    };

    if snapshots.len() < 2 {
        return PerformanceResponse {
            range: range_label.to_string(),
            points: Vec::new(),
            max_drawdown: 0.0,
            sharpe: 0.0,
            win_rates,
            attribution,
        };
    }

    let equity: Vec<f64> = snapshots.iter().map(|s| s.equity).collect();
    let drawdowns = drawdown_series(&equity);
    let interval_hours = interval_hours(snapshots);

    let points: Vec<EquityPoint> = snapshots
        .iter()
        .zip(drawdowns.iter())
        .map(|(s, dd)| EquityPoint {
            ts: s.ts,
            equity: s.equity,
            drawdown: *dd,
        })
        .collect();

    PerformanceResponse {
        range: range_label.to_string(),
        points: downsample(&points, MAX_POINTS),
        max_drawdown: max_drawdown(&equity),
        sharpe: sharpe(&returns(&equity), interval_hours),
        win_rates,
        attribution,
    }
}

/// Median spacing between snapshots in hours, used to annualise Sharpe.
fn interval_hours(snapshots: &[EquitySnapshot]) -> f64 {
    let mut gaps: Vec<f64> = snapshots
        .windows(2)
        .map(|w| (w[1].ts - w[0].ts).num_seconds() as f64 / 3600.0)
        .filter(|g| *g > 0.0)
        .collect();
    if gaps.is_empty() {
        return 1.0;
    }
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    gaps[gaps.len() / 2]
}

/// Filter snapshots to the requested range relative to `now`.
pub fn within_range(snapshots: &[EquitySnapshot], range: Range, now: DateTime<Utc>) -> Vec<EquitySnapshot> {
    match range.window_hours() {
        None => snapshots.to_vec(),
        Some(hours) => {
            let cutoff = now - Duration::hours(hours);
            snapshots.iter().filter(|s| s.ts >= cutoff).cloned().collect()
        }
    }
}

/// Aggregate win rate by strategy type from closed trades.
pub fn win_rates_by_strategy(trades: &[(String, f64)]) -> Vec<StrategyWinRate> {
    let mut buckets: HashMap<String, (u64, u64)> = HashMap::new();
    for (strategy, pnl) in trades {
        let entry = buckets.entry(strategy.clone()).or_insert((0, 0));
        entry.1 += 1;
        if *pnl > 0.0 {
            entry.0 += 1;
        }
    }
    let mut out: Vec<StrategyWinRate> = buckets
        .into_iter()
        .map(|(strategy, (wins, total))| StrategyWinRate {
            strategy,
            wins,
            total,
            win_rate: if total > 0 { wins as f64 / total as f64 } else { 0.0 },
        })
        .collect();
    out.sort_by(|a, b| a.strategy.cmp(&b.strategy));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(ts_offset_hours: i64, equity: f64) -> EquitySnapshot {
        let base = DateTime::from_timestamp(0, 0).unwrap();
        EquitySnapshot {
            ts: base + Duration::hours(ts_offset_hours),
            cash: equity,
            collateral: 0.0,
            unrealized: 0.0,
            equity,
        }
    }

    #[test]
    fn drawdown_on_known_series() {
        let series = [100.0, 120.0, 90.0, 110.0, 60.0];
        // Peak 120 -> trough 60 => 50%.
        assert!((max_drawdown(&series) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn drawdown_flat_series_is_zero() {
        assert_eq!(max_drawdown(&[100.0, 100.0, 100.0]), 0.0);
        assert_eq!(max_drawdown(&[100.0]), 0.0);
    }

    #[test]
    fn sharpe_zero_variance_is_zero() {
        assert_eq!(sharpe(&[0.01, 0.01, 0.01], 1.0), 0.0);
    }

    #[test]
    fn sharpe_positive_for_positive_mean() {
        let r = [0.01, 0.02, 0.015, 0.005];
        assert!(sharpe(&r, 1.0) > 0.0);
    }

    #[test]
    fn attribution_uses_start_greeks() {
        let start = Greeks { delta: 2.0, gamma: 1.0, theta: -0.5, vega: 3.0 };
        let from = GreekInputs { spot: 100.0, iv: 0.2, time: 0.0 };
        let to = GreekInputs { spot: 110.0, iv: 0.25, time: 1.0 };
        let a = attribute_interval(start, from, to);
        assert!((a.delta - 20.0).abs() < 1e-9);
        assert!((a.gamma - 50.0).abs() < 1e-9);
        assert!((a.theta + 0.5).abs() < 1e-9);
        assert!((a.vega - 0.15).abs() < 1e-9);
    }

    #[test]
    fn downsample_keeps_endpoints_and_cap() {
        let points: Vec<u32> = (0..1000).collect();
        let out = downsample(&points, MAX_POINTS);
        assert_eq!(out.len(), MAX_POINTS);
        assert_eq!(out[0], 0);
        assert_eq!(*out.last().unwrap(), 999);
    }

    #[test]
    fn fewer_than_two_snapshots_is_empty() {
        let resp = build_response(Range::H24, &[snap(0, 100.0)], Vec::new(), GreekAttribution::default());
        assert!(resp.points.is_empty());
        assert_eq!(resp.max_drawdown, 0.0);
        assert_eq!(resp.sharpe, 0.0);
    }

    #[test]
    fn range_filtering_uses_window() {
        let now = DateTime::from_timestamp(0, 0).unwrap() + Duration::hours(48);
        let snaps = vec![snap(0, 100.0), snap(30, 110.0), snap(47, 120.0)];
        let filtered = within_range(&snaps, Range::H24, now);
        assert_eq!(filtered.len(), 2);
        assert_eq!(within_range(&snaps, Range::All, now).len(), 3);
    }

    #[test]
    fn win_rate_buckets_by_strategy() {
        let trades = vec![
            ("covered_call".to_string(), 10.0),
            ("covered_call".to_string(), -5.0),
            ("straddle".to_string(), 3.0),
        ];
        let rates = win_rates_by_strategy(&trades);
        assert_eq!(rates.len(), 2);
        let cc = rates.iter().find(|r| r.strategy == "covered_call").unwrap();
        assert_eq!(cc.wins, 1);
        assert_eq!(cc.total, 2);
        assert!((cc.win_rate - 0.5).abs() < 1e-9);
    }
}
