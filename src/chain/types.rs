//! Typed request/response structures for the Soroban RPC client.
//!
//! These mirror the JSON-RPC 2.0 shapes returned by Stellar RPC
//! (<https://developers.stellar.org/docs/data/apis/rpc/api-reference>).

use serde::{Deserialize, Serialize};

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcRequest<P> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'static str,
    pub params: P,
}

impl<P> JsonRpcRequest<P> {
    pub fn new(id: u64, method: &'static str, params: P) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method,
            params,
        }
    }
}

/// JSON-RPC 2.0 response envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse<R> {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(default)]
    pub result: Option<R>,
    #[serde(default)]
    pub error: Option<JsonRpcErrorObject>,
}

/// The `error` member of a JSON-RPC 2.0 response.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

/// Result of `getHealth`.
#[derive(Debug, Clone, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    #[serde(default)]
    pub latest_ledger: Option<u32>,
    #[serde(default)]
    pub oldest_ledger: Option<u32>,
}

/// Result of `getLatestLedger`.
#[derive(Debug, Clone, Deserialize)]
pub struct LatestLedgerResponse {
    pub id: String,
    pub sequence: u32,
    #[serde(default)]
    pub protocol_version: Option<u32>,
}

/// A single ledger entry key, base64-encoded XDR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerKey {
    pub xdr: String,
}

/// Params for `getLedgerEntries`.
#[derive(Debug, Clone, Serialize)]
pub struct GetLedgerEntriesParams {
    pub keys: Vec<String>,
}

/// A single ledger entry returned by `getLedgerEntries`.
#[derive(Debug, Clone, Deserialize)]
pub struct LedgerEntryResult {
    pub key: String,
    pub xdr: String,
    #[serde(default)]
    pub last_modified_ledger_seq: Option<u32>,
    #[serde(default)]
    pub live_until_ledger_seq: Option<u32>,
}

/// Result of `getLedgerEntries`.
#[derive(Debug, Clone, Deserialize)]
pub struct GetLedgerEntriesResponse {
    #[serde(default)]
    pub entries: Vec<LedgerEntryResult>,
    #[serde(default)]
    pub latest_ledger: Option<u32>,
}

/// Params for `getEvents`.
#[derive(Debug, Clone, Serialize)]
pub struct GetEventsParams {
    pub start_ledger: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_ledger: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_ids: Option<Vec<String>>,
}

/// A single event returned by `getEvents`.
#[derive(Debug, Clone, Deserialize)]
pub struct EventResult {
    #[serde(rename = "type")]
    pub kind: String,
    pub ledger: u32,
    #[serde(default)]
    pub ledger_closed_at: Option<String>,
    pub contract_id: String,
    pub id: String,
    #[serde(default)]
    pub topic: Vec<String>,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub tx_hash: Option<String>,
}

/// Result of `getEvents`.
#[derive(Debug, Clone, Deserialize)]
pub struct GetEventsResponse {
    #[serde(default)]
    pub events: Vec<EventResult>,
    #[serde(default)]
    pub latest_ledger: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Params for `simulateTransaction`.
#[derive(Debug, Clone, Serialize)]
pub struct SimulateTransactionParams {
    pub transaction: String,
}

/// Result of `simulateTransaction`.
#[derive(Debug, Clone, Deserialize)]
pub struct SimulateTransactionResponse {
    #[serde(default)]
    pub transaction_data: Option<String>,
    #[serde(default)]
    pub min_resource_fee: Option<String>,
    #[serde(default)]
    pub results: Vec<SimulateTransactionResult>,
    #[serde(default)]
    pub latest_ledger: Option<u32>,
    #[serde(default)]
    pub error: Option<String>,
}

/// A single result entry within a `simulateTransaction` response.
#[derive(Debug, Clone, Deserialize)]
pub struct SimulateTransactionResult {
    #[serde(default)]
    pub xdr: Option<String>,
    #[serde(default)]
    pub auth: Vec<String>,
}

/// Params for `sendTransaction`.
#[derive(Debug, Clone, Serialize)]
pub struct SendTransactionParams {
    pub transaction: String,
}

/// Result of `sendTransaction`.
#[derive(Debug, Clone, Deserialize)]
pub struct SendTransactionResponse {
    pub status: String,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub latest_ledger: Option<u32>,
    #[serde(default)]
    pub error_result_xdr: Option<String>,
}

/// Params for `getTransaction`.
#[derive(Debug, Clone, Serialize)]
pub struct GetTransactionParams {
    pub hash: String,
}

/// Result of `getTransaction`.
#[derive(Debug, Clone, Deserialize)]
pub struct GetTransactionResponse {
    pub status: String,
    #[serde(default)]
    pub ledger: Option<u32>,
    #[serde(default)]
    pub envelope_xdr: Option<String>,
    #[serde(default)]
    pub result_xdr: Option<String>,
    #[serde(default)]
    pub result_meta_xdr: Option<String>,
    #[serde(default)]
    pub latest_ledger: Option<u32>,
}
