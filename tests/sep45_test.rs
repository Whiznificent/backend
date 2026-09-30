mod common;

use zenith_backend::chain::simulate::SorobanAuthorizationEntry;
use zenith_backend::strkey::{decode_contract_id, encode_contract_id, validate_stellar_address, AddressType};

#[test]
fn test_strkey_contract_and_muxed_vectors() {
    let raw_contract_id: [u8; 32] = [0x55; 32];
    let contract_address = encode_contract_id(&raw_contract_id);
    assert!(contract_address.starts_with('C'));
    assert_eq!(validate_stellar_address(&contract_address).unwrap(), AddressType::Contract);

    let decoded = decode_contract_id(&contract_address).unwrap();
    assert_eq!(decoded, raw_contract_id);
}

#[tokio::test]
async fn test_smart_wallet_sep45_verification() {
    let app = common::TestApp::spawn().await;

    let contract_id = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM";

    let auth_entry = SorobanAuthorizationEntry {
        credentials_type: "address".into(),
        address: contract_id.into(),
        nonce: 1,
        signature_expiration_ledger: 1_005_000,
        signature_args: vec!["WEBAUTHN_PASSKEY_SIG_BASE64".into()],
    };

    let (status, body) = app
        .post(
            "/api/v1/auth/smart-wallet/verify",
            serde_json::json!({
                "contract_address": contract_id,
                "auth_entry": auth_entry,
            }),
        )
        .await;

    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(body["token"].is_string());
    assert_eq!(body["wallet_address"], contract_id);
}
