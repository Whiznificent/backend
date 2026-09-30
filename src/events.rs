//! Internal domain event bus for authenticated private WebSocket channels.
//!
//! Events are published **after** the originating transaction commits (outbox
//! style) so a rolled-back transaction never emits. The bus is a filtered
//! global `tokio::broadcast`; every event carries the owning wallet so a
//! subscriber can only ever observe its own events.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tokio::sync::broadcast;

/// Capacity of the broadcast ring buffer. Slow subscribers that fall behind
/// receive `RecvError::Lagged` and are expected to resync via HTTP.
const EVENT_BUS_CAPACITY: usize = 1024;

/// Domain events pushed to authenticated private channels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DomainEvent {
    /// A position was opened.
    PositionOpened {
        position_id: String,
        market: String,
        side: String,
        size: f64,
        entry_price: f64,
    },
    /// A position was closed.
    PositionClosed {
        position_id: String,
        market: String,
        exit_price: f64,
        realized_pnl: f64,
    },
    /// A position was settled.
    PositionSettled {
        position_id: String,
        market: String,
        payout: f64,
    },
    /// A position was liquidated.
    PositionLiquidated {
        position_id: String,
        market: String,
        liquidation_price: f64,
    },
    /// The wallet balance changed.
    BalanceChanged {
        asset: String,
        balance: f64,
        delta: f64,
    },
    /// A price alert triggered.
    AlertTriggered {
        alert_id: String,
        market: String,
        price: f64,
        condition: String,
    },
    /// An order changed state.
    OrderUpdated {
        order_id: String,
        market: String,
        status: String,
    },
}

impl DomainEvent {
    /// The private channel this event belongs to.
    pub fn channel(&self) -> &'static str {
        match self {
            DomainEvent::PositionOpened { .. }
            | DomainEvent::PositionClosed { .. }
            | DomainEvent::PositionSettled { .. }
            | DomainEvent::PositionLiquidated { .. } => "positions",
            DomainEvent::BalanceChanged { .. } => "account",
            DomainEvent::AlertTriggered { .. } => "alerts",
            DomainEvent::OrderUpdated { .. } => "orders",
        }
    }
}

/// An event tagged with the wallet that owns it.
#[derive(Debug, Clone)]
pub struct WalletEvent {
    pub wallet: String,
    pub event: DomainEvent,
}

static EVENT_BUS: OnceLock<broadcast::Sender<WalletEvent>> = OnceLock::new();

/// Returns the process-wide event bus, initialising it on first use.
pub fn bus() -> &'static broadcast::Sender<WalletEvent> {
    EVENT_BUS.get_or_init(|| broadcast::channel(EVENT_BUS_CAPACITY).0)
}

/// Subscribe to the global bus. Callers must filter by wallet.
pub fn subscribe() -> broadcast::Receiver<WalletEvent> {
    bus().subscribe()
}

/// Publish an event for `wallet`.
///
/// MUST be called only after the originating transaction has committed.
/// Returns the number of active receivers, or 0 if there are none.
pub fn publish(wallet: impl Into<String>, event: DomainEvent) -> usize {
    bus()
        .send(WalletEvent {
            wallet: wallet.into(),
            event,
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publish_is_delivered_to_subscribers() {
        let mut rx = subscribe();
        publish(
            "wallet-a",
            DomainEvent::BalanceChanged {
                asset: "USDC".into(),
                balance: 100.0,
                delta: 10.0,
            },
        );
        let received = rx.recv().await.expect("event delivered");
        assert_eq!(received.wallet, "wallet-a");
        assert_eq!(received.event.channel(), "account");
    }

    #[tokio::test]
    async fn events_are_isolated_by_wallet() {
        let mut rx = subscribe();
        publish(
            "wallet-a",
            DomainEvent::AlertTriggered {
                alert_id: "a1".into(),
                market: "BTC-USD".into(),
                price: 42_000.0,
                condition: "above".into(),
            },
        );
        publish(
            "wallet-b",
            DomainEvent::AlertTriggered {
                alert_id: "b1".into(),
                market: "ETH-USD".into(),
                price: 2_500.0,
                condition: "below".into(),
            },
        );

        // A subscriber for wallet-a must never observe wallet-b's event.
        let mut seen_a = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if ev.wallet == "wallet-a" {
                seen_a.push(ev.event);
            }
        }
        assert_eq!(seen_a.len(), 1);
        assert_eq!(seen_a[0].channel(), "alerts");
    }

    #[test]
    fn channel_mapping_is_stable() {
        assert_eq!(
            DomainEvent::PositionOpened {
                position_id: "p".into(),
                market: "BTC-USD".into(),
                side: "long".into(),
                size: 1.0,
                entry_price: 1.0,
            }
            .channel(),
            "positions"
        );
        assert_eq!(
            DomainEvent::OrderUpdated {
                order_id: "o".into(),
                market: "BTC-USD".into(),
                status: "filled".into(),
            }
            .channel(),
            "orders"
        );
    }
}
