//! Minimal AWS Signature Version 4 request signing, used by
//! [`crate::signing::AwsKmsSigner`] so the KMS private key never enters this
//! process (issue #120).
//!
//! Only the pieces KMS needs are implemented: header-based signing with an
//! `x-amz-date`, canonical request/headers, the derived signing key, and the
//! `Authorization` header. The implementation is checked against the canonical
//! example from the AWS documentation in the test below.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::secrets::SecretError;

type HmacSha256 = Hmac<Sha256>;

/// Everything needed to sign one request. `headers` is the set of request
/// headers that participate in the signature (at minimum `host`); a
/// `x-amz-date` header is added automatically from `amz_date` if absent.
pub struct SignableRequest<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub headers: Vec<(String, String)>,
    pub payload: &'a [u8],
    pub region: &'a str,
    pub service: &'a str,
    pub access_key: &'a str,
    pub secret_key: &'a str,
    /// UTC timestamp in `YYYYMMDDTHHMMSSZ` form.
    pub amz_date: &'a str,
}

fn hex(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(bytes)
}

fn sha256_hex(data: &[u8]) -> String {
    hex(Sha256::digest(data).as_slice())
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Sign `request`, returning the extra headers to attach (`Authorization`,
/// `X-Amz-Date`) alongside whatever the caller already set.
pub fn sign(request: &SignableRequest<'_>) -> Result<Vec<(String, String)>, SecretError> {
    if request.amz_date.len() != 16 || !request.amz_date.ends_with('Z') {
        return Err(SecretError::Backend(format!(
            "amz_date must be YYYYMMDDTHHMMSSZ, got {:?}",
            request.amz_date
        )));
    }

    let parsed = reqwest::Url::parse(request.url)
        .map_err(|e| SecretError::Backend(format!("invalid signing URL: {e}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| SecretError::Backend("signing URL has no host".to_string()))?;
    let host = match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };

    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    if !headers.iter().any(|(name, _)| name == "host") {
        headers.push(("host".to_string(), host));
    }
    if !headers.iter().any(|(name, _)| name == "x-amz-date") {
        headers.push(("x-amz-date".to_string(), request.amz_date.to_string()));
    }
    headers.sort();
    headers.dedup_by(|a, b| a.0 == b.0);

    let canonical_headers: String = headers
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect();
    let signed_headers = headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let canonical_uri = if parsed.path().is_empty() {
        "/"
    } else {
        parsed.path()
    };
    let canonical_query = parsed.query().unwrap_or("");
    let payload_hash = sha256_hex(request.payload);

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        request.method,
        canonical_uri,
        canonical_query,
        canonical_headers,
        signed_headers,
        payload_hash
    );

    let date = &request.amz_date[..8];
    let scope = format!("{date}/{}/{}/aws4_request", request.region, request.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        request.amz_date,
        scope,
        sha256_hex(canonical_request.as_bytes())
    );

    let k_date = hmac(
        format!("AWS4{}", request.secret_key).as_bytes(),
        date.as_bytes(),
    );
    let k_region = hmac(&k_date, request.region.as_bytes());
    let k_service = hmac(&k_region, request.service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        request.access_key
    );

    Ok(vec![
        ("Authorization".to_string(), authorization),
        ("X-Amz-Date".to_string(), request.amz_date.to_string()),
    ])
}

/// Format a UNIX timestamp as `YYYYMMDDTHHMMSSZ` (UTC), mirroring the
/// civil-from-days algorithm `auth.rs` uses for its own timestamps.
pub fn amz_timestamp(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86400);
    let rem = unix_secs.rem_euclid(86400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::{SystemTime, UNIX_EPOCH};

    /// The canonical `GET` example from the AWS SigV4 documentation
    /// ("Examples of the complete Version 4 signing process").
    #[test]
    fn matches_the_aws_documented_example() {
        let request = SignableRequest {
            method: "GET",
            url: "https://iam.amazonaws.com/?Action=ListUsers&Version=2010-05-08",
            headers: vec![
                (
                    "content-type".to_string(),
                    "application/x-www-form-urlencoded; charset=utf-8".to_string(),
                ),
                ("host".to_string(), "iam.amazonaws.com".to_string()),
            ],
            payload: b"",
            region: "us-east-1",
            service: "iam",
            access_key: "AKIDEXAMPLE",
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            amz_date: "20150830T123600Z",
        };

        let signed = sign(&request).unwrap();
        let authorization = signed
            .iter()
            .find(|(name, _)| name == "Authorization")
            .map(|(_, value)| value)
            .expect("Authorization header must be returned");

        assert!(
            authorization.ends_with(
                "Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
            ),
            "unexpected signature: {authorization}"
        );
        assert!(
            authorization.contains("Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request")
        );
        assert!(authorization.contains("SignedHeaders=content-type;host;x-amz-date"));
    }

    #[test]
    fn rejects_a_malformed_timestamp() {
        let request = SignableRequest {
            method: "POST",
            url: "https://kms.us-east-1.amazonaws.com/",
            headers: vec![],
            payload: b"{}",
            region: "us-east-1",
            service: "kms",
            access_key: "AKIDEXAMPLE",
            secret_key: "secret",
            amz_date: "not-a-timestamp",
        };
        assert!(matches!(sign(&request), Err(SecretError::Backend(_))));
    }

    #[test]
    fn formats_timestamps_in_amz_form() {
        // 2015-08-30T12:36:00Z
        assert_eq!(amz_timestamp(1_440_938_160), "20150830T123600Z");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let formatted = amz_timestamp(now);
        assert_eq!(formatted.len(), 16);
        assert!(formatted.ends_with('Z'));
        assert_eq!(&formatted[8..9], "T");
    }
}
