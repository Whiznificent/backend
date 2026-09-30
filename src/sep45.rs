use crate::chain::simulate::{SorobanAuthorizationEntry, TransactionSimulator};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Sep45Challenge {
    pub contract_address: String,
    pub web_auth_contract: String,
    pub nonce: i64,
    pub min_ledger: u32,
    pub max_ledger: u32,
}

#[derive(Debug, Deserialize)]
pub struct Sep45VerifyRequest {
    pub contract_address: String,
    pub auth_entry: SorobanAuthorizationEntry,
}

#[derive(Debug, Serialize)]
pub struct Sep45VerifyResponse {
    pub token: String,
    pub wallet_address: String,
}

pub async fn post_sep45_verify(
    State(state): State<crate::AppState>,
    Json(req): Json<Sep45VerifyRequest>,
) -> Result<Json<Sep45VerifyResponse>, StatusCode> {
    // Validate that contract_address is a valid C... StrKey
    if crate::strkey::decode_contract_id(&req.contract_address).is_err() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let current_ledger = 1_000_000;
    let sim_result = TransactionSimulator::simulate_check_auth(
        &req.contract_address,
        &req.auth_entry,
        current_ledger,
    )
    .await;

    if !sim_result.success || !sim_result.auth_executed {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Insert or confirm account in database
    sqlx::query(
        "INSERT INTO accounts (wallet_address) VALUES (?) ON CONFLICT(wallet_address) DO NOTHING",
    )
    .bind(&req.contract_address)
    .execute(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let token = crate::auth::random_token_hex(32);
    let session_expires_at = crate::auth::format_unix_secs(
        crate::auth::now_unix() + crate::auth::SESSION_TTL_SECS,
    );

    sqlx::query("INSERT INTO sessions (token, wallet_address, expires_at) VALUES (?, ?, ?)")
        .bind(&token)
        .bind(&req.contract_address)
        .bind(&session_expires_at)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(Sep45VerifyResponse {
        token,
        wallet_address: req.contract_address,
    }))
}
