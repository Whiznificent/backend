use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SorobanAuthorizationEntry {
    pub credentials_type: String, // "address"
    pub address: String,          // C... or G... address
    pub nonce: i64,
    pub signature_expiration_ledger: u32,
    pub signature_args: Vec<String>, // passkey webauthn signature or ed25519 signature
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulateAuthResult {
    pub success: bool,
    pub auth_executed: bool,
    pub error: Option<String>,
}

pub struct TransactionSimulator;

impl TransactionSimulator {
    pub async fn simulate_check_auth(
        contract_id: &str,
        auth_entry: &SorobanAuthorizationEntry,
        current_ledger: u32,
    ) -> SimulateAuthResult {
        // 1. Verify signature has not expired
        if auth_entry.signature_expiration_ledger <= current_ledger {
            return SimulateAuthResult {
                success: false,
                auth_executed: false,
                error: Some(format!(
                    "Signature expired at ledger {} (current: {})",
                    auth_entry.signature_expiration_ledger, current_ledger
                )),
            };
        }

        // 2. Verify target contract
        if auth_entry.address != contract_id {
            return SimulateAuthResult {
                success: false,
                auth_executed: false,
                error: Some("Authorization entry address does not match contract ID".into()),
            };
        }

        // 3. Check signature payload exists
        if auth_entry.signature_args.is_empty() {
            return SimulateAuthResult {
                success: false,
                auth_executed: false,
                error: Some("No signature arguments provided for __check_auth".into()),
            };
        }

        // Simulated successful host __check_auth execution
        SimulateAuthResult {
            success: true,
            auth_executed: true,
            error: None,
        }
    }
}
