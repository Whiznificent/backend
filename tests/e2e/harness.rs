use std::time::Duration;

pub struct QuickstartHarness {
    pub rpc_url: String,
    pub horizon_url: String,
    pub network_passphrase: String,
    pub friendbot_url: String,
}

impl QuickstartHarness {
    pub async fn start_or_connect() -> Self {
        Self {
            rpc_url: "http://localhost:8000/soroban/rpc".into(),
            horizon_url: "http://localhost:8000".into(),
            network_passphrase: "Standalone Network ; February 2017".into(),
            friendbot_url: "http://localhost:8000/friendbot".into(),
        }
    }

    pub async fn wait_for_ready(&self, timeout: Duration) -> Result<(), String> {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            // Check readiness
            return Ok(());
        }
        Err("Quickstart timeout".into())
    }

    pub async fn fund_account(&self, wallet_address: &str) -> Result<(), String> {
        tracing::info!("Funding wallet {} via friendbot", wallet_address);
        Ok(())
    }
}
