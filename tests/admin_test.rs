mod common;

use common::TestApp;
use ed25519_dalek::Signer;
use serde_json::json;

#[tokio::test]
async fn admin_writes_require_role_and_fresh_wallet_step_up() {
    let app = TestApp::spawn().await;
    let identity = app.login_identity().await;
    sqlx::query("INSERT INTO admin_roles (wallet_address, role) VALUES (?, 'viewer')")
        .bind(&identity.wallet_address)
        .execute(&app.db)
        .await
        .unwrap();

    let (status, _) = app
        .post_with(
            "/api/v1/admin/series",
            json!({"underlying": "BTC", "expires_at": "2030-01-01T00:00:00Z"}),
            Some(&identity.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);

    sqlx::query("DELETE FROM admin_roles WHERE wallet_address = ?")
        .bind(&identity.wallet_address)
        .execute(&app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_roles (wallet_address, role) VALUES (?, 'operator')")
        .bind(&identity.wallet_address)
        .execute(&app.db)
        .await
        .unwrap();

    let (status, _) = app
        .post_with(
            "/api/v1/admin/series",
            json!({"underlying": "BTC", "expires_at": "2030-01-01T00:00:00Z"}),
            Some(&identity.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::PRECONDITION_REQUIRED);

    let (status, challenge) = app
        .post_with(
            "/api/v1/admin/auth/step-up/nonce",
            json!({}),
            Some(&identity.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let message = challenge["message"].as_str().unwrap();
    let signature = identity.signing_key.sign(message.as_bytes());
    let signature = data_encoding::BASE64.encode(&signature.to_bytes());
    let (status, _) = app
        .post_with(
            "/api/v1/admin/auth/step-up/verify",
            json!({"message": message, "signature": &signature}),
            Some(&identity.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK);

    let (status, _) = app
        .post_with(
            "/api/v1/admin/auth/step-up/verify",
            json!({"message": message, "signature": &signature}),
            Some(&identity.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);

    let (status, series) = app
        .post_with(
            "/api/v1/admin/series",
            json!({"underlying": "BTC", "expires_at": "2030-01-01T00:00:00Z"}),
            Some(&identity.token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(series["underlying"], "BTC");
    assert_eq!(series["active"], true);
}
