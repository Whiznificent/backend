use zenith_backend::auth::{meets_threshold, AccountSigner, SignerSignature};

#[test]
fn test_multisig_threshold_evaluation_table() {
    let signers = vec![
        AccountSigner { key: "G_SIGNER_1".into(), weight: 1 },
        AccountSigner { key: "G_SIGNER_2".into(), weight: 2 },
        AccountSigner { key: "G_SIGNER_3".into(), weight: 0 }, // zero weight master key
    ];

    let threshold = 3;

    // 1 + 2 = 3 >= 3 -> Pass
    let sigs_pass = vec![
        SignerSignature { public_key: "G_SIGNER_1".into(), signature: "sig1".into() },
        SignerSignature { public_key: "G_SIGNER_2".into(), signature: "sig2".into() },
    ];
    assert!(meets_threshold(&signers, threshold, &sigs_pass));

    // Only signer 1 (weight 1 < 3) -> Fail
    let sigs_fail = vec![
        SignerSignature { public_key: "G_SIGNER_1".into(), signature: "sig1".into() },
    ];
    assert!(!meets_threshold(&signers, threshold, &sigs_fail));

    // Zero-weight master key signature gives 0 weight -> Fail
    let sigs_zero = vec![
        SignerSignature { public_key: "G_SIGNER_3".into(), signature: "sig3".into() },
        SignerSignature { public_key: "G_SIGNER_2".into(), signature: "sig2".into() },
    ];
    assert!(!meets_threshold(&signers, threshold, &sigs_zero)); // 0 + 2 = 2 < 3

    // Duplicate signatures from same signer are only counted once -> Fail
    let sigs_dup = vec![
        SignerSignature { public_key: "G_SIGNER_2".into(), signature: "sig2".into() },
        SignerSignature { public_key: "G_SIGNER_2".into(), signature: "sig2_dup".into() },
    ];
    assert!(!meets_threshold(&signers, threshold, &sigs_dup)); // 2 < 3
}
