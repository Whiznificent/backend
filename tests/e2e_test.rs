mod e2e;

use e2e::QuickstartHarness;
use std::time::Duration;

#[tokio::test]
async fn test_quickstart_e2e_full_lifecycle_flow() {
    let harness = QuickstartHarness::start_or_connect().await;
    let ready = harness.wait_for_ready(Duration::from_secs(5)).await;
    assert!(ready.is_ok());

    let test_wallet = "GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5";
    let fund_res = harness.fund_account(test_wallet).await;
    assert!(fund_res.is_ok());
}
