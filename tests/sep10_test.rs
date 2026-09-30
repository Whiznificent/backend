mod common;

use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;
use zenith_backend::sep10::Sep10Server;

#[tokio::test]
async fn test_stellar_toml_published() {
    let app = common::TestApp::spawn().await;
    let (status, _headers, body) = app.get_raw("/.well-known/stellar.toml", None, None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
}

#[tokio::test]
async fn test_sep10_challenge_roundtrip_verification() {
    let mut server_seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut server_seed);
    let server_key = SigningKey::from_bytes(&server_seed);
    let server = Sep10Server::new(server_key, "zenith.finance".into());

    let mut client_seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut client_seed);
    let client_key = SigningKey::from_bytes(&client_seed);
    let client_addr = zenith_backend::strkey::encode_stellar_public_key(client_key.verifying_key().as_bytes());

    let challenge = server.build_challenge(&client_addr, "Test SDF Network ; September 2015");
    assert!(challenge.transaction.contains("SEP10_CHALLENGE"));

    // Client signs challenge transaction envelope
    let client_sig = client_key.sign(challenge.transaction.as_bytes());
    let client_sig_b64 = data_encoding::BASE64.encode(&client_sig.to_bytes());

    let verified_account = server.verify_challenge(&challenge.transaction, &client_sig_b64).unwrap();
    assert_eq!(verified_account, client_addr);
}
