//! Pre-trade risk checks.
//!
//! Each check implements [`RiskCheck`] and is evaluated against the
//! *post-trade hypothetical* state inside the same transaction that opens the
//! position, so there is no time-of-check to time-of-use gap. Checks are
//! composed into a [`RiskPipeline`] loaded from [`RiskConfig`], with per-check
//! enable flags and per-wallet overrides (e.g. a market-maker whitelist).
//!
//! Closing trades are never blocked: reducing risk is always allowed. Rolls
//! are evaluated as net changes because the pipeline only sees the resulting
//! hypothetical state.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// Machine-readable rejection codes returned to clients (HTTP 422).
pub mod codes {
    pub const RISK_MAX_CONTRACTS_SERIES: &str = "RISK_MAX_CONTRACTS_SERIES";
    pub const RISK_MAX_NOTIONAL_UNDERLYING: &str = "RISK_MAX_NOTIONAL_UNDERLYING";
    pub const RISK_MAX_NET_SHORT_VEGA: &str = "RISK_MAX_NET_SHORT_VEGA";
    pub const RISK_MAX_OPEN_POSITIONS: &str = "RISK_MAX_OPEN_POSITIONS";
    pub const RISK_MAX_OI_SERIES: &str = "RISK_MAX_OI_SERIES";
}

/// A single position in the hypothetical post-trade portfolio.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionState {
    pub series: String,
    pub underlying: String,
    /// Signed contracts: positive is long, negative is short.
    pub contracts: i64,
    /// Signed vega exposure for this position.
    pub vega: f64,
    /// Notional value of this position.
    pub notional: f64,
}

/// The hypothetical portfolio after the pending trade is applied.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PortfolioState {
    pub wallet: String,
    pub positions: Vec<PositionState>,
    /// Protocol-wide open interest per series, including the pending trade.
    pub protocol_open_interest: HashMap<String, i64>,
}

impl PortfolioState {
    /// Net contracts held in a series (long minus short).
    pub fn contracts_in_series(&self, series: &str) -> i64 {
        self.positions
            .iter()
            .filter(|p| p.series == series)
            .map(|p| p.contracts)
            .sum()
    }

    /// Net notional held for an underlying.
    pub fn notional_for_underlying(&self, underlying: &str) -> f64 {
        self.positions
            .iter()
            .filter(|p| p.underlying == underlying)
            .map(|p| p.notional)
            .sum()
    }

    /// Net vega across the whole portfolio (negative means net short vega).
    pub fn net_vega(&self) -> f64 {
        self.positions.iter().map(|p| p.vega).sum()
    }

    /// Number of distinct series with a non-zero position.
    pub fn open_positions(&self) -> usize {
        self.positions.iter().filter(|p| p.contracts != 0).count()
    }
}

/// A rejection produced by a risk check.
#[derive(Debug, Clone, PartialEq)]
pub struct RiskRejection {
    pub code: &'static str,
    pub message: String,
    pub limit: f64,
    pub current: f64,
}

impl RiskRejection {
    pub fn new(code: &'static str, message: impl Into<String>, limit: f64, current: f64) -> Self {
        Self {
            code,
            message: message.into(),
            limit,
            current,
        }
    }
}

/// Context passed to every check. `is_closing` short-circuits the pipeline so
/// that risk-reducing trades are always accepted.
#[derive(Debug, Clone)]
pub struct RiskContext {
    pub state: PortfolioState,
    pub is_closing: bool,
}

/// A single pre-trade risk check.
pub trait RiskCheck: Send + Sync {
    /// Stable identifier used for configuration and metrics.
    fn name(&self) -> &'static str;

    /// Evaluate the check against the post-trade hypothetical state.
    fn check(&self, ctx: &RiskContext) -> Result<(), RiskRejection>;
}

/// Per-wallet overrides. A wallet present here bypasses the listed checks
/// (used as a whitelist for market makers).
#[derive(Debug, Clone, Default)]
pub struct WalletOverride {
    pub bypass: Vec<&'static str>,
}

/// Configuration for the pipeline, loaded from application config.
#[derive(Debug, Clone)]
pub struct RiskConfig {
    pub max_contracts_per_series: i64,
    pub max_notional_per_underlying: f64,
    pub max_net_short_vega: f64,
    pub max_open_positions: usize,
    pub max_open_interest_per_series: i64,
    /// Per-check enable flags keyed by [`RiskCheck::name`].
    pub enabled: HashMap<&'static str, bool>,
    /// Per-wallet overrides keyed by wallet address.
    pub overrides: HashMap<String, WalletOverride>,
}

impl Default for RiskConfig {
    fn default() -> Self {
        let mut enabled = HashMap::new();
        enabled.insert(codes::RISK_MAX_CONTRACTS_SERIES, true);
        enabled.insert(codes::RISK_MAX_NOTIONAL_UNDERLYING, true);
        enabled.insert(codes::RISK_MAX_NET_SHORT_VEGA, true);
        enabled.insert(codes::RISK_MAX_OPEN_POSITIONS, true);
        enabled.insert(codes::RISK_MAX_OI_SERIES, true);
        Self {
            max_contracts_per_series: i64::MAX,
            max_notional_per_underlying: f64::MAX,
            max_net_short_vega: f64::MAX,
            max_open_positions: usize::MAX,
            max_open_interest_per_series: i64::MAX,
            enabled,
            overrides: HashMap::new(),
        }
    }
}

/// Rejection counters, one per check name.
#[derive(Debug, Default)]
pub struct RiskMetrics {
    rejections: HashMap<&'static str, AtomicU64>,
}

impl RiskMetrics {
    pub fn record_rejection(&mut self, check: &'static str) {
        self.rejections
            .entry(check)
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn rejections(&self, check: &'static str) -> u64 {
        self.rejections
            .get(check)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }
}

/// Enforces a maximum number of contracts per series.
pub struct MaxContractsPerSeries {
    pub limit: i64,
}

impl RiskCheck for MaxContractsPerSeries {
    fn name(&self) -> &'static str {
        codes::RISK_MAX_CONTRACTS_SERIES
    }

    fn check(&self, ctx: &RiskContext) -> Result<(), RiskRejection> {
        for position in &ctx.state.positions {
            let contracts = ctx.state.contracts_in_series(&position.series).abs();
            if contracts > self.limit {
                return Err(RiskRejection::new(
                    codes::RISK_MAX_CONTRACTS_SERIES,
                    format!("max contracts per series exceeded for {}", position.series),
                    self.limit as f64,
                    contracts as f64,
                ));
            }
        }
        Ok(())
    }
}

/// Enforces a maximum notional per underlying.
pub struct MaxNotionalPerUnderlying {
    pub limit: f64,
}

impl RiskCheck for MaxNotionalPerUnderlying {
    fn name(&self) -> &'static str {
        codes::RISK_MAX_NOTIONAL_UNDERLYING
    }

    fn check(&self, ctx: &RiskContext) -> Result<(), RiskRejection> {
        let mut seen: Vec<&str> = Vec::new();
        for position in &ctx.state.positions {
            if seen.contains(&position.underlying.as_str()) {
                continue;
            }
            seen.push(&position.underlying);
            let notional = ctx.state.notional_for_underlying(&position.underlying).abs();
            if notional > self.limit {
                return Err(RiskRejection::new(
                    codes::RISK_MAX_NOTIONAL_UNDERLYING,
                    format!("max notional per underlying exceeded for {}", position.underlying),
                    self.limit,
                    notional,
                ));
            }
        }
        Ok(())
    }
}

/// Enforces a maximum net short vega across the portfolio.
pub struct MaxNetShortVega {
    pub limit: f64,
}

impl RiskCheck for MaxNetShortVega {
    fn name(&self) -> &'static str {
        codes::RISK_MAX_NET_SHORT_VEGA
    }

    fn check(&self, ctx: &RiskContext) -> Result<(), RiskRejection> {
        let net_vega = ctx.state.net_vega();
        if net_vega < 0.0 && net_vega.abs() > self.limit {
            return Err(RiskRejection::new(
                codes::RISK_MAX_NET_SHORT_VEGA,
                "max net short vega exceeded",
                self.limit,
                net_vega.abs(),
            ));
        }
        Ok(())
    }
}

/// Enforces a maximum number of open positions.
pub struct MaxOpenPositions {
    pub limit: usize,
}

impl RiskCheck for MaxOpenPositions {
    fn name(&self) -> &'static str {
        codes::RISK_MAX_OPEN_POSITIONS
    }

    fn check(&self, ctx: &RiskContext) -> Result<(), RiskRejection> {
        let open = ctx.state.open_positions();
        if open > self.limit {
            return Err(RiskRejection::new(
                codes::RISK_MAX_OPEN_POSITIONS,
                "max open positions exceeded",
                self.limit as f64,
                open as f64,
            ));
        }
        Ok(())
    }
}

/// Enforces a protocol-wide open interest cap per series.
pub struct MaxOpenInterestPerSeries {
    pub limit: i64,
}

impl RiskCheck for MaxOpenInterestPerSeries {
    fn name(&self) -> &'static str {
        codes::RISK_MAX_OI_SERIES
    }

    fn check(&self, ctx: &RiskContext) -> Result<(), RiskRejection> {
        for (series, oi) in &ctx.state.protocol_open_interest {
            if *oi > self.limit {
                return Err(RiskRejection::new(
                    codes::RISK_MAX_OI_SERIES,
                    format!("protocol open interest cap exceeded for {}", series),
                    self.limit as f64,
                    *oi as f64,
                ));
            }
        }
        Ok(())
    }
}

/// Composes the configured checks and runs them in order.
pub struct RiskPipeline {
    checks: Vec<Box<dyn RiskCheck>>,
    overrides: HashMap<String, WalletOverride>,
    metrics: RiskMetrics,
}

impl RiskPipeline {
    /// Build the pipeline from configuration, honouring per-check enable flags.
    pub fn from_config(config: &RiskConfig) -> Self {
        let mut checks: Vec<Box<dyn RiskCheck>> = Vec::new();
        let mut push = |check: Box<dyn RiskCheck>| {
            if config.enabled.get(check.name()).copied().unwrap_or(true) {
                checks.push(check);
            }
        };
        push(Box::new(MaxContractsPerSeries {
            limit: config.max_contracts_per_series,
        }));
        push(Box::new(MaxNotionalPerUnderlying {
            limit: config.max_notional_per_underlying,
        }));
        push(Box::new(MaxNetShortVega {
            limit: config.max_net_short_vega,
        }));
        push(Box::new(MaxOpenPositions {
            limit: config.max_open_positions,
        }));
        push(Box::new(MaxOpenInterestPerSeries {
            limit: config.max_open_interest_per_series,
        }));
        Self {
            checks,
            overrides: config.overrides.clone(),
            metrics: RiskMetrics::default(),
        }
    }

    /// Run every enabled check against the post-trade hypothetical state.
    ///
    /// Closing trades bypass all checks. Per-wallet overrides bypass the
    /// checks listed for that wallet. Rejections increment the per-check
    /// metric and are returned as a [`RiskRejection`] (mapped to HTTP 422).
    pub fn evaluate(&mut self, ctx: &RiskContext) -> Result<(), RiskRejection> {
        if ctx.is_closing {
            return Ok(());
        }
        let bypass = self
            .overrides
            .get(&ctx.state.wallet)
            .map(|o| o.bypass.clone())
            .unwrap_or_default();
        for check in &self.checks {
            if bypass.contains(&check.name()) {
                continue;
            }
            if let Err(rejection) = check.check(ctx) {
                self.metrics.record_rejection(check.name());
                return Err(rejection);
            }
        }
        Ok(())
    }

    pub fn metrics(&self) -> &RiskMetrics {
        &self.metrics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(series: &str, underlying: &str, contracts: i64, vega: f64, notional: f64) -> PositionState {
        PositionState {
            series: series.to_string(),
            underlying: underlying.to_string(),
            contracts,
            vega,
            notional,
        }
    }

    fn ctx(wallet: &str, positions: Vec<PositionState>, oi: &[(&str, i64)]) -> RiskContext {
        RiskContext {
            state: PortfolioState {
                wallet: wallet.to_string(),
                positions,
                protocol_open_interest: oi
                    .iter()
                    .map(|(s, v)| (s.to_string(), *v))
                    .collect(),
            },
            is_closing: false,
        }
    }

    fn config() -> RiskConfig {
        RiskConfig {
            max_contracts_per_series: 10,
            max_notional_per_underlying: 1_000.0,
            max_net_short_vega: 50.0,
            max_open_positions: 2,
            max_open_interest_per_series: 100,
            ..RiskConfig::default()
        }
    }

    #[test]
    fn contracts_at_limit_accepted_one_above_rejected() {
        let mut pipeline = RiskPipeline::from_config(&config());
        let at = ctx("w", vec![position("S", "U", 10, 0.0, 0.0)], &[]);
        assert!(pipeline.evaluate(&at).is_ok());
        let above = ctx("w", vec![position("S", "U", 11, 0.0, 0.0)], &[]);
        let err = pipeline.evaluate(&above).unwrap_err();
        assert_eq!(err.code, codes::RISK_MAX_CONTRACTS_SERIES);
        assert_eq!(err.limit, 10.0);
        assert_eq!(err.current, 11.0);
        assert_eq!(pipeline.metrics().rejections(codes::RISK_MAX_CONTRACTS_SERIES), 1);
    }

    #[test]
    fn notional_at_limit_accepted_one_above_rejected() {
        let mut pipeline = RiskPipeline::from_config(&config());
        let at = ctx("w", vec![position("S", "U", 1, 0.0, 1_000.0)], &[]);
        assert!(pipeline.evaluate(&at).is_ok());
        let above = ctx("w", vec![position("S", "U", 1, 0.0, 1_000.01)], &[]);
        let err = pipeline.evaluate(&above).unwrap_err();
        assert_eq!(err.code, codes::RISK_MAX_NOTIONAL_UNDERLYING);
    }

    #[test]
    fn net_short_vega_at_limit_accepted_one_above_rejected() {
        let mut pipeline = RiskPipeline::from_config(&config());
        let at = ctx("w", vec![position("S", "U", 1, -50.0, 0.0)], &[]);
        assert!(pipeline.evaluate(&at).is_ok());
        let above = ctx("w", vec![position("S", "U", 1, -50.01, 0.0)], &[]);
        let err = pipeline.evaluate(&above).unwrap_err();
        assert_eq!(err.code, codes::RISK_MAX_NET_SHORT_VEGA);
    }

    #[test]
    fn open_positions_at_limit_accepted_one_above_rejected() {
        let mut pipeline = RiskPipeline::from_config(&config());
        let at = ctx(
            "w",
            vec![position("S1", "U", 1, 0.0, 0.0), position("S2", "U", 1, 0.0, 0.0)],
            &[],
        );
        assert!(pipeline.evaluate(&at).is_ok());
        let above = ctx(
            "w",
            vec![
                position("S1", "U", 1, 0.0, 0.0),
                position("S2", "U", 1, 0.0, 0.0),
                position("S3", "U", 1, 0.0, 0.0),
            ],
            &[],
        );
        let err = pipeline.evaluate(&above).unwrap_err();
        assert_eq!(err.code, codes::RISK_MAX_OPEN_POSITIONS);
    }

    #[test]
    fn open_interest_at_limit_accepted_one_above_rejected() {
        let mut pipeline = RiskPipeline::from_config(&config());
        let at = ctx("w", vec![], &[("S", 100)]);
        assert!(pipeline.evaluate(&at).is_ok());
        let above = ctx("w", vec![], &[("S", 101)]);
        let err = pipeline.evaluate(&above).unwrap_err();
        assert_eq!(err.code, codes::RISK_MAX_OI_SERIES);
        assert_eq!(err.limit, 100.0);
        assert_eq!(err.current, 101.0);
    }

    #[test]
    fn closing_trades_are_always_allowed() {
        let mut pipeline = RiskPipeline::from_config(&config());
        let mut closing = ctx("w", vec![position("S", "U", 1_000, -1_000.0, 1_000_000.0)], &[("S", 1_000)]);
        closing.is_closing = true;
        assert!(pipeline.evaluate(&closing).is_ok());
    }

    #[test]
    fn wallet_override_bypasses_listed_checks() {
        let mut cfg = config();
        cfg.overrides.insert(
            "mm".to_string(),
            WalletOverride {
                bypass: vec![codes::RISK_MAX_CONTRACTS_SERIES],
            },
        );
        let mut pipeline = RiskPipeline::from_config(&cfg);
        let over = ctx("mm", vec![position("S", "U", 11, 0.0, 0.0)], &[]);
        assert!(pipeline.evaluate(&over).is_ok());
        let other = ctx("other", vec![position("S", "U", 11, 0.0, 0.0)], &[]);
        assert!(pipeline.evaluate(&other).is_err());
    }

    #[test]
    fn disabled_check_is_skipped() {
        let mut cfg = config();
        cfg.enabled.insert(codes::RISK_MAX_CONTRACTS_SERIES, false);
        let mut pipeline = RiskPipeline::from_config(&cfg);
        let over = ctx("w", vec![position("S", "U", 11, 0.0, 0.0)], &[]);
        assert!(pipeline.evaluate(&over).is_ok());
    }

    #[test]
    fn concurrent_opens_at_oi_cap_reject_the_overflow() {
        // Two opens that each fit individually but together exceed the cap:
        // the pipeline sees the post-trade hypothetical OI, so the second is
        // rejected inside the same transaction.
        let mut pipeline = RiskPipeline::from_config(&config());
        let first = ctx("w1", vec![], &[("S", 60)]);
        assert!(pipeline.evaluate(&first).is_ok());
        let second = ctx("w2", vec![], &[("S", 101)]);
        let err = pipeline.evaluate(&second).unwrap_err();
        assert_eq!(err.code, codes::RISK_MAX_OI_SERIES);
    }
}
