//! Pure matching engine for resting limit orders.
//!
//! The matching loop evaluates open orders against the platform's current
//! bid/ask quote for a series on each tick. The counterparty is always the
//! platform quote (no peer-to-peer matching).
//!
//! The core is a pure function over `(orders, quotes) -> fills` so it can be
//! unit tested in isolation and reasoned about without any I/O.

use std::collections::HashMap;

/// Side of a resting order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Buy,
    Sell,
}

/// Whether the order opens a new position or closes an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Open,
    Close,
}

/// Time-in-force for an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeInForce {
    /// Good-till-cancel: rests until filled or explicitly cancelled.
    Gtc,
    /// Good-till-date: rests until the expiry timestamp, then expires.
    Gtd,
    /// Immediate-or-cancel: fills whatever is available now, remainder rejected.
    Ioc,
}

/// Lifecycle status of an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    Open,
    Filled,
    Cancelled,
    Expired,
    Rejected,
}

/// A resting limit order as seen by the matching engine.
#[derive(Debug, Clone)]
pub struct Order {
    pub id: i64,
    pub wallet: String,
    pub series: String,
    pub side: Side,
    pub intent: Intent,
    /// Limit price in the series' quote units.
    pub limit_price: f64,
    pub contracts: i64,
    pub tif: TimeInForce,
    pub status: OrderStatus,
    /// Unix timestamp (seconds) after which a GTD order expires.
    pub expires_at: Option<i64>,
}

/// Current platform quote for a series.
#[derive(Debug, Clone, Copy)]
pub struct Quote {
    pub bid: f64,
    pub ask: f64,
}

/// A single fill produced by the matching engine.
#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    pub order_id: i64,
    pub wallet: String,
    pub series: String,
    pub side: Side,
    pub intent: Intent,
    /// Price at which the fill executes (always the better price for the taker).
    pub price: f64,
    pub contracts: i64,
}

/// Result of evaluating the resting book against the current quotes.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MatchResult {
    /// Fills to execute, in evaluation order.
    pub fills: Vec<Fill>,
    /// Orders that should transition to `Expired` (GTD past expiry).
    pub expired: Vec<i64>,
    /// IOC orders that could not be (fully) filled and must be rejected.
    pub rejected: Vec<i64>,
}

/// Evaluate the resting order book against the current quotes.
///
/// This is a pure function: it never mutates its inputs and performs no I/O.
/// The caller is responsible for applying the returned transitions
/// transactionally (guarded with `UPDATE orders SET status='filled'
/// WHERE id=? AND status='open'`).
///
/// Matching rules:
/// * A buy order fills when the platform ask is at or below the limit price.
/// * A sell order fills when the platform bid is at or above the limit price.
/// * When the quote gaps through the limit, the fill executes at the better
///   price for the taker (the quote), never the worse one (the limit).
/// * GTD orders past `expires_at` are expired and never matched.
/// * IOC orders that cannot fill are rejected; a partial IOC fill is allowed
///   and the remainder is rejected.
pub fn match_orders(orders: &[Order], quotes: &HashMap<String, Quote>, now: i64) -> MatchResult {
    let mut result = MatchResult::default();

    for order in orders {
        if order.status != OrderStatus::Open {
            continue;
        }

        // GTD expiry takes precedence over matching.
        if order.tif == TimeInForce::Gtd {
            if let Some(expires_at) = order.expires_at {
                if now >= expires_at {
                    result.expired.push(order.id);
                    continue;
                }
            }
        }

        let quote = match quotes.get(&order.series) {
            Some(q) => q,
            None => {
                // No quote available: an IOC order cannot rest, so reject it.
                if order.tif == TimeInForce::Ioc {
                    result.rejected.push(order.id);
                }
                continue;
            }
        };

        let fill_price = match order.side {
            // Buy crosses when the ask is at or below the limit; fill at the
            // better (lower) of the two prices.
            Side::Buy => {
                if quote.ask <= order.limit_price {
                    Some(quote.ask.min(order.limit_price))
                } else {
                    None
                }
            }
            // Sell crosses when the bid is at or above the limit; fill at the
            // better (higher) of the two prices.
            Side::Sell => {
                if quote.bid >= order.limit_price {
                    Some(quote.bid.max(order.limit_price))
                } else {
                    None
                }
            }
        };

        match fill_price {
            Some(price) => {
                result.fills.push(Fill {
                    order_id: order.id,
                    wallet: order.wallet.clone(),
                    series: order.series.clone(),
                    side: order.side,
                    intent: order.intent,
                    price,
                    contracts: order.contracts,
                });
            }
            None => {
                // IOC orders that do not cross are rejected immediately.
                if order.tif == TimeInForce::Ioc {
                    result.rejected.push(order.id);
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(id: i64, side: Side, limit_price: f64, tif: TimeInForce) -> Order {
        Order {
            id,
            wallet: "w1".to_string(),
            series: "SPX-2024".to_string(),
            side,
            intent: Intent::Open,
            limit_price,
            contracts: 10,
            tif,
            status: OrderStatus::Open,
            expires_at: None,
        }
    }

    fn quotes(bid: f64, ask: f64) -> HashMap<String, Quote> {
        let mut m = HashMap::new();
        m.insert("SPX-2024".to_string(), Quote { bid, ask });
        m
    }

    #[test]
    fn buy_fills_when_ask_at_or_below_limit() {
        let orders = vec![order(1, Side::Buy, 100.0, TimeInForce::Gtc)];
        let res = match_orders(&orders, &quotes(98.0, 99.0), 0);
        assert_eq!(res.fills.len(), 1);
        assert_eq!(res.fills[0].price, 99.0);
    }

    #[test]
    fn buy_does_not_fill_when_ask_above_limit() {
        let orders = vec![order(1, Side::Buy, 100.0, TimeInForce::Gtc)];
        let res = match_orders(&orders, &quotes(101.0, 102.0), 0);
        assert!(res.fills.is_empty());
    }

    #[test]
    fn sell_fills_when_bid_at_or_above_limit() {
        let orders = vec![order(1, Side::Sell, 100.0, TimeInForce::Gtc)];
        let res = match_orders(&orders, &quotes(101.0, 102.0), 0);
        assert_eq!(res.fills.len(), 1);
        assert_eq!(res.fills[0].price, 101.0);
    }

    #[test]
    fn gap_through_limit_fills_at_better_price() {
        // Buy limit 100, ask gaps down to 95 -> fill at 95, not 100.
        let orders = vec![order(1, Side::Buy, 100.0, TimeInForce::Gtc)];
        let res = match_orders(&orders, &quotes(94.0, 95.0), 0);
        assert_eq!(res.fills[0].price, 95.0);

        // Sell limit 100, bid gaps up to 105 -> fill at 105, not 100.
        let orders = vec![order(2, Side::Sell, 100.0, TimeInForce::Gtc)];
        let res = match_orders(&orders, &quotes(105.0, 106.0), 0);
        assert_eq!(res.fills[0].price, 105.0);
    }

    #[test]
    fn gtd_expires_past_expiry() {
        let mut o = order(1, Side::Buy, 100.0, TimeInForce::Gtd);
        o.expires_at = Some(1000);
        let res = match_orders(&[o], &quotes(98.0, 99.0), 1000);
        assert_eq!(res.expired, vec![1]);
        assert!(res.fills.is_empty());
    }

    #[test]
    fn ioc_rejected_when_not_crossing() {
        let orders = vec![order(1, Side::Buy, 100.0, TimeInForce::Ioc)];
        let res = match_orders(&orders, &quotes(101.0, 102.0), 0);
        assert_eq!(res.rejected, vec![1]);
        assert!(res.fills.is_empty());
    }

    #[test]
    fn ioc_fills_when_crossing() {
        let orders = vec![order(1, Side::Buy, 100.0, TimeInForce::Ioc)];
        let res = match_orders(&orders, &quotes(98.0, 99.0), 0);
        assert_eq!(res.fills.len(), 1);
        assert!(res.rejected.is_empty());
    }

    #[test]
    fn non_open_orders_are_ignored() {
        let mut o = order(1, Side::Buy, 100.0, TimeInForce::Gtc);
        o.status = OrderStatus::Filled;
        let res = match_orders(&[o], &quotes(98.0, 99.0), 0);
        assert!(res.fills.is_empty());
    }
}
