//! Pre-trade risk control pipeline.
//!
//! Checks are composed into a [`RiskPipeline`] and evaluated against the
//! *post-trade hypothetical* state of an opening transaction. Every check
//! reads state that is passed in by the caller, which is expected to have been
//! loaded inside the same transaction as the trade itself, so there is no
//! time-of-check to time-of-use gap.
//!
//! Closing trades (and the closing legs of a roll) reduce risk and must never
//! be blocked by these limits. Callers are responsible for only invoking the
//! pipeline for the *net opening* portion of a trade; [`RiskPipeline::check`]
//! additionally short-circuits when the hypothetical delta is non-increasing.

use std::collections::HashMap;
use std::fmt;

/// Machine-readable rejection codes returned to clients (HTTP 422).
pub mod codes {
    pub const RISK_MAX_CONTRACTS_SERIES: &str = "RISK_MAX_CONTRACTS_SERIES";
    pub const RISK_MAX_NOTIONAL_UNDERLYING: &str = "RISK_MAX_NOTIONAL_UNDERLYING";
    pub const RISK_MAX_NET_SHORT_VEGA: &str = "RISK_MAX_NET_SHORT_VEGA";
    pub const RISK_MAX_OPEN_POSITIONS: &str = "RISK_MAX_OPEN_POSITIONS";
    pub const RISK_MAX_OI_SERIES: &str = "RISK_MAX_OI_SERIES";
}

/// A single limit value. `None` means the limit is disabled.
pub type Limit = Option<i64>;

/// Configuration for the pre-trade risk pipeline.
///
/// Loaded from configuration; each check has an explicit enable flag so
/// operators can turn individual controls on or off without redeploying.
#[derive(Debug, Clone, Default)]
pub struct RiskConfig {
    pub max_contracts_per_series: Limit,
    pub max_notional_per_underlying: Limit,
    pub max_net_short_vega: Limit,
    pub max_open_positions: Limit,
    pub max_oi_per_series: Limit,
    /// Per-wallet overrides (e.g. a whitelist for market makers).
    pub overrides: HashMap<String, WalletOverride>,
}

/// Per-wallet limit overrides. Any `Some` value replaces the global limit for
/// that wallet; `None` leaves the global limit in effect.
#[derive(Debug, Clone, Default)]
pub struct WalletOverride {
    pub max_contracts_per_series: Limit,
    pub max_notional_per_underlying: Limit,
    pub max_net_short_vega: Limit,
    pub max_open_positions: Limit,
    pub max_oi_per_series: Limit,
}

impl RiskConfig {
    /// Resolve the effective limit for a wallet, preferring the override.
    fn effective(&self, wallet: &str, pick: fn(&WalletOverride) -> Limit, global: Limit) -> Limit {
        match self.overrides.get(wallet) {
            Some(o) => pick(o).or(global),
            None => global,
        }
    }
}

/// The hypothetical post-trade state a check is evaluated against.
#[derive(Debug, Clone, Default)]
pub struct TradeState {
    pub wallet: String,
    pub series: String,
    pub underlying: String,
    /// Signed contracts after the trade (positive long, negative short).
    pub contracts_after: i64,
    /// Signed contracts before the trade.
    pub contracts_before: i64,
    /// Signed notional after the trade, in the smallest unit.
    pub notional_after: i64,
    /// Signed net vega after the trade (negative = short vega).
    pub net_vega_after: i64,
    /// Number of open positions for the wallet after the trade.
    pub open_positions_after: i64,
    /// Protocol-wide open interest for the series after the trade.
    pub series_oi_after: i64,
}

impl TradeState {
    /// True when the trade does not increase the wallet's absolute exposure.
    /// Closing trades and risk-reducing rolls are always allowed.
    pub fn is_risk_reducing(&self) -> bool {
        self.contracts_after.abs() <= self.contracts_before.abs()
    }
}

/// A rejection produced by a risk check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskRejection {
    pub code: &'static str,
    pub limit: i64,
    pub current: i64,
    pub message: String,
}

impl fmt::Display for RiskRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: limit {} exceeded (current {})",
            self.code, self.limit, self.current
        )
    }
}

impl std::error::Error for RiskRejection {}

/// A single pre-trade risk check.
///
/// Implementations must be pure with respect to the supplied [`TradeState`];
/// all state is read from the same transaction that produced it.
pub trait RiskCheck: Send + Sync {
    /// Stable identifier used for metrics and configuration.
    fn name(&self) -> &'static str;

    /// Evaluate the check against the post-trade hypothetical state.
    /// Returns `Some(rejection)` when the trade must be rejected.
    fn check(&self, cfg: &RiskConfig, state: &TradeState) -> Option<RiskRejection>;
}

/// Enforces the maximum contracts per series.
pub struct MaxContractsPerSeries;

impl RiskCheck for MaxContractsPerSeries {
    fn name(&self) -> &'static str {
        codes::RISK_MAX_CONTRACTS_SERIES
    }

    fn check(&self, cfg: &RiskConfig, state: &TradeState) -> Option<RiskRejection> {
        let limit = cfg.effective(
            &state.wallet,
            |o| o.max_contracts_per_series,
            cfg.max_contracts_per_series,
        )?;
        let current = state.contracts_after.abs();
        (current > limit).then(|| RiskRejection {
            code: codes::RISK_MAX_CONTRACTS_SERIES,
            limit,
            current,
            message: format!(
                "max contracts per series exceeded for {}",
                state.series
            ),
        })
    }
}

/// Enforces the maximum notional per underlying.
pub struct MaxNotionalPerUnderlying;

impl RiskCheck for MaxNotionalPerUnderlying {
    fn name(&self) -> &'static str {
        codes::RISK_MAX_NOTIONAL_UNDERLYING
    }

    fn check(&self, cfg: &RiskConfig, state: &TradeState) -> Option<RiskRejection> {
        let limit = cfg.effective(
            &state.wallet,
            |o| o.max_notional_per_underlying,
            cfg.max_notional_per_underlying,
        )?;
        let current = state.notional_after.abs();
        (current > limit).then(|| RiskRejection {
            code: codes::RISK_MAX_NOTIONAL_UNDERLYING,
            limit,
            current,
            message: format!(
                "max notional per underlying exceeded for {}",
                state.underlying
            ),
        })
    }
}

/// Enforces the maximum net short vega.
pub struct MaxNetShortVega;

impl RiskCheck for MaxNetShortVega {
    fn name(&self) -> &'static str {
        codes::RISK_MAX_NET_SHORT_VEGA
    }

    fn check(&self, cfg: &RiskConfig, state: &TradeState) -> Option<RiskRejection> {
        let limit = cfg.effective(
            &state.wallet,
            |o| o.max_net_short_vega,
            cfg.max_net_short_vega,
        )?;
        // Short vega is negative; compare the magnitude of the short side.
        let current = (-state.net_vega_after).max(0);
        (current > limit).then(|| RiskRejection {
            code: codes::RISK_MAX_NET_SHORT_VEGA,
            limit,
            current,
            message: "max net short vega exceeded".to_string(),
        })
    }
}

/// Enforces the maximum number of open positions per wallet.
pub struct MaxOpenPositions;

impl RiskCheck for MaxOpenPositions {
    fn name(&self) -> &'static str {
        codes::RISK_MAX_OPEN_POSITIONS
    }

    fn check(&self, cfg: &RiskConfig, state: &TradeState) -> Option<RiskRejection> {
        let limit = cfg.effective(
            &state.wallet,
            |o| o.max_open_positions,
            cfg.max_open_positions,
        )?;
        let current = state.open_positions_after;
        (current > limit).then(|| RiskRejection {
            code: codes::RISK_MAX_OPEN_POSITIONS,
            limit,
            current,
            message: "max open positions exceeded".to_string(),
        })
    }
}

/// Enforces the protocol-wide open interest cap per series.
pub struct MaxOiPerSeries;

impl RiskCheck for MaxOiPerSeries {
    fn name(&self) -> &'static str {
        codes::RISK_MAX_OI_SERIES
    }

    fn check(&self, cfg: &RiskConfig, state: &TradeState) -> Option<RiskRejection> {
        let limit = cfg.effective(
            &state.wallet,
            |o| o.max_oi_per_series,
            cfg.max_oi_per_series,
        )?;
        let current = state.series_oi_after;
        (current > limit).then(|| RiskRejection {
            code: codes::RISK_MAX_OI_SERIES,
            limit,
            current,
            message: format!("protocol open interest cap exceeded for {}", state.series),
        })
    }
}

/// A composed, ordered set of risk checks.
pub struct RiskPipeline {
    checks: Vec<Box<dyn RiskCheck>>,
}

impl RiskPipeline {
    /// Build the default pipeline from configuration. Checks whose limit is
    /// `None` (and has no override) are still registered but are no-ops, which
    /// keeps metrics stable across configuration changes.
    pub fn from_config(_cfg: &RiskConfig) -> Self {
        Self {
            checks: vec![
                Box::new(MaxContractsPerSeries),
                Box::new(MaxNotionalPerUnderlying),
                Box::new(MaxNetShortVega),
                Box::new(MaxOpenPositions),
                Box::new(MaxOiPerSeries),
            ],
        }
    }

    /// Run every check against the post-trade hypothetical state.
    ///
    /// Risk-reducing trades (closes and net-reducing rolls) are always allowed.
    /// Returns the first rejection, or `Ok(())` when the trade passes.
    pub fn check(
        &self,
        cfg: &RiskConfig,
        state: &TradeState,
    ) -> Result<(), RiskRejection> {
        if state.is_risk_reducing() {
            return Ok(());
        }
        for check in &self.checks {
            if let Some(rejection) = check.check(cfg, state) {
                return Err(rejection);
            }
        }
        Ok(())
    }

    /// Names of the registered checks, in evaluation order.
    pub fn check_names(&self) -> Vec<&'static str> {
        self.checks.iter().map(|c| c.name()).collect()
    }
}

impl Default for RiskPipeline {
    fn default() -> Self {
        Self::from_config(&RiskConfig::default())
    }
}
