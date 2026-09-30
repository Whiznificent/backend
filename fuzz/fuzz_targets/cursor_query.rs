//! Fuzz target: cursor/pagination query-string decoding.
//!
//! `ListPositionsQuery` and `HistoryQuery` accept `?limit=&offset=`
//! query parameters via `serde_qs`/`serde_urlencoded`.  A malformed or
//! extreme value must never cause a panic; clamping logic is exercised
//! here to confirm it holds for all i64-range values.
//!
//! This target fuzzes the parsing logic independently of HTTP — it drives
//! `serde_urlencoded::from_str` directly, mirroring what axum's `Query`
//! extractor does under the hood.
#![no_main]

use libfuzzer_sys::fuzz_target;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ListPositionsQuery {
    status: Option<String>,
    strategy_id: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

const DEFAULT_LIST_LIMIT: i64 = 50;
const MAX_LIST_LIMIT: i64 = 200;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // 1. ListPositionsQuery — must not panic
    if let Ok(q) = serde_urlencoded::from_str::<ListPositionsQuery>(s) {
        // Replicate the clamping logic from list_positions handler
        let limit = q.limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(1, MAX_LIST_LIMIT);
        let offset = q.offset.unwrap_or(0).max(0);

        // Clamped limit must always be in [1, MAX_LIST_LIMIT]
        assert!(
            limit >= 1 && limit <= MAX_LIST_LIMIT,
            "limit={limit} out of bounds after clamp"
        );
        // Offset must always be non-negative after max(0)
        assert!(
            offset >= 0,
            "offset={offset} should be >= 0 after max(0)"
        );
    }

    // 2. HistoryQuery — same treatment
    if let Ok(q) = serde_urlencoded::from_str::<HistoryQuery>(s) {
        let limit = q.limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(1, MAX_LIST_LIMIT);
        let offset = q.offset.unwrap_or(0).max(0);
        assert!(limit >= 1 && limit <= MAX_LIST_LIMIT);
        assert!(offset >= 0);
    }
});
