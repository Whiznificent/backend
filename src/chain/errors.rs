//! Decoded error mapping for the transaction simulation preflight API.
//!
//! Soroban simulation failures surface as opaque host error codes. This module
//! translates them into stable API error codes with human-readable messages and
//! remediation hints so integrators can act before requesting a signature.
//!
//! Two classes of errors are handled:
//! * Zenith contract errors declared via `#[contracterror]` (mapped from the
//!   contract spec / error codes).
//! * Soroban host errors (budget exceeded, archived storage entries, auth
//!   failures, etc.).

use serde::{Deserialize, Serialize};

/// Stable API error code returned to clients. These are intentionally decoupled
/// from the on-chain numeric codes so the API surface stays stable even if the
/// contract renumbers its errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApiErrorCode {
    /// Insufficient collateral to satisfy the requested action.
    InsufficientCollateral,
    /// The referenced series has expired.
    SeriesExpired,
    /// The referenced series does not exist.
    SeriesNotFound,
    /// A required trustline is missing.
    MissingTrustline,
    /// The caller is not authorized for this action.
    Unauthorized,
    /// The transaction would exceed the network CPU instruction budget.
    BudgetExceeded,
    /// A storage entry required by the transaction has been archived.
    StorageArchived,
    /// Authentication requirements were not satisfied.
    AuthFailure,
    /// The action or XDR could not be parsed.
    InvalidRequest,
    /// An error that could not be mapped to a more specific code.
    Unknown,
}

/// A decoded error with a stable code, message, and remediation hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodedError {
    /// Stable API error code.
    pub code: ApiErrorCode,
    /// Human-readable message describing the failure.
    pub message: String,
    /// Actionable remediation hint for the integrator.
    pub remediation: String,
    /// The raw on-chain error code, when one was available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_code: Option<u32>,
}

impl DecodedError {
    fn new(
        code: ApiErrorCode,
        message: &str,
        remediation: &str,
        raw_code: Option<u32>,
    ) -> Self {
        Self {
            code,
            message: message.to_string(),
            remediation: remediation.to_string(),
            raw_code,
        }
    }
}

/// A single entry in the Zenith `#[contracterror]` mapping table.
///
/// The table is generated from the contract spec where possible; the numeric
/// codes below mirror the `#[contracterror]` discriminants declared by the
/// Zenith contract.
pub struct ContractErrorMapping {
    /// Numeric `#[contracterror]` code emitted by the contract.
    pub contract_code: u32,
    /// Stable API error code.
    pub api_code: ApiErrorCode,
    /// Human-readable message.
    pub message: &'static str,
    /// Remediation hint.
    pub remediation: &'static str,
}

/// Mapping table from Zenith `#[contracterror]` codes to stable API errors.
///
/// Keep this in sync with the contract's `#[contracterror]` enum. When the
/// contract spec is available at build time this table can be generated from
/// it; the static table below is the source of truth for the API surface.
pub const ZENITH_CONTRACT_ERRORS: &[ContractErrorMapping] = &[
    ContractErrorMapping {
        contract_code: 1,
        api_code: ApiErrorCode::InsufficientCollateral,
        message: "Insufficient collateral for the requested action",
        remediation: "Deposit additional collateral or reduce the requested amount before retrying.",
    },
    ContractErrorMapping {
        contract_code: 2,
        api_code: ApiErrorCode::SeriesExpired,
        message: "The referenced series has expired",
        remediation: "Select an active series or roll the position into a new series.",
    },
    ContractErrorMapping {
        contract_code: 3,
        api_code: ApiErrorCode::SeriesNotFound,
        message: "The referenced series does not exist",
        remediation: "Verify the series identifier and ensure it has been initialized.",
    },
    ContractErrorMapping {
        contract_code: 4,
        api_code: ApiErrorCode::MissingTrustline,
        message: "A required trustline is missing",
        remediation: "Establish the required trustline for the asset before retrying.",
    },
    ContractErrorMapping {
        contract_code: 5,
        api_code: ApiErrorCode::Unauthorized,
        message: "The caller is not authorized for this action",
        remediation: "Ensure the transaction is signed by the required account and satisfies auth requirements.",
    },
];

/// Look up a Zenith contract error by its numeric `#[contracterror]` code.
pub fn map_contract_error(contract_code: u32) -> DecodedError {
    match ZENITH_CONTRACT_ERRORS
        .iter()
        .find(|m| m.contract_code == contract_code)
    {
        Some(m) => DecodedError::new(
            m.api_code,
            m.message,
            m.remediation,
            Some(contract_code),
        ),
        None => DecodedError::new(
            ApiErrorCode::Unknown,
            "Unrecognized contract error",
            "Inspect the raw contract error code and consult the contract spec.",
            Some(contract_code),
        ),
    }
}

/// Recognised Soroban host error classes relevant to the preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostErrorClass {
    /// The transaction would exceed the network CPU instruction budget.
    BudgetExceeded,
    /// A storage entry required by the transaction has been archived.
    StorageArchived,
    /// Authentication requirements were not satisfied.
    AuthFailure,
}

/// Map a recognised host error class to a decoded error.
pub fn map_host_error(class: HostErrorClass) -> DecodedError {
    match class {
        HostErrorClass::BudgetExceeded => DecodedError::new(
            ApiErrorCode::BudgetExceeded,
            "The transaction would exceed the network CPU instruction budget",
            "Reduce the number of operations or split the action into smaller transactions.",
            None,
        ),
        HostErrorClass::StorageArchived => DecodedError::new(
            ApiErrorCode::StorageArchived,
            "A storage entry required by the transaction has been archived",
            "Restore the archived entries using the provided restore preamble, then retry.",
            None,
        ),
        HostErrorClass::AuthFailure => DecodedError::new(
            ApiErrorCode::AuthFailure,
            "Authentication requirements were not satisfied",
            "Provide the required signatures and ensure the auth entries match the invocation.",
            None,
        ),
    }
}

/// Decode a raw Soroban host error code into a [`DecodedError`].
///
/// Host error codes are grouped by class; unrecognised codes fall back to
/// [`ApiErrorCode::Unknown`] with the raw code preserved for diagnostics.
pub fn map_host_error_code(raw_code: u32) -> DecodedError {
    // Host error discriminants are grouped by class in the Soroban host error
    // enum. The ranges below cover the classes relevant to the preflight.
    let class = match raw_code {
        // Budget / resource exhaustion.
        0x0000_0001..=0x0000_000F => Some(HostErrorClass::BudgetExceeded),
        // Storage / archival errors.
        0x0000_0010..=0x0000_001F => Some(HostErrorClass::StorageArchived),
        // Authentication errors.
        0x0000_0020..=0x0000_002F => Some(HostErrorClass::AuthFailure),
        _ => None,
    };

    match class {
        Some(class) => {
            let mut decoded = map_host_error(class);
            decoded.raw_code = Some(raw_code);
            decoded
        }
        None => DecodedError::new(
            ApiErrorCode::Unknown,
            "Unrecognized host error",
            "Inspect the raw host error code and consult the Soroban host error reference.",
            Some(raw_code),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_contract_error() {
        let decoded = map_contract_error(1);
        assert_eq!(decoded.code, ApiErrorCode::InsufficientCollateral);
        assert_eq!(decoded.raw_code, Some(1));
        assert!(!decoded.remediation.is_empty());
    }

    #[test]
    fn maps_unknown_contract_error() {
        let decoded = map_contract_error(9999);
        assert_eq!(decoded.code, ApiErrorCode::Unknown);
        assert_eq!(decoded.raw_code, Some(9999));
    }

    #[test]
    fn maps_host_error_classes() {
        assert_eq!(
            map_host_error_code(0x0000_0001).code,
            ApiErrorCode::BudgetExceeded
        );
        assert_eq!(
            map_host_error_code(0x0000_0010).code,
            ApiErrorCode::StorageArchived
        );
        assert_eq!(
            map_host_error_code(0x0000_0020).code,
            ApiErrorCode::AuthFailure
        );
    }

    #[test]
    fn maps_unknown_host_error() {
        let decoded = map_host_error_code(0xDEAD_BEEF);
        assert_eq!(decoded.code, ApiErrorCode::Unknown);
        assert_eq!(decoded.raw_code, Some(0xDEAD_BEEF));
    }
}
