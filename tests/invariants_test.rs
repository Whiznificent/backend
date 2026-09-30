//! Property-based, state-machine test harness for the trading ledger
//! (issue #118).
//!
//! A reference model — a pure in-memory simulator of the account/position
//! ledger — is stepped in lockstep with the real system under test (the
//! real router over a throwaway SQLite database, via `TestApp`). Both are
//! fed the same randomly generated sequence of operations (open, close,
//! roll, strategy execute/close, price moves), and after every step the
//! global invariants are re-checked against the live database:
//!
//! * the model's predicted balance/collateral/open-leg count match the
//!   server's, and the server's acceptance/rejection of each operation
//!   matches the model's buying-power prediction;
//! * conservation: `balance == initial_funds + net premium cash flows`
//!   (computed straight from the position rows, so a dropped balance
//!   update shows up). Note `collateral_locked` deliberately does not
//!   enter this equation: it is a reservation against buying power
//!   (`balance - collateral_locked`), never a debit from `balance`, so
//!   locking/releasing it moves no cash; its invariant is the one below;
//! * no negative balances, collateral, or available buying power;
//! * `collateral_locked == sum of the open positions' collateral`;
//! * `realized_pnl` agrees with the entry/close premiums on every
//!   closed/rolled row;
//! * status transitions stay within `open`/`closed`/`rolled`;
//! * no cross-wallet visibility (every row returned for a wallet belongs
//!   to that wallet).
//!
//! Determinism: `TestApp` never spawns the background price simulator, so
//! spot/vol only move when a `PriceMove` operation does — and it moves the
//! model's price and the server's price together. Every other input is
//! fixed (seed constants, integer strikes/expiries/contracts), so a failing
//! sequence is reproducible, and proptest shrinks it to a minimal case and
//! persists it under `proptest-regressions/`.
//!
//! Case count: bounded by default (see `invariant_cases`) so `cargo test`
//! stays fast; the nightly workflow raises it with `PROPTEST_CASES`.
//!
//! Deliberate injected-bug demo: `checker_tests` at the bottom of this file
//! exercises the pure invariant checker against a ledger with a skipped
//! collateral release on roll and against a ledger with a dropped balance
//! update, asserting it rejects both. To reproduce end-to-end, comment out
//! the `new_collateral_locked = account.collateral_locked - position.collateral`
//! release in `close_position_in_tx` and this harness fails on the next run.

mod common;

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;

use axum::http::StatusCode;
use common::TestApp;
use proptest::prelude::*;
use serde_json::{json, Value};

use zenith_backend::collateral::collateral_required;
use zenith_backend::{black_scholes, smile_vol, BSInputs};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Matches the `accounts.balance` default in migration 0001.
const INITIAL_BALANCE: f64 = 100_000.0;
/// Distinct wallets stepped in parallel, to exercise cross-wallet isolation.
/// Kept at 4: each case performs `WALLETS` logins (8 rate-limited auth
/// requests, under the auth endpoints' burst of 10 per IP) and each wallet
/// may receive at most `MAX_OPS - 1` mutating requests (under the mutation
/// endpoints' burst of 20 per wallet). Raising this without also keeping
/// those two budgets in mind makes the harness race the rate limiter.
const WALLETS: usize = 4;
/// Exclusive upper bound on the per-case operation count (0..MAX_OPS).
const MAX_OPS: usize = 16;
const UNDERLYINGS: [&str; 4] = ["BTC", "ETH", "SOL", "XLM"];
/// Relative tolerance for float comparisons. Pricing is deterministic and
/// both sides run the same formulas, so exact equality is expected in
/// practice; the tolerance only guards against a different (but equivalent)
/// order of the same additions after optimizations.
const TOL: f64 = 1e-6;

fn invariant_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(24)
}

// ─── Operations ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum Op {
    Open {
        wallet: usize,
        underlying: usize,
        moneyness: i32,
        expiry: i32,
        call: bool,
        short: bool,
        contracts: f64,
    },
    Close {
        wallet: usize,
        pick: usize,
    },
    Roll {
        wallet: usize,
        pick: usize,
        moneyness: i32,
        expiry: i32,
    },
    Strategy {
        wallet: usize,
        underlying: usize,
        expiry: i32,
        contracts: f64,
        short_put: bool,
    },
    CloseStrategy {
        wallet: usize,
        pick: usize,
    },
    PriceMove {
        underlying: usize,
        pct: i32,
    },
}

fn arb_op() -> impl Strategy<Value = Op> {
    proptest::prop_oneof![
        8 => (
            0usize..WALLETS,
            0usize..UNDERLYINGS.len(),
            -15i32..16,
            1i32..91,
            any::<bool>(),
            any::<bool>(),
            1u32..4,
        )
            .prop_map(|(wallet, underlying, moneyness, expiry, call, short, contracts)| Op::Open {
                wallet,
                underlying,
                moneyness,
                expiry,
                call,
                short,
                contracts: f64::from(contracts),
            }),
        5 => (0usize..WALLETS, 0usize..8).prop_map(|(wallet, pick)| Op::Close { wallet, pick }),
        4 => (0usize..WALLETS, 0usize..8, -15i32..16, 1i32..91)
            .prop_map(|(wallet, pick, moneyness, expiry)| Op::Roll { wallet, pick, moneyness, expiry }),
        5 => (0usize..WALLETS, 0usize..UNDERLYINGS.len(), 1i32..91, 1u32..4, any::<bool>())
            .prop_map(|(wallet, underlying, expiry, contracts, short_put)| Op::Strategy {
                wallet,
                underlying,
                expiry,
                contracts: f64::from(contracts),
                short_put,
            }),
        4 => (0usize..WALLETS, 0usize..8)
            .prop_map(|(wallet, pick)| Op::CloseStrategy { wallet, pick }),
        5 => (0usize..UNDERLYINGS.len(), -10i32..11)
            .prop_map(|(underlying, pct)| Op::PriceMove { underlying, pct }),
    ]
}

// ─── Reference model ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Leg {
    id: String,
    underlying: String,
    strike: f64,
    expiry_days: f64,
    call: bool,
    short: bool,
    contracts: f64,
    collateral: f64,
    strategy_id: Option<String>,
    closed: bool,
}

#[derive(Debug)]
struct WalletModel {
    balance: f64,
    collateral: f64,
    legs: Vec<Leg>,
    strategy_ids: Vec<String>,
}

impl WalletModel {
    fn new() -> Self {
        Self {
            balance: INITIAL_BALANCE,
            collateral: 0.0,
            legs: Vec::new(),
            strategy_ids: Vec::new(),
        }
    }

    fn open_indices(&self) -> Vec<usize> {
        self.legs
            .iter()
            .enumerate()
            .filter(|(_, leg)| !leg.closed)
            .map(|(index, _)| index)
            .collect()
    }
}

/// Deterministic option premium: the same pure pricing the handlers use
/// (`black_scholes` over `smile_vol`), evaluated against the harness's own
/// view of spot/vol so the model can predict what the server will charge
/// before it charges it.
fn premium(
    prices: &HashMap<String, (f64, f64)>,
    underlying: &str,
    strike: f64,
    expiry_days: f64,
    call: bool,
) -> f64 {
    let (spot, base_vol) = prices[underlying];
    let vol = smile_vol(base_vol, strike / spot);
    black_scholes(&BSInputs {
        spot,
        strike,
        vol,
        t: expiry_days / 365.0,
        r: 0.05,
        is_call: call,
    })
    .premium
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn pick_from(open: &[usize], pick: usize) -> Option<usize> {
    if open.is_empty() {
        None
    } else {
        Some(open[pick % open.len()])
    }
}

// ─── Pure invariant checker (unit-testable in isolation) ─────────────────────

#[derive(Debug, Clone)]
struct PositionRow {
    wallet_address: String,
    position_type: String,
    contracts: f64,
    entry_premium: f64,
    close_premium: Option<f64>,
    realized_pnl: Option<f64>,
    status: String,
    collateral: f64,
}

#[derive(Debug)]
struct WalletSnapshot {
    balance: f64,
    collateral: f64,
    model_balance: f64,
    model_collateral: f64,
    model_open: usize,
    rows: Vec<PositionRow>,
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    let tolerance = TOL * (1.0 + actual.abs() + expected.abs());
    assert!(
        (actual - expected).abs() <= tolerance,
        "invariant violated: {what} was {actual}, expected {expected}"
    );
}

/// Checks every ledger invariant for one wallet. Panics on the first
/// violation; proptest turns that into a shrunk, persisted counterexample.
fn check_ledger_invariants(wallet_address: &str, snapshot: &WalletSnapshot) {
    // No negative balances or collateral, and buying power stays solvent.
    assert!(
        snapshot.balance >= -TOL,
        "invariant violated: negative balance {}",
        snapshot.balance
    );
    assert!(
        snapshot.collateral >= -TOL,
        "invariant violated: negative locked collateral {}",
        snapshot.collateral
    );
    assert!(
        snapshot.balance - snapshot.collateral >= -TOL,
        "invariant violated: negative buying power (balance {} < collateral {})",
        snapshot.balance,
        snapshot.collateral
    );

    // The reference model and the server agree.
    assert_close(snapshot.balance, snapshot.model_balance, "balance");
    assert_close(
        snapshot.collateral,
        snapshot.model_collateral,
        "locked collateral",
    );

    // Locked collateral is exactly the sum over open positions.
    let open_rows: Vec<&PositionRow> = snapshot
        .rows
        .iter()
        .filter(|row| row.status == "open")
        .collect();
    assert_eq!(
        open_rows.len(),
        snapshot.model_open,
        "invariant violated: open-leg count {} != model {}",
        open_rows.len(),
        snapshot.model_open
    );
    let open_collateral: f64 = open_rows.iter().map(|row| row.collateral).sum();
    assert_close(
        open_collateral,
        snapshot.collateral,
        "collateral vs. open legs",
    );

    // Conservation, computed from the immutable trade ledger.
    let mut cash_flows = 0.0;
    for row in &snapshot.rows {
        assert_eq!(
            row.wallet_address, wallet_address,
            "invariant violated: cross-wallet row visible ({} for {wallet_address})",
            row.wallet_address
        );
        assert!(
            row.collateral >= -TOL,
            "invariant violated: negative collateral on a row"
        );
        let short = row.position_type == "short";
        cash_flows += if short {
            row.entry_premium * row.contracts
        } else {
            -(row.entry_premium * row.contracts)
        };
        match row.status.as_str() {
            "open" => {}
            "closed" | "rolled" => {
                let close_premium = row
                    .close_premium
                    .expect("a closed/rolled row must carry close_premium");
                cash_flows += if short {
                    -(close_premium * row.contracts)
                } else {
                    close_premium * row.contracts
                };
                let realized = row
                    .realized_pnl
                    .expect("a closed/rolled row must carry realized_pnl");
                let expected = if short {
                    (row.entry_premium - close_premium) * row.contracts
                } else {
                    (close_premium - row.entry_premium) * row.contracts
                };
                assert_close(realized, expected, "realized_pnl vs. entry/close premiums");
            }
            other => panic!("invariant violated: impossible position status {other:?}"),
        }
    }
    assert_close(
        snapshot.balance,
        INITIAL_BALANCE + cash_flows,
        "ledger conservation",
    );
}

fn parse_row(value: &Value) -> PositionRow {
    PositionRow {
        wallet_address: value["wallet_address"]
            .as_str()
            .expect("wallet_address")
            .to_string(),
        position_type: value["position_type"]
            .as_str()
            .expect("position_type")
            .to_string(),
        contracts: value["contracts"].as_f64().expect("contracts"),
        entry_premium: value["entry_premium"].as_f64().expect("entry_premium"),
        close_premium: value["close_premium"].as_f64(),
        realized_pnl: value["realized_pnl"].as_f64(),
        status: value["status"].as_str().expect("status").to_string(),
        collateral: value["collateral"].as_f64().expect("collateral"),
    }
}

// ─── Harness ──────────────────────────────────────────────────────────────────

struct Harness {
    app: TestApp,
    addresses: Vec<String>,
    tokens: Vec<String>,
    wallets: Vec<WalletModel>,
    prices: HashMap<String, (f64, f64)>,
}

impl Harness {
    async fn step(&mut self, op: Op) {
        match op {
            Op::Open {
                wallet,
                underlying,
                moneyness,
                expiry,
                call,
                short,
                contracts,
            } => {
                self.do_open(
                    wallet, underlying, moneyness, expiry, call, short, contracts,
                )
                .await;
            }
            Op::Close { wallet, pick } => {
                let open = self.wallets[wallet].open_indices();
                if let Some(index) = pick_from(&open, pick) {
                    self.do_close(wallet, index).await;
                }
            }
            Op::Roll {
                wallet,
                pick,
                moneyness,
                expiry,
            } => {
                let open = self.wallets[wallet].open_indices();
                if let Some(index) = pick_from(&open, pick) {
                    self.do_roll(wallet, index, moneyness, expiry).await;
                }
            }
            Op::Strategy {
                wallet,
                underlying,
                expiry,
                contracts,
                short_put,
            } => {
                self.do_strategy(wallet, underlying, expiry, contracts, short_put)
                    .await;
            }
            Op::CloseStrategy { wallet, pick } => {
                self.do_close_strategy(wallet, pick).await;
            }
            Op::PriceMove { underlying, pct } => {
                let symbol = UNDERLYINGS[underlying];
                let (spot, vol) = self.prices[symbol];
                let moved = (spot * (1.0 + f64::from(pct) / 100.0)).max(0.0001);
                self.prices.insert(symbol.to_string(), (moved, vol));
                self.app.set_spot(symbol, moved);
            }
        }
        self.check_all().await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn do_open(
        &mut self,
        wallet: usize,
        underlying: usize,
        moneyness: i32,
        expiry: i32,
        call: bool,
        short: bool,
        contracts: f64,
    ) {
        let symbol = UNDERLYINGS[underlying];
        let (spot, _) = self.prices[symbol];
        let strike = round4(spot * (1.0 + f64::from(moneyness) / 100.0));
        if strike <= 0.0 {
            return;
        }
        let expiry_days = f64::from(expiry);
        let entry_premium = premium(&self.prices, symbol, strike, expiry_days, call);
        let option_type = if call { "call" } else { "put" };
        let collateral = if short {
            collateral_required(option_type, contracts, strike, spot)
        } else {
            0.0
        };
        let cash_delta = if short {
            entry_premium * contracts
        } else {
            -(entry_premium * contracts)
        };

        let predicted_ok = {
            let wallet_model = &self.wallets[wallet];
            wallet_model.balance + cash_delta - (wallet_model.collateral + collateral) >= 0.0
        };

        let body = json!({
            "underlying": symbol,
            "strike": strike,
            "expiry_days": expiry_days,
            "option_type": option_type,
            "position_type": if short { "short" } else { "long" },
            "contracts": contracts,
        });
        let (status, response) = self
            .app
            .post_with(
                "/api/v1/positions/open",
                body,
                Some(self.tokens[wallet].as_str()),
            )
            .await;

        if predicted_ok {
            assert_eq!(
                status,
                StatusCode::OK,
                "reference model expected open to succeed, server said {status}: {response}"
            );
            let id = response["id"]
                .as_str()
                .expect("open response missing id")
                .to_string();
            let wallet_model = &mut self.wallets[wallet];
            wallet_model.balance += cash_delta;
            wallet_model.collateral += collateral;
            wallet_model.legs.push(Leg {
                id,
                underlying: symbol.to_string(),
                strike,
                expiry_days,
                call,
                short,
                contracts,
                collateral,
                strategy_id: None,
                closed: false,
            });
        } else {
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "reference model expected open to be rejected, server said {status}: {response}"
            );
        }
    }

    async fn do_close(&mut self, wallet: usize, leg_index: usize) {
        let leg = self.wallets[wallet].legs[leg_index].clone();
        let close_premium = premium(
            &self.prices,
            &leg.underlying,
            leg.strike,
            leg.expiry_days,
            leg.call,
        );
        let cash_delta = if leg.short {
            -(close_premium * leg.contracts)
        } else {
            close_premium * leg.contracts
        };

        let (status, response) = self
            .app
            .post_with(
                &format!("/api/v1/positions/{}/close", leg.id),
                json!({}),
                Some(self.tokens[wallet].as_str()),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "reference model expected close to succeed, server said {status}: {response}"
        );

        let wallet_model = &mut self.wallets[wallet];
        wallet_model.balance += cash_delta;
        wallet_model.collateral -= leg.collateral;
        wallet_model.legs[leg_index].closed = true;
    }

    async fn do_roll(&mut self, wallet: usize, leg_index: usize, moneyness: i32, expiry: i32) {
        let leg = self.wallets[wallet].legs[leg_index].clone();
        let (spot, _) = self.prices[&leg.underlying];
        let new_strike = round4(spot * (1.0 + f64::from(moneyness) / 100.0));
        if new_strike <= 0.0 {
            return;
        }
        let new_expiry_days = f64::from(expiry);

        let close_premium = premium(
            &self.prices,
            &leg.underlying,
            leg.strike,
            leg.expiry_days,
            leg.call,
        );
        let close_cash = if leg.short {
            -(close_premium * leg.contracts)
        } else {
            close_premium * leg.contracts
        };
        let entry_premium = premium(
            &self.prices,
            &leg.underlying,
            new_strike,
            new_expiry_days,
            leg.call,
        );
        let option_type = if leg.call { "call" } else { "put" };
        let new_collateral = if leg.short {
            collateral_required(option_type, leg.contracts, new_strike, spot)
        } else {
            0.0
        };
        let open_cash = if leg.short {
            entry_premium * leg.contracts
        } else {
            -(entry_premium * leg.contracts)
        };

        let predicted_ok = {
            let wallet_model = &self.wallets[wallet];
            let balance_after_close = wallet_model.balance + close_cash;
            let collateral_after_close = wallet_model.collateral - leg.collateral;
            balance_after_close + open_cash - (collateral_after_close + new_collateral) >= 0.0
        };

        let (status, response) = self
            .app
            .post_with(
                &format!("/api/v1/positions/{}/roll", leg.id),
                json!({ "new_strike": new_strike, "new_expiry_days": new_expiry_days }),
                Some(self.tokens[wallet].as_str()),
            )
            .await;

        if predicted_ok {
            assert_eq!(
                status,
                StatusCode::OK,
                "reference model expected roll to succeed, server said {status}: {response}"
            );
            let new_id = response["opened"]["id"]
                .as_str()
                .expect("roll response missing opened.id")
                .to_string();
            let wallet_model = &mut self.wallets[wallet];
            wallet_model.balance += close_cash + open_cash;
            wallet_model.collateral += new_collateral - leg.collateral;
            wallet_model.legs[leg_index].closed = true;
            wallet_model.legs.push(Leg {
                id: new_id,
                underlying: leg.underlying.clone(),
                strike: new_strike,
                expiry_days: new_expiry_days,
                call: leg.call,
                short: leg.short,
                contracts: leg.contracts,
                collateral: new_collateral,
                strategy_id: leg.strategy_id.clone(),
                closed: false,
            });
        } else {
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "reference model expected roll to be rejected, server said {status}: {response}"
            );
        }
    }

    async fn do_strategy(
        &mut self,
        wallet: usize,
        underlying: usize,
        expiry: i32,
        contracts: f64,
        short_put: bool,
    ) {
        let symbol = UNDERLYINGS[underlying];
        let (spot, _) = self.prices[symbol];
        let strike = round4(spot);
        let expiry_days = f64::from(expiry);
        let call_premium = premium(&self.prices, symbol, strike, expiry_days, true);
        let put_premium = premium(&self.prices, symbol, strike, expiry_days, false);

        let put_collateral = if short_put {
            collateral_required("put", contracts, strike, spot)
        } else {
            0.0
        };
        let call_cash = -(call_premium * contracts);
        let put_cash = if short_put {
            put_premium * contracts
        } else {
            -(put_premium * contracts)
        };

        let predicted_ok = {
            let wallet_model = &self.wallets[wallet];
            let after_call_balance = wallet_model.balance + call_cash;
            let after_call_ok = after_call_balance - wallet_model.collateral >= 0.0;
            let final_balance = after_call_balance + put_cash;
            let final_collateral = wallet_model.collateral + put_collateral;
            after_call_ok && final_balance - final_collateral >= 0.0
        };

        let body = json!({
            "legs": [
                {
                    "underlying": symbol, "strike": strike, "expiry_days": expiry_days,
                    "option_type": "call", "position_type": "long", "contracts": contracts,
                },
                {
                    "underlying": symbol, "strike": strike, "expiry_days": expiry_days,
                    "option_type": "put",
                    "position_type": if short_put { "short" } else { "long" },
                    "contracts": contracts,
                },
            ]
        });
        let (status, response) = self
            .app
            .post_with(
                "/api/v1/strategies/execute",
                body,
                Some(self.tokens[wallet].as_str()),
            )
            .await;

        if predicted_ok {
            assert_eq!(
                status,
                StatusCode::OK,
                "reference model expected strategy to succeed, server said {status}: {response}"
            );
            let legs = response
                .as_array()
                .expect("strategy response must be an array");
            let strategy_id = legs[0]["strategy_id"]
                .as_str()
                .expect("strategy_id")
                .to_string();
            let call_id = legs[0]["id"].as_str().expect("call leg id").to_string();
            let put_id = legs[1]["id"].as_str().expect("put leg id").to_string();

            let wallet_model = &mut self.wallets[wallet];
            wallet_model.balance += call_cash + put_cash;
            wallet_model.collateral += put_collateral;
            wallet_model.legs.push(Leg {
                id: call_id,
                underlying: symbol.to_string(),
                strike,
                expiry_days,
                call: true,
                short: false,
                contracts,
                collateral: 0.0,
                strategy_id: Some(strategy_id.clone()),
                closed: false,
            });
            wallet_model.legs.push(Leg {
                id: put_id,
                underlying: symbol.to_string(),
                strike,
                expiry_days,
                call: false,
                short: short_put,
                contracts,
                collateral: put_collateral,
                strategy_id: Some(strategy_id.clone()),
                closed: false,
            });
            wallet_model.strategy_ids.push(strategy_id);
        } else {
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "reference model expected strategy to be rejected, server said {status}: {response}"
            );
        }
    }

    async fn do_close_strategy(&mut self, wallet: usize, pick: usize) {
        let candidates: Vec<String> = {
            let wallet_model = &self.wallets[wallet];
            wallet_model
                .strategy_ids
                .iter()
                .filter(|strategy_id| {
                    wallet_model.legs.iter().any(|leg| {
                        !leg.closed && leg.strategy_id.as_deref() == Some(strategy_id.as_str())
                    })
                })
                .cloned()
                .collect()
        };
        if candidates.is_empty() {
            return;
        }
        let strategy_id = candidates[pick % candidates.len()].clone();

        let mut cash = 0.0;
        let mut collateral_released = 0.0;
        let mut closed_indexes = Vec::new();
        {
            let wallet_model = &self.wallets[wallet];
            for (index, leg) in wallet_model.legs.iter().enumerate() {
                if leg.closed {
                    continue;
                }
                if leg.strategy_id.as_deref() != Some(strategy_id.as_str()) {
                    continue;
                }
                let close_premium = premium(
                    &self.prices,
                    &leg.underlying,
                    leg.strike,
                    leg.expiry_days,
                    leg.call,
                );
                cash += if leg.short {
                    -(close_premium * leg.contracts)
                } else {
                    close_premium * leg.contracts
                };
                collateral_released += leg.collateral;
                closed_indexes.push(index);
            }
        }

        let (status, response) = self
            .app
            .post_with(
                &format!("/api/v1/strategies/{strategy_id}/close"),
                json!({}),
                Some(self.tokens[wallet].as_str()),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "reference model expected close-strategy to succeed, server said {status}: {response}"
        );

        let wallet_model = &mut self.wallets[wallet];
        wallet_model.balance += cash;
        wallet_model.collateral -= collateral_released;
        for index in closed_indexes {
            wallet_model.legs[index].closed = true;
        }
    }

    async fn check_wallet(&self, wallet: usize) {
        let token = &self.tokens[wallet];
        let (account_status, account) = self
            .app
            .get_with("/api/v1/account", Some(token.as_str()))
            .await;
        assert_eq!(
            account_status,
            StatusCode::OK,
            "account fetch failed: {account}"
        );
        let (positions_status, positions) = self
            .app
            .get_with("/api/v1/positions?limit=200", Some(token.as_str()))
            .await;
        assert_eq!(
            positions_status,
            StatusCode::OK,
            "positions fetch failed: {positions}"
        );

        let rows: Vec<PositionRow> = positions
            .as_array()
            .expect("positions response must be an array")
            .iter()
            .map(parse_row)
            .collect();
        let wallet_model = &self.wallets[wallet];
        let snapshot = WalletSnapshot {
            balance: account["balance"].as_f64().expect("balance"),
            collateral: account["collateral_locked"]
                .as_f64()
                .expect("collateral_locked"),
            model_balance: wallet_model.balance,
            model_collateral: wallet_model.collateral,
            model_open: wallet_model.open_indices().len(),
            rows,
        };
        check_ledger_invariants(&self.addresses[wallet], &snapshot);
    }

    async fn check_all(&self) {
        for wallet in 0..WALLETS {
            self.check_wallet(wallet).await;
        }
    }
}

async fn run_sequence(ops: &[Op]) {
    let app = TestApp::spawn().await;

    let mut addresses = Vec::with_capacity(WALLETS);
    let mut tokens = Vec::with_capacity(WALLETS);
    let mut wallets = Vec::with_capacity(WALLETS);
    for _ in 0..WALLETS {
        let (address, token) = app.login_account().await;
        addresses.push(address);
        tokens.push(token);
        wallets.push(WalletModel::new());
    }

    // Seed the model's view of spot/vol from the server's own state, so
    // both start from identical inputs. TestApp never spawns the price
    // simulator, so this stays in sync unless a PriceMove op changes it.
    let prices = {
        let spots = app.state.spot_prices.lock().unwrap().clone();
        let vols = app.state.vol_surface.lock().unwrap().clone();
        spots
            .into_iter()
            .map(|(symbol, spot)| {
                let vol = vols[&symbol];
                (symbol, (spot, vol))
            })
            .collect::<HashMap<_, _>>()
    };

    let mut harness = Harness {
        app,
        addresses,
        tokens,
        wallets,
        prices,
    };
    for op in ops {
        harness.step(*op).await;
    }
    harness.check_all().await;
}

// ─── The property ─────────────────────────────────────────────────────────────

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config {
        cases: invariant_cases(),
        ..proptest::test_runner::Config::default()
    })]

    /// Step random operation sequences through the reference model and the
    /// real router in lockstep, checking every ledger invariant after each
    /// step. A failure shrinks to a minimal sequence and is persisted under
    /// `proptest-regressions/`.
    #[test]
    fn ledger_and_trading_invariants_hold(
        ops in proptest::collection::vec(arb_op(), 0..MAX_OPS),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build a tokio runtime for the invariant harness");
        runtime.block_on(run_sequence(&ops));
    }
}

// ─── Injected-bug demonstration ───────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn row(
    wallet: &str,
    position_type: &str,
    contracts: f64,
    entry_premium: f64,
    status: &str,
    close_premium: Option<f64>,
    realized_pnl: Option<f64>,
    collateral: f64,
) -> PositionRow {
    PositionRow {
        wallet_address: wallet.to_string(),
        position_type: position_type.to_string(),
        contracts,
        entry_premium,
        close_premium,
        realized_pnl,
        status: status.to_string(),
        collateral,
    }
}

#[test]
fn checker_accepts_a_consistent_ledger() {
    let snapshot = WalletSnapshot {
        balance: 99_995.0,
        collateral: 0.0,
        model_balance: 99_995.0,
        model_collateral: 0.0,
        model_open: 1,
        rows: vec![row("GW", "long", 1.0, 5.0, "open", None, None, 0.0)],
    };
    check_ledger_invariants("GW", &snapshot);
}

#[test]
fn checker_catches_a_skipped_collateral_release_on_roll() {
    // A roll closed a short put (1100 collateral) and opened its
    // replacement, but the handler never released the closed leg's
    // collateral: the account still shows 2200 locked while only the
    // replacement leg (1100) is open. This is the concrete bug the
    // harness must catch.
    let snapshot = WalletSnapshot {
        balance: 100_050.0,
        collateral: 2_200.0,
        model_balance: 100_050.0,
        model_collateral: 1_100.0,
        model_open: 1,
        rows: vec![row("GW", "short", 1.0, 50.0, "open", None, None, 1_100.0)],
    };
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        check_ledger_invariants("GW", &snapshot)
    }));
    assert!(
        result.is_err(),
        "the checker must reject a ledger whose locked collateral exceeds the open legs' collateral"
    );
}

#[test]
fn checker_catches_a_dropped_balance_update() {
    // A long that paid 5 to enter and closed for 7 should leave equity at
    // initial + 2; here the account still shows the initial balance, i.e.
    // the closing cash flow never landed.
    let snapshot = WalletSnapshot {
        balance: 100_000.0,
        collateral: 0.0,
        model_balance: 100_000.0,
        model_collateral: 0.0,
        model_open: 0,
        rows: vec![row(
            "GW",
            "long",
            1.0,
            5.0,
            "closed",
            Some(7.0),
            Some(2.0),
            0.0,
        )],
    };
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        check_ledger_invariants("GW", &snapshot)
    }));
    assert!(
        result.is_err(),
        "the checker must reject a ledger that lost a balance update"
    );
}
