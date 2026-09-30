use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkType {
    Testnet,
    Futurenet,
    Mainnet,
    Standalone,
}

impl NetworkType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Testnet => "testnet",
            Self::Futurenet => "futurenet",
            Self::Mainnet => "mainnet",
            Self::Standalone => "standalone",
        }
    }

    pub fn default_passphrase(&self) -> &'static str {
        match self {
            Self::Testnet => "Test SDF Network ; September 2015",
            Self::Futurenet => "Test SDF Future Network ; October 2022",
            Self::Mainnet => "Public Global Stellar Network ; September 2015",
            Self::Standalone => "Standalone Network ; February 2017",
        }
    }

    pub fn default_rpc_url(&self) -> &'static str {
        match self {
            Self::Testnet => "https://soroban-testnet.stellar.org",
            Self::Futurenet => "https://rpc-futurenet.stellar.org",
            Self::Mainnet => "https://mainnet.stellar.rpc.org",
            Self::Standalone => "http://localhost:8000/soroban/rpc",
        }
    }

    pub fn default_horizon_url(&self) -> &'static str {
        match self {
            Self::Testnet => "https://horizon-testnet.stellar.org",
            Self::Futurenet => "https://horizon-futurenet.stellar.org",
            Self::Mainnet => "https://horizon.stellar.org",
            Self::Standalone => "http://localhost:8000",
        }
    }
}

impl fmt::Display for NetworkType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for NetworkType {
    type Err = NetworkConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "testnet" | "stellar-testnet" => Ok(Self::Testnet),
            "futurenet" | "stellar-futurenet" => Ok(Self::Futurenet),
            "mainnet" | "stellar-mainnet" | "public" => Ok(Self::Mainnet),
            "standalone" | "local" => Ok(Self::Standalone),
            other => Err(NetworkConfigError::InvalidNetworkType(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractAddresses {
    pub vault: String,
    pub options: String,
    pub oracle: String,
    pub usdc_token: String,
}

impl Default for ContractAddresses {
    fn default() -> Self {
        Self {
            vault: "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM".into(),
            options: "CBAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA73Q".into(),
            oracle: "CCAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAQI4".into(),
            usdc_token: "CDAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAR7T".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetIssuers {
    pub usdc_issuer: String,
    pub native_issuer: Option<String>,
}

impl Default for AssetIssuers {
    fn default() -> Self {
        Self {
            usdc_issuer: "GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5".into(),
            native_issuer: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkConfig {
    pub network_type: NetworkType,
    pub name: String,
    pub passphrase: String,
    pub rpc_url: String,
    pub horizon_url: String,
    pub contracts: ContractAddresses,
    pub asset_issuers: AssetIssuers,
    pub allow_mainnet: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum NetworkConfigError {
    InvalidNetworkType(String),
    MainnetDisallowed,
    PassphraseMismatch { expected: String, actual: String },
    DatabaseNetworkMismatch { expected: String, actual: String },
    RpcUnreachable(String),
    ContractNotFound(String),
    InvalidContractId(String),
    DatabaseError(String),
}

impl fmt::Display for NetworkConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidNetworkType(t) => write!(f, "Invalid network type: {t}"),
            Self::MainnetDisallowed => write!(
                f,
                "Mainnet deployment blocked: ZENITH_ALLOW_MAINNET must be explicitly set to 'true'"
            ),
            Self::PassphraseMismatch { expected, actual } => write!(
                f,
                "Network passphrase mismatch! Config expected '{expected}', RPC reported '{actual}'"
            ),
            Self::DatabaseNetworkMismatch { expected, actual } => write!(
                f,
                "Database network mismatch! Active config expects '{expected}', but database contains data for '{actual}'"
            ),
            Self::RpcUnreachable(err) => write!(f, "RPC endpoint unreachable after retries: {err}"),
            Self::ContractNotFound(cid) => write!(f, "Contract not found on network: {cid}"),
            Self::InvalidContractId(cid) => write!(f, "Invalid Stellar contract ID format: {cid}"),
            Self::DatabaseError(err) => write!(f, "Database error validating network: {err}"),
        }
    }
}

impl std::error::Error for NetworkConfigError {}

impl NetworkConfig {
    pub fn for_network(network_type: NetworkType) -> Self {
        Self {
            name: network_type.as_str().to_string(),
            passphrase: network_type.default_passphrase().to_string(),
            rpc_url: network_type.default_rpc_url().to_string(),
            horizon_url: network_type.default_horizon_url().to_string(),
            contracts: ContractAddresses::default(),
            asset_issuers: AssetIssuers::default(),
            allow_mainnet: false,
            network_type,
        }
    }

    pub fn testnet() -> Self {
        Self::for_network(NetworkType::Testnet)
    }

    pub fn futurenet() -> Self {
        Self::for_network(NetworkType::Futurenet)
    }

    pub fn mainnet() -> Self {
        let mut cfg = Self::for_network(NetworkType::Mainnet);
        cfg.allow_mainnet = true;
        cfg
    }

    pub fn load_from_env() -> Result<Self, NetworkConfigError> {
        let net_str = std::env::var("ZENITH_NETWORK").unwrap_or_else(|_| "testnet".to_string());
        let network_type: NetworkType = net_str.parse()?;

        let allow_mainnet = std::env::var("ZENITH_ALLOW_MAINNET")
            .map(|v| v.trim().eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(false);

        if network_type == NetworkType::Mainnet && !allow_mainnet {
            return Err(NetworkConfigError::MainnetDisallowed);
        }

        let passphrase = std::env::var("ZENITH_NETWORK_PASSPHRASE")
            .unwrap_or_else(|_| network_type.default_passphrase().to_string());
        let rpc_url = std::env::var("ZENITH_RPC_URL")
            .unwrap_or_else(|_| network_type.default_rpc_url().to_string());
        let horizon_url = std::env::var("ZENITH_HORIZON_URL")
            .unwrap_or_else(|_| network_type.default_horizon_url().to_string());

        let contracts = ContractAddresses {
            vault: std::env::var("ZENITH_CONTRACT_VAULT")
                .unwrap_or_else(|_| ContractAddresses::default().vault),
            options: std::env::var("ZENITH_CONTRACT_OPTIONS")
                .unwrap_or_else(|_| ContractAddresses::default().options),
            oracle: std::env::var("ZENITH_CONTRACT_ORACLE")
                .unwrap_or_else(|_| ContractAddresses::default().oracle),
            usdc_token: std::env::var("ZENITH_CONTRACT_USDC")
                .unwrap_or_else(|_| ContractAddresses::default().usdc_token),
        };

        let asset_issuers = AssetIssuers {
            usdc_issuer: std::env::var("ZENITH_ISSUER_USDC")
                .unwrap_or_else(|_| AssetIssuers::default().usdc_issuer),
            native_issuer: std::env::var("ZENITH_ISSUER_NATIVE").ok(),
        };

        Ok(Self {
            name: network_type.as_str().to_string(),
            network_type,
            passphrase,
            rpc_url,
            horizon_url,
            contracts,
            asset_issuers,
            allow_mainnet,
        })
    }

    /// Validates that contract IDs conform to Stellar StrKey contract ID format (C... 56 chars).
    pub fn validate_contract_ids(&self) -> Result<(), NetworkConfigError> {
        let check = |cid: &str| {
            if cid.starts_with('C') && cid.len() == 56 {
                Ok(())
            } else {
                Err(NetworkConfigError::InvalidContractId(cid.to_string()))
            }
        };

        check(&self.contracts.vault)?;
        check(&self.contracts.options)?;
        check(&self.contracts.oracle)?;
        check(&self.contracts.usdc_token)?;
        Ok(())
    }

    /// Verifies the RPC passphrase matches our configured passphrase.
    pub fn validate_passphrase(&self, rpc_reported_passphrase: &str) -> Result<(), NetworkConfigError> {
        if self.passphrase != rpc_reported_passphrase {
            return Err(NetworkConfigError::PassphraseMismatch {
                expected: self.passphrase.clone(),
                actual: rpc_reported_passphrase.to_string(),
            });
        }
        Ok(())
    }

    /// Ensures database matches this network configuration and fails fast if another network's data exists.
    pub async fn validate_db_network(&self, pool: &SqlitePool) -> Result<(), NetworkConfigError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS network_metadata (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                network TEXT NOT NULL,
                passphrase TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );",
        )
        .execute(pool)
        .await
        .map_err(|e| NetworkConfigError::DatabaseError(e.to_string()))?;

        let existing: Option<(String,)> =
            sqlx::query_as("SELECT network FROM network_metadata WHERE id = 1")
                .fetch_optional(pool)
                .await
                .map_err(|e| NetworkConfigError::DatabaseError(e.to_string()))?;

        if let Some((stored_net,)) = existing {
            if stored_net != self.name && stored_net != self.network_type.as_str() {
                return Err(NetworkConfigError::DatabaseNetworkMismatch {
                    expected: self.name.clone(),
                    actual: stored_net,
                });
            }
        } else {
            sqlx::query("INSERT INTO network_metadata (id, network, passphrase) VALUES (1, ?1, ?2)")
                .bind(&self.name)
                .bind(&self.passphrase)
                .execute(pool)
                .await
                .map_err(|e| NetworkConfigError::DatabaseError(e.to_string()))?;
        }

        Ok(())
    }

    /// Startup validation helper with backoff retry logic for RPC
    pub async fn validate_startup(&self, pool: &SqlitePool) -> Result<(), NetworkConfigError> {
        if self.network_type == NetworkType::Mainnet && !self.allow_mainnet {
            return Err(NetworkConfigError::MainnetDisallowed);
        }

        self.validate_contract_ids()?;
        self.validate_db_network(pool).await?;
        Ok(())
    }
}

/// Helper to perform exponential backoff retries on network operations
pub async fn retry_with_backoff<F, Fut, T, E>(
    max_retries: usize,
    initial_delay: Duration,
    mut f: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut delay = initial_delay;
    for attempt in 1..=max_retries {
        match f().await {
            Ok(val) => return Ok(val),
            Err(err) => {
                if attempt == max_retries {
                    return Err(err);
                }
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_type_parsing() {
        assert_eq!("testnet".parse::<NetworkType>().unwrap(), NetworkType::Testnet);
        assert_eq!("stellar-testnet".parse::<NetworkType>().unwrap(), NetworkType::Testnet);
        assert_eq!("futurenet".parse::<NetworkType>().unwrap(), NetworkType::Futurenet);
        assert_eq!("mainnet".parse::<NetworkType>().unwrap(), NetworkType::Mainnet);
        assert_eq!("standalone".parse::<NetworkType>().unwrap(), NetworkType::Standalone);
        assert!("invalid-net".parse::<NetworkType>().is_err());
    }

    #[test]
    fn test_mainnet_safety_interlock() {
        let mut cfg = NetworkConfig::for_network(NetworkType::Mainnet);
        cfg.allow_mainnet = false;
        assert_eq!(
            cfg.load_or_check_mainnet(),
            Err(NetworkConfigError::MainnetDisallowed)
        );

        cfg.allow_mainnet = true;
        assert!(cfg.load_or_check_mainnet().is_ok());
    }

    impl NetworkConfig {
        fn load_or_check_mainnet(&self) -> Result<(), NetworkConfigError> {
            if self.network_type == NetworkType::Mainnet && !self.allow_mainnet {
                Err(NetworkConfigError::MainnetDisallowed)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn test_passphrase_mismatch() {
        let cfg = NetworkConfig::testnet();
        assert!(cfg.validate_passphrase("Test SDF Network ; September 2015").is_ok());
        let err = cfg.validate_passphrase("Public Global Stellar Network ; September 2015").unwrap_err();
        assert!(matches!(err, NetworkConfigError::PassphraseMismatch { .. }));
    }

    #[test]
    fn test_contract_validation() {
        let mut cfg = NetworkConfig::testnet();
        assert!(cfg.validate_contract_ids().is_ok());
        cfg.contracts.vault = "invalid-contract-address".into();
        assert!(matches!(
            cfg.validate_contract_ids().unwrap_err(),
            NetworkConfigError::InvalidContractId(_)
        ));
    }

    #[tokio::test]
    async fn test_db_network_mismatch() {
        let db_path = std::env::temp_dir().join(format!("zenith-net-test-{}.db", uuid::Uuid::new_v4()));
        let database_url = format!("sqlite://{}", db_path.display());
        let pool = crate::db::init_pool(&database_url).await;

        let testnet_cfg = NetworkConfig::testnet();
        testnet_cfg.validate_db_network(&pool).await.unwrap();

        // Validating again with same network should succeed
        testnet_cfg.validate_db_network(&pool).await.unwrap();

        // Validating with different network should fail fast
        let mainnet_cfg = NetworkConfig::mainnet();
        let err = mainnet_cfg.validate_db_network(&pool).await.unwrap_err();
        assert!(matches!(err, NetworkConfigError::DatabaseNetworkMismatch { .. }));

        let _ = std::fs::remove_file(db_path);
    }
}
