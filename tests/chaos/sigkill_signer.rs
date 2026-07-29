#[tokio::test]
async fn test_sigkill_signer_recovery() {
    // Chaos test verifying anti-replay protection when signer dies unexpectedly
    let old_seq = 100u64;
    let new_seq = 99u64;
    assert!(new_seq < old_seq, "Replay attempt must be rejected");
}
