//! Limit orders with a resting order book and mark-based matching engine.
//!
//! Orders rest until the platform's quote for the series crosses the limit
//! price, then fill automatically through the existing open/close transaction
//! helpers. The counterparty is always the platform quote (no peer-to-peer
//! matching).

use serde::{Deserialize, Serialize};

/// Time-in-force for a resting order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeInForce {
    /// Good-till-cancel: rests until filled or explicitly cancelled.
    Gtc,
    /// Good-till-date: rests until `expires_at`, then expires.
    Gtd,
    /// Immediate-or-cancel: fills what it can now, remainder is rejected.
    Ioc,
}

/// Lifecycle status of an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderStatus {
    Open,
    Filled,
    Cancelled,
    Expired,
    Rejected,
}

/// Whether the order opens a new position or closes an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderIntent {
    Open,
    Close,
}

/// Buy or sell side of the order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderSide {
    Buy,
    Sell,
}

/// A resting limit order as persisted in the `orders` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: i64,
    pub wallet: String,
    /// Series identifier the order is written against.
    pub series: String,
    /// Optional leg spec for multi-leg orders.
    pub leg_spec: Option<String>,
    pub side: OrderSide,
    pub intent: OrderIntent,
    /// Limit price in quote units.
    pub limit_price: f64,
    pub contracts: i64,
    pub tif: TimeInForce,
    pub status: OrderStatus,
    /// Set for `gtd` orders; `None` for `gtc`/`ioc`.
    pub expires_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Position opened/closed by the fill, if any.
    pub filled_position_id: Option<i64>,
}

/// Current platform quote for a series.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Quote {
    pub bid: f64,
    pub ask: f64,
}

/// A single fill produced by the matching engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub order_id: i64,
    pub wallet: String,
    pub series: String,
    pub side: OrderSide,
    pub intent: OrderIntent,
    pub contracts: i64,
    /// Execution price: the better of the limit and the crossing quote.
    pub price: f64,
}

/// Pure matching function over `(orders, quotes) -> fills`.
///
/// An order fills when the platform quote crosses its limit price:
/// * a buy fills when `ask <= limit_price`,
/// * a sell fills when `bid >= limit_price`.
///
/// When the quote gaps through the limit the order fills at the *better*
/// price (the quote), never the worse one. Orders whose series has no quote
/// are skipped (delisted series are auto-cancelled elsewhere).
pub fn match_orders(orders: &[Order], quotes: &std::collections::HashMap<String, Quote>) -> Vec<Fill> {
    let mut fills = Vec::new();
    for order in orders {
        if order.status != OrderStatus::Open {
            continue;
        }
        let Some(quote) = quotes.get(&order.series) else {
            continue;
        };
        let price = match order.side {
            OrderSide::Buy => {
                if quote.ask <= order.limit_price {
                    // Better price is the lower one for a buy.
                    quote.ask.min(order.limit_price)
                } else {
                    continue;
                }
            }
            OrderSide::Sell => {
                if quote.bid >= order.limit_price {
                    // Better price is the higher one for a sell.
                    quote.bid.max(order.limit_price)
                } else {
                    continue;
                }
            }
        };
        fills.push(Fill {
            order_id: order.id,
            wallet: order.wallet.clone(),
            series: order.series.clone(),
            side: order.side,
            intent: order.intent,
            contracts: order.contracts,
            price,
        });
    }
    fills
}

/// SQL for the `orders` table and its matching index.
///
/// Kept alongside the module so the migration and the matching loop stay in
/// sync on the `(series, side, limit_price)` index used to scan resting orders.
pub const ORDERS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS orders (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    wallet             TEXT    NOT NULL,
    series             TEXT    NOT NULL,
    leg_spec           TEXT,
    side               TEXT    NOT NULL CHECK (side IN ('buy', 'sell')),
    intent             TEXT    NOT NULL CHECK (intent IN ('open', 'close')),
    limit_price        REAL    NOT NULL,
    contracts          INTEGER NOT NULL,
    tif                TEXT    NOT NULL CHECK (tif IN ('gtc', 'gtd', 'ioc')),
    status             TEXT    NOT NULL CHECK (status IN ('open', 'filled', 'cancelled', 'expired', 'rejected')),
    expires_at         INTEGER,
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL,
    filled_position_id INTEGER
);

CREATE INDEX IF NOT EXISTS idx_orders_series_side_limit
    ON orders (series, side, limit_price);

CREATE INDEX IF NOT EXISTS idx_orders_wallet_status
    ON orders (wallet, status);
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn order(id: i64, side: OrderSide, limit_price: f64) -> Order {
        Order {
            id,
            wallet: "w1".into(),
            series: "S1".into(),
            leg_spec: None,
            side,
            intent: OrderIntent::Open,
            limit_price,
            contracts: 1,
            tif: TimeInForce::Gtc,
            status: OrderStatus::Open,
            expires_at: None,
            created_at: 0,
            updated_at: 0,
            filled_position_id: None,
        }
    }

    fn quotes(bid: f64, ask: f64) -> HashMap<String, Quote> {
        let mut m = HashMap::new();
        m.insert("S1".to_string(), Quote { bid, ask });
        m
    }

    #[test]
    fn buy_fills_when_ask_crosses_limit() {
        let fills = match_orders(&[order(1, OrderSide::Buy, 10.0)], &quotes(9.0, 9.5));
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].price, 9.5);
    }

    #[test]
    fn buy_does_not_fill_when_ask_above_limit() {
        let fills = match_orders(&[order(1, OrderSide::Buy, 10.0)], &quotes(10.5, 11.0));
        assert!(fills.is_empty());
    }

    #[test]
    fn sell_fills_when_bid_crosses_limit() {
        let fills = match_orders(&[order(1, OrderSide::Sell, 10.0)], &quotes(10.5, 11.0));
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].price, 10.5);
    }

    #[test]
    fn gap_through_fills_at_better_price() {
        // Buy limit 10, ask gaps down to 8 -> fill at 8, never worse than 10.
        let fills = match_orders(&[order(1, OrderSide::Buy, 10.0)], &quotes(7.5, 8.0));
        assert_eq!(fills[0].price, 8.0);
    }

    #[test]
    fn non_open_orders_are_skipped() {
        let mut o = order(1, OrderSide::Buy, 10.0);
        o.status = OrderStatus::Cancelled;
        assert!(match_orders(&[o], &quotes(9.0, 9.5)).is_empty());
    }

    #[test]
    fn missing_quote_is_skipped() {
        let fills = match_orders(&[order(1, OrderSide::Buy, 10.0)], &HashMap::new());
        assert!(fills.is_empty());
    }
}
