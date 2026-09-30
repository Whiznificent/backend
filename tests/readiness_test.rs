mod common;

use zenith_backend::chain::horizon::{AccountResponse, BalanceLine, HorizonClient};
use zenith_backend::chain::readiness::check_wallet_readiness;
use zenith_backend::chain::tx_builder::TxBuilder;

#[tokio::test]
async fn test_wallet_readiness_unfunded_account() {
    let client = HorizonClient::new("https://horizon-testnet.stellar.org".into());
    let report = check_wallet_readiness(
        &client,
        "GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5",
        "GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5",
        100.0,
        1.0,
    )
    .await;

    assert!(!report.ready);
    assert_eq!(report.checks.len(), 4);
    assert!(!report.checks[0].status);
    assert!(report.checks[0].remediation.contains("fund account"));
}

#[tokio::test]
async fn test_wallet_readiness_funded_with_trustline_and_balance() {
    let client = HorizonClient::new("https://horizon-testnet.stellar.org".into());
    let wallet = "GAAAZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ";
    let issuer = "GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5";

    let mock_account = AccountResponse {
        id: wallet.to_string(),
        sequence: "123456789".to_string(),
        subentry_count: 1,
        num_sponsoring: None,
        num_sponsored: None,
        balances: vec![
            BalanceLine {
                asset_type: "native".into(),
                balance: "10.0".into(),
                limit: None,
                asset_code: None,
                asset_issuer: None,
                buying_liabilities: None,
                selling_liabilities: None,
                is_authorized: None,
            },
            BalanceLine {
                asset_type: "credit_alphanum4".into(),
                balance: "500.0".into(),
                limit: Some("10000.0".into()),
                asset_code: Some("USDC".into()),
                asset_issuer: Some(issuer.to_string()),
                buying_liabilities: None,
                selling_liabilities: None,
                is_authorized: Some(true),
            },
        ],
    };

    client.insert_cached_account(mock_account);

    let report = check_wallet_readiness(&client, wallet, issuer, 100.0, 1.0).await;
    assert!(report.ready);
    assert!(report.checks.iter().all(|c| c.status));

    // Fast-fail tx builder test
    let tx_res = TxBuilder::build_deposit_collateral(&client, wallet, issuer, 50.0).await;
    assert!(tx_res.is_ok());

    // Excess required USDC fails
    let tx_fail = TxBuilder::build_deposit_collateral(&client, wallet, issuer, 1000.0).await;
    assert!(tx_fail.is_err());
}
