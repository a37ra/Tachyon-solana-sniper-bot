//! Test to prove signer validation fix for slippage spoofing attack
//!
//! This test demonstrates that the signer now correctly rejects intents
//! where the declared hard_cap_bps in metadata does not match the actual
//! slippage in the decoded transaction instruction.

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
const RAYDIUM_AMM_V4: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    println!("=== SLIPPAGE VALIDATION TEST ===");
    println!("Testing slippage spoofing attack prevention\n");

    // Load test keypair
    let keypair = load_test_keypair()?;
    let pubkey = keypair.pubkey();
    println!("Test keypair pubkey: {}", pubkey);

    // Create signer client
    let signer_client = SignerClient::new(TEST_SIGNER_SOCKET.to_string());

    // Build a fake Raydium swap instruction with high slippage
    // Format: discriminator (1 byte) + amount_in (u64) + minimum_amount_out (u64)
    let amount_in = 1_000_000_000u64; // 1 SOL
    let minimum_amount_out = 500_000_000u64; // 0.5 SOL (50% slippage!)

    let mut instruction_data = Vec::with_capacity(17);
    instruction_data.push(9u8); // Raydium AMM v4 swap discriminator
    instruction_data.extend_from_slice(&amount_in.to_le_bytes());
    instruction_data.extend_from_slice(&minimum_amount_out.to_le_bytes());

    // Create a transaction with this instruction
    let raydium_program = Pubkey::from_str(RAYDIUM_AMM_V4)?;
    let instruction = solana_sdk::instruction::Instruction {
        program_id: raydium_program,
        accounts: vec![
            solana_sdk::instruction::AccountMeta::new_readonly(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new_readonly(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new_readonly(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
            solana_sdk::instruction::AccountMeta::new(Pubkey::new_unique(), false),
        ],
        data: instruction_data,
    };

    let tx = Transaction::new_with_payer(&[instruction], Some(&pubkey));

    // Convert to base64
    let unsigned_tx_bytes = bincode::serialize(&tx)?;
    let unsigned_tx_base64 = base64::engine::general_purpose::STANDARD.encode(&unsigned_tx_bytes);

    println!("\n--- TEST 1: FAKE SLIPPAGE (50% actual, 1% declared) ---");
    let unique_pool = format!("slippage-fake-{}", Utc::now().timestamp_millis());
    let fake_request = SignerIntentRequest {
        context_key: unique_pool.clone(),
        ipc_seq: 0,
        issued_at_unix_ms: Utc::now().timestamp_millis() as u64,
        intent_type: IntentType::Buy,
        unsigned_transaction_base64: unsigned_tx_base64.clone(),
        program_id: RAYDIUM_AMM_V4.to_string(),
        dex_program_id: RAYDIUM_AMM_V4.to_string(),
        max_size_lamports: amount_in,
        hard_cap_bps: 100, // 1% slippage declared (LIE)
        destination_addresses: vec![],
        mint: "".to_string(),
        pool_address: unique_pool.clone(),
        position_id: None,
        amount_or_pct: 1.0,
    };

    match signer_client.sign_intent(fake_request).await {
        Ok(response) => {
            if response.status == VerdictStatus::Accepted {
                println!("❌ TEST 1 FAILED: Signer accepted high slippage intent (BUG!)");
                println!("   This should have been rejected with SlippageMismatch");
            } else {
                println!("✅ TEST 1 PASSED: Signer rejected high slippage intent");
                println!("   Rejection reason: {:?}", response.rejection_reason);
            }
        }
        Err(e) => {
            println!("✅ TEST 1 PASSED: Signer rejected high slippage intent with error");
            println!("   Error: {}", e);
        }
    }

    println!("\n--- TEST 2: LEGITIMATE SLIPPAGE (50% actual, 50% declared) ---");
    let unique_pool2 = format!("slippage-legit-{}", Utc::now().timestamp_millis());
    let legit_request = SignerIntentRequest {
        context_key: unique_pool2.clone(),
        ipc_seq: 0,
        issued_at_unix_ms: Utc::now().timestamp_millis() as u64,
        intent_type: IntentType::Buy,
        unsigned_transaction_base64: unsigned_tx_base64.clone(),
        program_id: RAYDIUM_AMM_V4.to_string(),
        dex_program_id: RAYDIUM_AMM_V4.to_string(),
        max_size_lamports: amount_in,
        hard_cap_bps: 5000, // 50% slippage declared (TRUTH)
        destination_addresses: vec![],
        mint: "".to_string(),
        pool_address: unique_pool2.clone(),
        position_id: None,
        amount_or_pct: 1.0,
    };

    match signer_client.sign_intent(legit_request).await {
        Ok(response) => {
            if response.status == VerdictStatus::Accepted {
                println!("✅ TEST 2 PASSED: Signer accepted legitimate slippage intent");
            } else {
                println!("❌ TEST 2 FAILED: Signer rejected legitimate slippage intent (BUG!)");
                println!("   Rejection reason: {:?}", response.rejection_reason);
            }
        }
        Err(e) => {
            println!("❌ TEST 2 FAILED: Signer rejected legitimate slippage intent with error");
            println!("   Error: {}", e);
        }
    }

    println!("\n=== SLIPPAGE VALIDATION TEST COMPLETE ===");
    Ok(())
}

fn load_test_keypair() -> Result<Keypair> {
    let json_str = std::fs::read_to_string(TEST_KEYPAIR_PATH)?;
    let bytes: Vec<u8> = serde_json::from_str(&json_str)?;
    let keypair = Keypair::from_bytes(&bytes)?;
    Ok(keypair)
}
