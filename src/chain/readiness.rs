use super::horizon::{AccountResponse, HorizonClient};
use serde::{Deserialize, Serialize};

pub const BASE_RESERVE: f64 = 0.5; // 0.5 XLM per entry on Stellar

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadinessItem {
    pub name: String,
    pub status: bool,
    pub remediation: String,
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessReport {
    pub ready: bool,
    pub wallet_address: String,
    pub checks: Vec<ReadinessItem>,
}

pub async fn check_wallet_readiness(
    horizon: &HorizonClient,
    wallet: &str,
    usdc_issuer: &str,
    required_usdc: f64,
    required_xlm: f64,
) -> ReadinessReport {
    let mut checks = Vec::new();

    let account_res = horizon.get_account(wallet).await;
    let account: Option<AccountResponse> = match account_res {
        Ok(acc) => {
            checks.push(ReadinessItem {
                name: "account_exists".into(),
                status: true,
                remediation: "".into(),
                details: Some(serde_json::json!({ "sequence": acc.sequence })),
            });
            Some(acc)
        }
        Err(_) => {
            checks.push(ReadinessItem {
                name: "account_exists".into(),
                status: false,
                remediation: format!("Create and fund account {wallet} on Stellar with at least 1.5 XLM."),
                details: None,
            });
            None
        }
    };

    if let Some(acc) = account {
        // Minimum reserve calculation
        let subentries = acc.subentry_count as f64;
        let sponsoring = acc.num_sponsoring.unwrap_or(0) as f64;
        let sponsored = acc.num_sponsored.unwrap_or(0) as f64;
        let min_reserve = (2.0 + subentries + sponsoring - sponsored) * BASE_RESERVE;

        let native_balance_line = acc.balances.iter().find(|b| b.asset_type == "native");
        let native_balance: f64 = native_balance_line
            .and_then(|b| b.balance.parse().ok())
            .unwrap_or(0.0);
        let selling_liabilities: f64 = native_balance_line
            .and_then(|b| b.selling_liabilities.as_ref())
            .and_then(|l| l.parse().ok())
            .unwrap_or(0.0);

        let available_xlm = (native_balance - min_reserve - selling_liabilities).max(0.0);
        let min_reserve_ok = available_xlm >= required_xlm;

        checks.push(ReadinessItem {
            name: "min_reserve_ok".into(),
            status: min_reserve_ok,
            remediation: if min_reserve_ok {
                "".into()
            } else {
                format!("Deposit at least {:.2} additional XLM to cover base reserve ({:.2} XLM) and actions.", (required_xlm - available_xlm).max(0.1), min_reserve)
            },
            details: Some(serde_json::json!({
                "native_balance": native_balance,
                "min_reserve": min_reserve,
                "available_xlm": available_xlm,
            })),
        });

        // USDC Trustline verification
        let usdc_line = acc.balances.iter().find(|b| {
            (b.asset_code.as_deref() == Some("USDC") || b.asset_code.as_deref() == Some("USDC_SAC"))
                && b.asset_issuer.as_deref() == Some(usdc_issuer)
        });

        match usdc_line {
            Some(line) => {
                let limit: f64 = line.limit.as_ref().and_then(|l| l.parse().ok()).unwrap_or(f64::MAX);
                let usdc_balance: f64 = line.balance.parse().unwrap_or(0.0);

                checks.push(ReadinessItem {
                    name: "trustline_usdc".into(),
                    status: limit >= required_usdc,
                    remediation: if limit >= required_usdc {
                        "".into()
                    } else {
                        format!("Increase USDC trustline limit to at least {required_usdc} USDC.")
                    },
                    details: Some(serde_json::json!({ "limit": limit, "balance": usdc_balance })),
                });

                let balance_ok = usdc_balance >= required_usdc;
                checks.push(ReadinessItem {
                    name: "balance_sufficient_for_action".into(),
                    status: balance_ok,
                    remediation: if balance_ok {
                        "".into()
                    } else {
                        format!("Deposit at least {:.2} additional USDC (current balance: {:.2} USDC).", required_usdc - usdc_balance, usdc_balance)
                    },
                    details: Some(serde_json::json!({ "required": required_usdc, "current": usdc_balance })),
                });
            }
            None => {
                checks.push(ReadinessItem {
                    name: "trustline_usdc".into(),
                    status: false,
                    remediation: format!("Establish a USDC trustline for asset code USDC issued by {usdc_issuer}."),
                    details: None,
                });
                checks.push(ReadinessItem {
                    name: "balance_sufficient_for_action".into(),
                    status: false,
                    remediation: format!("Deposit at least {required_usdc} USDC after establishing trustline."),
                    details: None,
                });
            }
        }
    } else {
        checks.push(ReadinessItem {
            name: "min_reserve_ok".into(),
            status: false,
            remediation: "Account must be funded with XLM first.".into(),
            details: None,
        });
        checks.push(ReadinessItem {
            name: "trustline_usdc".into(),
            status: false,
            remediation: "Establish USDC trustline once account is funded.".into(),
            details: None,
        });
        checks.push(ReadinessItem {
            name: "balance_sufficient_for_action".into(),
            status: false,
            remediation: "Deposit required collateral once account is ready.".into(),
            details: None,
        });
    }

    let ready = checks.iter().all(|c| c.status);
    ReadinessReport {
        ready,
        wallet_address: wallet.to_string(),
        checks,
    }
}

#[derive(Deserialize)]
pub struct ReadinessQuery {
    pub wallet_address: String,
    pub required_usdc: Option<f64>,
    pub required_xlm: Option<f64>,
}

pub async fn get_readiness_handler(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    crate::error::AppQuery(q): crate::error::AppQuery<ReadinessQuery>,
) -> axum::response::Json<ReadinessReport> {
    let horizon = HorizonClient::new(state.network.horizon_url.clone());
    let report = check_wallet_readiness(
        &horizon,
        &q.wallet_address,
        &state.network.asset_issuers.usdc_issuer,
        q.required_usdc.unwrap_or(0.0),
        q.required_xlm.unwrap_or(0.0),
    )
    .await;
    axum::response::Json(report)
}
