use crate::chain::bindings::events::DecodedEvent;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DecoderKey {
    pub contract_id: String,
    pub wasm_hash: String,
    pub event_topic: String,
    pub schema_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawContractEvent {
    pub contract_id: String,
    pub wasm_hash: String,
    pub ledger: u32,
    pub tx_index: u32,
    pub op_index: u32,
    pub topics: Vec<String>,
    pub data: serde_json::Value,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    UnsupportedSchema(DecoderKey),
    MissingField(String),
    TypeMismatch(String),
    MalformedData(String),
    DatabaseError(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSchema(key) => write!(
                f,
                "No registered decoder for contract {} (wasm: {}, topic: {}, v{})",
                key.contract_id, key.wasm_hash, key.event_topic, key.schema_version
            ),
            Self::MissingField(field) => write!(f, "Missing event field: {field}"),
            Self::TypeMismatch(err) => write!(f, "Event field type mismatch: {err}"),
            Self::MalformedData(err) => write!(f, "Malformed event data: {err}"),
            Self::DatabaseError(err) => write!(f, "Indexer database error: {err}"),
        }
    }
}

impl std::error::Error for DecodeError {}

pub trait EventDecoder: Send + Sync {
    fn supports(&self, key: &DecoderKey) -> bool;
    fn decode(&self, raw: &RawContractEvent) -> Result<DecodedEvent, DecodeError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackStrategy {
    ExactOnly,
    FallbackLatestSchema,
}

pub struct DecoderRegistry {
    decoders: Vec<Box<dyn EventDecoder>>,
    fallback_strategy: FallbackStrategy,
    paused_contracts: Arc<std::sync::Mutex<HashMap<String, bool>>>,
    db: SqlitePool,
}

impl DecoderRegistry {
    pub fn new(fallback_strategy: FallbackStrategy, db: SqlitePool) -> Self {
        Self {
            decoders: Vec::new(),
            fallback_strategy,
            paused_contracts: Arc::new(std::sync::Mutex::new(HashMap::new())),
            db,
        }
    }

    pub fn register<D: EventDecoder + 'static>(&mut self, decoder: D) {
        self.decoders.push(Box::new(decoder));
    }

    pub fn is_contract_paused(&self, contract_id: &str) -> bool {
        let paused = self.paused_contracts.lock().unwrap();
        *paused.get(contract_id).unwrap_or(&false)
    }

    pub async fn record_wasm_upgrade(
        &self,
        contract_id: &str,
        wasm_hash: &str,
        from_ledger: u32,
    ) -> Result<(), DecodeError> {
        sqlx::query(
            "INSERT INTO contract_wasm_history (contract_id, wasm_hash, from_ledger) VALUES (?1, ?2, ?3)",
        )
        .bind(contract_id)
        .bind(wasm_hash)
        .bind(from_ledger as i64)
        .execute(&self.db)
        .await
        .map_err(|e| DecodeError::DatabaseError(e.to_string()))?;
        Ok(())
    }

    pub async fn decode_and_dispatch(
        &self,
        raw: &RawContractEvent,
        schema_version: u32,
    ) -> Result<DecodedEvent, DecodeError> {
        let topic = raw.topics.first().cloned().unwrap_or_default();
        let key = DecoderKey {
            contract_id: raw.contract_id.clone(),
            wasm_hash: raw.wasm_hash.clone(),
            event_topic: topic,
            schema_version,
        };

        let decoder = self.decoders.iter().find(|d| d.supports(&key));

        match decoder {
            Some(d) => match d.decode(raw) {
                Ok(event) => Ok(event),
                Err(err) => {
                    self.handle_failure(raw, &err).await?;
                    Err(err)
                }
            },
            None => {
                let err = DecodeError::UnsupportedSchema(key);
                self.handle_failure(raw, &err).await?;
                Err(err)
            }
        }
    }

    async fn handle_failure(&self, raw: &RawContractEvent, error: &DecodeError) -> Result<(), DecodeError> {
        let record_id = uuid::Uuid::new_v4().to_string();
        let topics_json = serde_json::to_string(&raw.topics).unwrap_or_default();
        let data_json = raw.data.to_string();
        let err_reason = error.to_string();

        // Pause projections for this contract to prevent corruption
        {
            let mut paused = self.paused_contracts.lock().unwrap();
            paused.insert(raw.contract_id.clone(), true);
        }

        sqlx::query(
            "INSERT INTO unprocessed_events (id, contract_id, wasm_hash, ledger, topics, data, error_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(&record_id)
        .bind(&raw.contract_id)
        .bind(&raw.wasm_hash)
        .bind(raw.ledger as i64)
        .bind(&topics_json)
        .bind(&data_json)
        .bind(&err_reason)
        .execute(&self.db)
        .await
        .map_err(|e| DecodeError::DatabaseError(e.to_string()))?;

        Ok(())
    }
}

// Sample V1 & V2 Decoders for testing upgrades
pub struct OptionMintedDecoderV1;
impl EventDecoder for OptionMintedDecoderV1 {
    fn supports(&self, key: &DecoderKey) -> bool {
        key.event_topic == "mint" && key.schema_version == 1 && key.wasm_hash == "WASM_V1_HASH"
    }

    fn decode(&self, raw: &RawContractEvent) -> Result<DecodedEvent, DecodeError> {
        let series_id = raw.data["series_id"].as_str().ok_or(DecodeError::MissingField("series_id".into()))?;
        let strike = raw.data["strike"].as_f64().ok_or(DecodeError::MissingField("strike".into()))?;
        let contracts = raw.data["contracts"].as_f64().ok_or(DecodeError::MissingField("contracts".into()))?;
        let writer = raw.data["writer"].as_str().ok_or(DecodeError::MissingField("writer".into()))?;

        Ok(DecodedEvent::OptionMinted {
            series_id: series_id.to_string(),
            strike,
            expiry: 1735689600,
            is_call: true,
            contracts,
            writer: writer.to_string(),
        })
    }
}

pub struct OptionMintedDecoderV2;
impl EventDecoder for OptionMintedDecoderV2 {
    fn supports(&self, key: &DecoderKey) -> bool {
        key.event_topic == "mint" && key.schema_version == 2 && key.wasm_hash == "WASM_V2_HASH"
    }

    fn decode(&self, raw: &RawContractEvent) -> Result<DecodedEvent, DecodeError> {
        let series_id = raw.data["series_id"].as_str().ok_or(DecodeError::MissingField("series_id".into()))?;
        let strike = raw.data["strike_price"].as_f64().ok_or(DecodeError::MissingField("strike_price".into()))?;
        let contracts = raw.data["volume"].as_f64().ok_or(DecodeError::MissingField("volume".into()))?;
        let is_call = raw.data["is_call"].as_bool().ok_or(DecodeError::MissingField("is_call".into()))?;
        let writer = raw.data["writer"].as_str().ok_or(DecodeError::MissingField("writer".into()))?;

        Ok(DecodedEvent::OptionMinted {
            series_id: series_id.to_string(),
            strike,
            expiry: 1735689600,
            is_call,
            contracts,
            writer: writer.to_string(),
        })
    }
}
