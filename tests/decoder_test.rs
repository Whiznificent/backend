use zenith_backend::chain::bindings::events::DecodedEvent;
use zenith_backend::indexer::decoder::{
    DecoderRegistry, FallbackStrategy, OptionMintedDecoderV1, OptionMintedDecoderV2,
    RawContractEvent,
};

#[tokio::test]
async fn test_contract_upgrade_multi_version_decoding() {
    let db_path = std::env::temp_dir().join(format!("zenith-decoder-1-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", db_path.display());
    let pool = zenith_backend::db::init_pool(&database_url).await;

    let mut registry = DecoderRegistry::new(FallbackStrategy::ExactOnly, pool.clone());
    registry.register(OptionMintedDecoderV1);
    registry.register(OptionMintedDecoderV2);

    let contract_id = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM";

    // 1. Record WASM upgrade history
    registry.record_wasm_upgrade(contract_id, "WASM_V1_HASH", 100).await.unwrap();
    registry.record_wasm_upgrade(contract_id, "WASM_V2_HASH", 200).await.unwrap();

    // 2. Decode event from V1 era
    let raw_v1 = RawContractEvent {
        contract_id: contract_id.to_string(),
        wasm_hash: "WASM_V1_HASH".to_string(),
        ledger: 150,
        tx_index: 0,
        op_index: 0,
        topics: vec!["mint".into()],
        data: serde_json::json!({
            "series_id": "XLM-CALL-0.12",
            "strike": 0.12,
            "contracts": 1000.0,
            "writer": "G_WRITER_1"
        }),
    };

    let event_v1 = registry.decode_and_dispatch(&raw_v1, 1).await.unwrap();
    match event_v1 {
        DecodedEvent::OptionMinted { strike, contracts, is_call, .. } => {
            assert_eq!(strike, 0.12);
            assert_eq!(contracts, 1000.0);
            assert!(is_call);
        }
        _ => panic!("Expected OptionMinted"),
    }

    // 3. Decode event from V2 era (layout changed: strike_price, volume, is_call)
    let raw_v2 = RawContractEvent {
        contract_id: contract_id.to_string(),
        wasm_hash: "WASM_V2_HASH".to_string(),
        ledger: 250,
        tx_index: 0,
        op_index: 0,
        topics: vec!["mint".into()],
        data: serde_json::json!({
            "series_id": "XLM-PUT-0.10",
            "strike_price": 0.10,
            "volume": 2500.0,
            "is_call": false,
            "writer": "G_WRITER_2"
        }),
    };

    let event_v2 = registry.decode_and_dispatch(&raw_v2, 2).await.unwrap();
    match event_v2 {
        DecodedEvent::OptionMinted { strike, contracts, is_call, .. } => {
            assert_eq!(strike, 0.10);
            assert_eq!(contracts, 2500.0);
            assert!(!is_call);
        }
        _ => panic!("Expected OptionMinted"),
    }

    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn test_unsupported_event_stored_and_pauses_contract() {
    let db_path = std::env::temp_dir().join(format!("zenith-decoder-2-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", db_path.display());
    let pool = zenith_backend::db::init_pool(&database_url).await;

    let registry = DecoderRegistry::new(FallbackStrategy::ExactOnly, pool.clone());
    let contract_id = "C_UNKNOWN_CONTRACT";

    let raw_unknown = RawContractEvent {
        contract_id: contract_id.to_string(),
        wasm_hash: "UNKNOWN_HASH".to_string(),
        ledger: 300,
        tx_index: 0,
        op_index: 0,
        topics: vec!["unknown_topic".into()],
        data: serde_json::json!({ "foo": "bar" }),
    };

    let res = registry.decode_and_dispatch(&raw_unknown, 1).await;
    assert!(res.is_err());
    assert!(registry.is_contract_paused(contract_id));

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM unprocessed_events WHERE contract_id = ?1")
        .bind(contract_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    let _ = std::fs::remove_file(db_path);
}
