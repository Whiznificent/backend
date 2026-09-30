//! Liquidation engine for under-margined accounts.
//!
//! A background risk monitor recomputes maintenance margin for accounts with
//! short exposure on every price tick. Accounts whose equity falls below
//! maintenance are flagged (`margin_call`) and liquidated step by step, in
//! order of risk reduction, until the account is healthy again.
//!
//! Each liquidation step is its own transaction. A configurable penalty (bps)
//! is credited to the insurance-fund ledger account, and every step is
//! recorded in the `liquidations` table.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::events::{Event, EventBus};
use crate::margin::{self, MarginAccount, MarginError};
use crate::positions::{self, Position, PositionError};
use crate::prices::{PriceFeed, PriceError};

/// Health state of an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    /// Equity is at or above initial margin.
    Healthy,
    /// Equity is between maintenance and initial margin.
    MarginCall,
    /// Equity is below maintenance margin; liquidation is in progress.
    Liquidating,
}

/// Configuration for the liquidation engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidationConfig {
    /// Liquidation penalty in basis points, credited to the insurance fund.
    pub penalty_bps: u32,
    /// Ledger account id of the insurance fund.
    pub insurance_fund_account: String,
}

impl Default for LiquidationConfig {
    fn default() -> Self {
        Self {
            penalty_bps: 100,
            insurance_fund_account: "insurance_fund".to_string(),
        }
    }
}

/// A single recorded liquidation step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidationRecord {
    pub account: String,
    pub position_id: String,
    pub symbol: String,
    pub quantity_closed: i64,
    pub price: i64,
    pub penalty: i64,
    pub equity_before: i64,
    pub equity_after: i64,
    pub maintenance_before: i64,
    pub maintenance_after: i64,
}

/// Errors surfaced by the liquidation engine.
#[derive(Debug, thiserror::Error)]
pub enum LiquidationError {
    #[error("price feed error: {0}")]
    Price(#[from] PriceError),
    #[error("margin error: {0}")]
    Margin(#[from] MarginError),
    #[error("position error: {0}")]
    Position(#[from] PositionError),
    #[error("account not found: {0}")]
    AccountNotFound(String),
}

/// Incremental index of accounts that currently carry short exposure.
///
/// Accounts with no short exposure are never scanned.
#[derive(Debug, Default)]
pub struct AtRiskIndex {
    accounts: HashMap<String, ()>,
}

impl AtRiskIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an account that has (or may have) short exposure.
    pub fn track(&mut self, account: &str) {
        self.accounts.insert(account.to_string(), ());
    }

    /// Remove an account once it no longer has short exposure.
    pub fn untrack(&mut self, account: &str) {
        self.accounts.remove(account);
    }

    pub fn contains(&self, account: &str) -> bool {
        self.accounts.contains_key(account)
    }

    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.accounts.keys()
    }
}

/// Compute the health state of an account given its equity and margin figures.
pub fn health_state(equity: i64, initial_margin: i64, maintenance_margin: i64) -> HealthState {
    if equity < maintenance_margin {
        HealthState::Liquidating
    } else if equity < initial_margin {
        HealthState::MarginCall
    } else {
        HealthState::Healthy
    }
}

/// A single liquidation step: close the position that reduces maintenance
/// margin the most per unit of cost, then re-evaluate.
///
/// Returns `Ok(None)` when the account is already healthy.
pub fn liquidate_once(
    state: &mut LiquidationState,
    config: &LiquidationConfig,
    events: &EventBus,
) -> Result<Option<LiquidationRecord>, LiquidationError> {
    let account = state.account.clone();

    // Never liquidate on a stale or degraded price.
    if state.price_feed.is_stale() || state.price_feed.is_degraded() {
        return Ok(None);
    }

    let equity = margin::equity(&state.margin_account)?;
    let initial = margin::initial_margin(&state.margin_account)?;
    let maintenance = margin::maintenance_margin(&state.margin_account)?;

    let health = health_state(equity, initial, maintenance);
    if health == HealthState::Healthy {
        return Ok(None);
    }

    if health == HealthState::MarginCall {
        events.publish(Event::MarginCall {
            account: account.clone(),
            equity,
            maintenance_margin: maintenance,
        });
    }

    // Pick the short position that reduces maintenance margin the most per
    // unit of cost. Positions with no short exposure are skipped.
    let candidate = state
        .positions
        .iter()
        .filter(|p| p.status == positions::PositionStatus::Open && p.quantity < 0)
        .min_by_key(|p| {
            let reduction = margin::maintenance_reduction_per_unit(&state.margin_account, p);
            let cost = p.quantity.unsigned_abs().max(1);
            // Lower ratio => better risk reduction per unit of cost.
            reduction.saturating_mul(1_000_000) / cost as i64
        })
        .cloned();

    let Some(position) = candidate else {
        // No short exposure left to close; nothing more to do.
        return Ok(None);
    };

    let price = state.price_feed.price(&position.symbol)?;
    let quantity_closed = position.quantity;

    // Each step is its own transaction. The optimistic `WHERE status='open'`
    // guard protects against a user closing the position concurrently.
    let closed = positions::close_position_in_tx(
        &mut state.tx,
        &position.id,
        price,
        positions::PositionStatus::Open,
    )?;

    if !closed {
        // The user closed the position first; re-evaluate on the next tick.
        return Ok(None);
    }

    let penalty = (price
        .saturating_mul(quantity_closed.unsigned_abs() as i64)
        .saturating_mul(config.penalty_bps as i64))
        / 10_000;

    state
        .ledger
        .credit(&config.insurance_fund_account, penalty)?;

    let equity_after = margin::equity(&state.margin_account)?;
    let maintenance_after = margin::maintenance_margin(&state.margin_account)?;

    let record = LiquidationRecord {
        account: account.clone(),
        position_id: position.id.clone(),
        symbol: position.symbol.clone(),
        quantity_closed,
        price,
        penalty,
        equity_before: equity,
        equity_after,
        maintenance_before: maintenance,
        maintenance_after,
    };

    state.records.push(record.clone());

    // Negative equity after full liquidation is bad debt absorbed by the
    // insurance fund.
    if equity_after < 0 {
        state
            .ledger
            .credit(&config.insurance_fund_account, -equity_after)?;
    }

    events.publish(Event::Liquidated {
        account,
        position_id: position.id,
        price,
        penalty,
        equity_before: equity,
        equity_after,
    });

    Ok(Some(record))
}

/// Mutable state threaded through a liquidation run.
pub struct LiquidationState {
    pub account: String,
    pub margin_account: MarginAccount,
    pub positions: Vec<Position>,
    pub price_feed: PriceFeed,
    pub ledger: Ledger,
    pub tx: Tx,
    pub records: Vec<LiquidationRecord>,
}

/// Minimal ledger handle used to credit the insurance fund.
pub trait Ledger {
    fn credit(&mut self, account: &str, amount: i64) -> Result<(), MarginError>;
}

/// Minimal transaction handle passed to `close_position_in_tx`.
pub struct Tx;
