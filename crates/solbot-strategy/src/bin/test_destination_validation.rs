//! Test to prove signer validation fix for destination spoofing attack
//!
//! This test demonstrates that the signer now correctly rejects intents
//! where the declared destination_addresses in metadata do not match the
//! actual account keys in the decoded transaction.

use anyhow::Result;
use base64::Engine;
use chrono::Utc;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_sdk::signature::Signer;
use solana_transaction::Transaction;
use std::str::FromStr;
use tracing_subscriber;

use solbot_common::types::intent::{IntentType, SignerIntentRequest, VerdictStatus};
use solbot_strategy::signer_client::SignerClient;

const TEST_SIGNER_SOCKET: &str = "/tmp/solbot-signer-test.sock";
const TEST_KEYPAIR_PATH: &str = "/tmp/test-keypair.json";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    println!("=== DESTINATION VALIDATION TEST ===");
    println!("Testing destination spoofing attack prevention\n");

    // Load test keypair
    let keypair = load_test_keypair()?;
    let pubkey = keypair.pubkey();
    println!("Test keypair pubkey: {}", pubkey);

    // Create signer client
    let signer_client = SignerClient::new(TEST_SIGNER_SOCKET.to_string());

    // Build a system transfer transaction
    let actual_recipient = Pubkey::new_unique();
    let fake_recipient = Pubkey::new_unique();
    let amount_in_lamports = 10_000_000; // 0.01 SOL
    let tx = Transaction::new_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &pubkey,
            &actual_recipient,
            amount_in_lamports,
        )],
        Some(&pubkey),
    );

    // Convert to base64
    let unsigned_tx_bytes = bincode::serialize(&tx)?;
    let unsigned_tx_base64 = base64::engine::general_purpose::STANDARD.encode(&unsigned_tx_bytes);

    println!("\n--- TEST 1: FAKE DESTINATION (claims safe wallet, sends to attacker) ---");
    let unique_pool = format!("dest-fake-{}", Utc::now().timestamp_millis());
    let fake_request = SignerIntentRequest {
        context_key: unique_pool.clone(),
        ipc_seq: 0,
        issued_at_unix_ms: Utc::now().timestamp_millis() as u64,
        intent_type: IntentType::Buy,
        unsigned_transaction_base64: unsigned_tx_base64.clone(),
        program_id: "11111111111111111111111111111111".to_string(),
        dex_program_id: "11111111111111111111111111111111".to_string(),
        max_size_lamports: amount_in_lamports,
        hard_cap_bps: 1000,
        destination_addresses: vec![fake_recipient.to_string()], // Claims tokens go here (LIE)
        mint: "".to_string(),
        pool_address: unique_pool.clone(),
        position_id: None,
        amount_or_pct: 0.01,
    };

    match signer_client.sign_intent(fake_request).await {
        Ok(response) => {
            if response.status == VerdictStatus::Accepted {
                println!("❌ TEST 1 FAILED: Signer accepted fake destination intent (BUG!)");
                println!("   This should have been rejected with DestinationMismatch");
            } else {
                println!("✅ TEST 1 PASSED: Signer rejected fake destination intent");
                println!("   Rejection reason: {:?}", response.rejection_reason);
            }
        }
        Err(e) => {
            println!("✅ TEST 1 PASSED: Signer rejected fake destination intent with error");
            println!("   Error: {}", e);
        }
    }

    println!("\n--- TEST 2: LEGITIMATE DESTINATION (declares actual recipient) ---");
    let unique_pool2 = format!("dest-legit-{}", Utc::now().timestamp_millis());
    let legit_request = SignerIntentRequest {
        context_key: unique_pool2.clone(),
        ipc_seq: 0,
        issued_at_unix_ms: Utc::now().timestamp_millis() as u64,
        intent_type: IntentType::Buy,
        unsigned_transaction_base64: unsigned_tx_base64.clone(),
        program_id: "11111111111111111111111111111111".to_string(),
        dex_program_id: "11111111111111111111111111111111".to_string(),
        max_size_lamports: amount_in_lamports,
        hard_cap_bps: 1000,
        destination_addresses: vec![actual_recipient.to_string()], // Declares actual recipient (TRUTH)
        mint: "".to_string(),
        pool_address: unique_pool2.clone(),
        position_id: None,
        amount_or_pct: 0.01,
    };

    match signer_client.sign_intent(legit_request).await {
        Ok(response) => {
            if response.status == VerdictStatus::Accepted {
                println!("✅ TEST 2 PASSED: Signer accepted legitimate destination intent");
            } else {
                println!("❌ TEST 2 FAILED: Signer rejected legitimate destination intent (BUG!)");
                println!("   Rejection reason: {:?}", response.rejection_reason);
            }
        }
        Err(e) => {
            println!("❌ TEST 2 FAILED: Signer rejected legitimate destination intent with error");
            println!("   Error: {}", e);
        }
    }

    println!("\n--- TEST 3: NO DESTINATIONS DECLARED (should pass) ---");
    let unique_pool3 = format!("dest-empty-{}", Utc::now().timestamp_millis());
    let empty_request = SignerIntentRequest {
        context_key: unique_pool3.clone(),
        ipc_seq: 0,
        issued_at_unix_ms: Utc::now().timestamp_millis() as u64,
        intent_type: IntentType::Buy,
        unsigned_transaction_base64: unsigned_tx_base64.clone(),
        program_id: "11111111111111111111111111111111".to_string(),
        dex_program_id: "11111111111111111111111111111111".to_string(),
        max_size_lamports: amount_in_lamports,
        hard_cap_bps: 1000,
        destination_addresses: vec![], // No destinations declared (should skip validation)
        mint: "".to_string(),
        pool_address: unique_pool3.clone(),
        position_id: None,
        amount_or_pct: 0.01,
    };

    match signer_client.sign_intent(empty_request).await {
        Ok(response) => {
            if response.status == VerdictStatus::Accepted {
                println!("✅ TEST 3 PASSED: Signer accepted intent with no destinations");
            } else {
                println!("❌ TEST 3 FAILED: Signer rejected intent with no destinations (BUG!)");
                println!("   Rejection reason: {:?}", response.rejection_reason);
            }
        }
        Err(e) => {
            println!("❌ TEST 3 FAILED: Signer rejected intent with no destinations with error");
            println!("   Error: {}", e);
        }
    }

    println!("\n=== DESTINATION VALIDATION TEST COMPLETE ===");
    Ok(())
}

fn load_test_keypair() -> Result<Keypair> {
    let json_str = std::fs::read_to_string(TEST_KEYPAIR_PATH)?;
    let bytes: Vec<u8> = serde_json::from_str(&json_str)?;
    let keypair = Keypair::from_bytes(&bytes)?;
    Ok(keypair)
}
