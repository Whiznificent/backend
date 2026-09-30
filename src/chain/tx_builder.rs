use super::horizon::HorizonClient;
use super::readiness::{check_wallet_readiness, ReadinessReport};

#[derive(Debug)]
pub enum TxBuilderError {
    ReadinessFailed(ReadinessReport),
    InvalidParameters(String),
}

impl std::fmt::Display for TxBuilderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadinessFailed(_) => write!(f, "Account readiness check failed before building transaction"),
            Self::InvalidParameters(msg) => write!(f, "Invalid transaction parameters: {msg}"),
        }
    }
}

impl std::error::Error for TxBuilderError {}

pub struct TxBuilder;

impl TxBuilder {
    pub async fn build_deposit_collateral(
        horizon: &HorizonClient,
        wallet: &str,
        usdc_issuer: &str,
        amount_usdc: f64,
    ) -> Result<String, TxBuilderError> {
        let report = check_wallet_readiness(horizon, wallet, usdc_issuer, amount_usdc, 0.1).await;
        if !report.ready {
            return Err(TxBuilderError::ReadinessFailed(report));
        }

        // Return simulated built transaction envelope XDR
        Ok(format!("AAAA_BUILT_TX_FOR_{wallet}_{amount_usdc}"))
    }
}
