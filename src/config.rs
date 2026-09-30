use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;

#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub bind_address: String,
    pub database_url: String,
    pub nonce_ttl_secs: i64,
    pub session_ttl_secs: i64,
    pub max_pct_move_per_tick: f64,
    pub auth_rate_limit_per_second: u64,
    pub auth_rate_limit_burst: u32,
    pub mutation_rate_limit_per_second: u64,
    pub mutation_rate_limit_burst: u32,
    pub seeded_prices: HashMap<String, f64>,
    pub seeded_vols: HashMap<String, f64>,
    pub deprecated_routes: Vec<String>,
    pub deprecation_timestamp: Option<u64>,
    pub sunset_date: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_address: "0.0.0.0:8081".into(),
            database_url: "sqlite://zenith.db".into(),
            nonce_ttl_secs: 5 * 60,
            session_ttl_secs: 24 * 60 * 60,
            max_pct_move_per_tick: 0.003,
            auth_rate_limit_per_second: 2,
            auth_rate_limit_burst: 10,
            mutation_rate_limit_per_second: 5,
            mutation_rate_limit_burst: 20,
            seeded_prices: HashMap::from([
                ("XLM".into(), 0.1182),
                ("BTC".into(), 67420.50),
                ("ETH".into(), 3512.80),
                ("SOL".into(), 182.45),
            ]),
            seeded_vols: HashMap::from([
                ("XLM".into(), 0.82),
                ("BTC".into(), 0.65),
                ("ETH".into(), 0.72),
                ("SOL".into(), 0.91),
            ]),
            deprecated_routes: Vec::new(),
            deprecation_timestamp: None,
            sunset_date: None,
        }
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind_address", &self.bind_address)
            .field("database_url", &"[REDACTED]")
            .field("nonce_ttl_secs", &self.nonce_ttl_secs)
            .field("session_ttl_secs", &self.session_ttl_secs)
            .field("max_pct_move_per_tick", &self.max_pct_move_per_tick)
            .field(
                "auth_rate_limit_per_second",
                &self.auth_rate_limit_per_second,
            )
            .field("auth_rate_limit_burst", &self.auth_rate_limit_burst)
            .field(
                "mutation_rate_limit_per_second",
                &self.mutation_rate_limit_per_second,
            )
            .field("mutation_rate_limit_burst", &self.mutation_rate_limit_burst)
            .field("seeded_prices", &self.seeded_prices)
            .field("seeded_vols", &self.seeded_vols)
            .field("deprecated_routes", &self.deprecated_routes)
            .field("deprecation_timestamp", &self.deprecation_timestamp)
            .field("sunset_date", &self.sunset_date)
            .finish()
    }
}

impl Config {
    pub fn load() -> Result<Self, String> {
        Self::load_from(std::env::args().skip(1), std::env::vars())
    }

    fn load_from(
        args: impl IntoIterator<Item = String>,
        env: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, String> {
        let args: Vec<String> = args.into_iter().collect();
        let mut config_path = None;
        let mut cli = HashMap::new();
        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            if arg == "--config" {
                config_path = Some(iter.next().ok_or("--config requires a path")?);
            } else if let Some((key, value)) =
                arg.strip_prefix("--").and_then(|s| s.split_once('='))
            {
                cli.insert(
                    key.replace('-', "_").to_ascii_uppercase(),
                    value.to_string(),
                );
            } else if let Some(key) = arg.strip_prefix("--") {
                let value = iter
                    .next()
                    .ok_or_else(|| format!("--{key} requires a value"))?;
                cli.insert(key.replace('-', "_").to_ascii_uppercase(), value);
            } else {
                return Err(format!("unexpected argument: {arg}"));
            }
        }

        let mut config = Config::default();
        if let Some(path) = config_path {
            let contents = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed to read config file {path}: {e}"))?;
            config = toml::from_str(&contents)
                .map_err(|e| format!("invalid config file {path}: {e}"))?;
        }

        let env: Vec<_> = env.into_iter().collect();
        if let Some((_, value)) = env.iter().find(|(key, _)| key == "DATABASE_URL") {
            config.database_url = value.clone();
        }
        for (key, value) in env {
            if let Some(key) = key.strip_prefix("ZENITH_") {
                config.set(key, &value)?;
            }
        }
        for (key, value) in cli {
            config.set(&key, &value)?;
        }
        config.validate()?;
        Ok(config)
    }

    fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        macro_rules! parse {
            ($field:ident, $type:ty) => {{
                self.$field = value.parse::<$type>().map_err(|_| {
                    format!("invalid value for {}: expected {}", key, stringify!($type))
                })?;
            }};
        }
        match key {
            "BIND_ADDRESS" => self.bind_address = value.into(),
            "DATABASE_URL" => self.database_url = value.into(),
            "NONCE_TTL_SECS" => parse!(nonce_ttl_secs, i64),
            "SESSION_TTL_SECS" => parse!(session_ttl_secs, i64),
            "MAX_PCT_MOVE_PER_TICK" => parse!(max_pct_move_per_tick, f64),
            "AUTH_RATE_LIMIT_PER_SECOND" => parse!(auth_rate_limit_per_second, u64),
            "AUTH_RATE_LIMIT_BURST" => parse!(auth_rate_limit_burst, u32),
            "MUTATION_RATE_LIMIT_PER_SECOND" => parse!(mutation_rate_limit_per_second, u64),
            "MUTATION_RATE_LIMIT_BURST" => parse!(mutation_rate_limit_burst, u32),
            "SEEDED_PRICES" => {
                self.seeded_prices = serde_json::from_str(value)
                    .map_err(|e| format!("invalid value for {key}: {e}"))?
            }
            "SEEDED_VOLS" => {
                self.seeded_vols = serde_json::from_str(value)
                    .map_err(|e| format!("invalid value for {key}: {e}"))?
            }
            "DEPRECATED_ROUTES" => {
                self.deprecated_routes = serde_json::from_str(value)
                    .map_err(|e| format!("invalid value for {key}: {e}"))?
            }
            "DEPRECATION_TIMESTAMP" => {
                self.deprecation_timestamp = Some(
                    value
                        .parse()
                        .map_err(|_| format!("invalid value for {key}: expected Unix timestamp"))?,
                )
            }
            "SUNSET_DATE" => self.sunset_date = Some(value.into()),
            _ => return Err(format!("unknown configuration key: {key}")),
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        self.bind_address
            .parse::<SocketAddr>()
            .map_err(|_| "bind_address must be a valid socket address".to_string())?;
        if !self.database_url.starts_with("sqlite:") {
            return Err("database_url must use the sqlite: scheme".into());
        }
        if self.nonce_ttl_secs <= 0 || self.session_ttl_secs <= 0 {
            return Err("nonce_ttl_secs and session_ttl_secs must be positive".into());
        }
        if !self.max_pct_move_per_tick.is_finite()
            || self.max_pct_move_per_tick <= 0.0
            || self.max_pct_move_per_tick >= 1.0
        {
            return Err("max_pct_move_per_tick must be finite and between 0 and 1".into());
        }
        if self.auth_rate_limit_per_second == 0
            || self.auth_rate_limit_burst == 0
            || self.mutation_rate_limit_per_second == 0
            || self.mutation_rate_limit_burst == 0
        {
            return Err("rate-limit values must be positive".into());
        }
        if self.seeded_prices.is_empty()
            || self
                .seeded_prices
                .values()
                .any(|v| !v.is_finite() || *v <= 0.0)
            || self.seeded_vols.is_empty()
            || self
                .seeded_vols
                .values()
                .any(|v| !v.is_finite() || *v <= 0.0)
        {
            return Err(
                "seeded prices and vols must be non-empty and finite positive values".into(),
            );
        }
        if !self.deprecated_routes.is_empty() {
            if self
                .deprecated_routes
                .iter()
                .any(|route| !route.starts_with("/api/v1/"))
            {
                return Err("deprecated_routes may only contain /api/v1/ paths".into());
            }
            if self.deprecation_timestamp.is_none() {
                return Err("deprecation_timestamp is required for deprecated routes".into());
            }
            let sunset = self
                .sunset_date
                .as_deref()
                .ok_or("sunset_date is required for deprecated routes")?;
            httpdate::parse_http_date(sunset)
                .map_err(|_| "sunset_date must be an RFC 1123 HTTP-date".to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_layers_apply_in_precedence_order() {
        let config = Config::load_from(
            ["--bind-address=127.0.0.1:9000".to_string()],
            [("ZENITH_BIND_ADDRESS".into(), "127.0.0.1:8082".into())],
        )
        .unwrap();
        assert_eq!(config.bind_address, "127.0.0.1:9000");
    }

    #[test]
    fn debug_output_redacts_the_database_url() {
        let config = Config {
            database_url: "sqlite://user:password@private.db".into(),
            ..Config::default()
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("password"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn rejects_invalid_values() {
        let config = Config {
            max_pct_move_per_tick: 1.0,
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }
}
