use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum DecodedEvent {
    OptionMinted {
        series_id: String,
        strike: f64,
        expiry: u64,
        is_call: bool,
        contracts: f64,
        writer: String,
    },
    OptionExercised {
        series_id: String,
        buyer: String,
        payout: f64,
    },
    CollateralDeposited {
        wallet: String,
        amount: f64,
    },
    ContractUpgraded {
        new_wasm_hash: String,
        new_version: u32,
    },
}
