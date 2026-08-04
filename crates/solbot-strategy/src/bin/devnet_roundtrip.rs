//! Real Devnet Roundtrip Test for Phase 1 (1.2 Tx Builder & 1.3 Signer IPC gRPC)
//!
//! 1. Generates or loads a devnet Keypair.
//! 2. Requests an airdrop of SOL on Devnet if balance < 0.1 SOL.
//! 3. Starts `solbot-signer` as an isolated child process over UDS `/tmp/solbot-signer-devnet.sock`.
//! 4. Uses `SignerClient` to request signing for a real Devnet transaction constructed by `tx_builder`.
//! 5. Receives the signed base64 transaction from `solbot-signer`.
//! 6. Submits the signed transaction to Devnet RPC.
//! 7. Waits for confirmation on-chain and verifies execution!

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_signer::Signer;
use solbot_common::types::intent::{IntentType, SignerIntentRequest, VerdictStatus};
use solbot_strategy::signer_client::SignerClient;
use solbot_strategy::tx_builder::{
    ata, blockhash_cache::BlockhashCache, build_unsigned_transaction, SwapInstructions,
};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

const DEVNET_RPC: &str = "https://api.devnet.solana.com";
const SOCKET_PATH: &str = "/tmp/solbot-signer-devnet.sock";
const DB_PATH: &str = "sqlite:/tmp/solbot_devnet_test.db";

struct SignerGuard {
    child: Child,
}

impl Drop for SignerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(SOCKET_PATH);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    println!("🚀 Starting Devnet Round-Trip Test (Signer + Tx Builder + Devnet RPC)...");
    println!("=========================================================================");

    // 1. Prepare Keypair (persist to target/devnet_test_wallet.json to avoid rate limits)
    let key_path = std::path::Path::new("target/devnet_test_wallet.json");
    let keypair = if key_path.exists() {
        let content = std::fs::read_to_string(key_path)?;
        let bytes: Vec<u8> = serde_json::from_str(&content)?;
        Keypair::from_bytes(&bytes).map_err(|e| anyhow::anyhow!("Invalid saved keypair: {e}"))?
    } else {
        let kp = Keypair::new();
        if let Some(parent) = key_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let bytes = kp.to_bytes().to_vec();
        std::fs::write(key_path, serde_json::to_string(&bytes)?)?;
        kp
    };

    let wallet_pubkey = keypair.pubkey();
    let keypair_bytes = keypair.to_bytes().to_vec();
    let keypair_json = serde_json::to_string(&keypair_bytes)?;

    println!("🔑 Wallet pubkey: {}", wallet_pubkey);

    // 2. Connect to Devnet RPC & Airdrop SOL
    let rpc_client = Arc::new(RpcClient::new_with_commitment(
        DEVNET_RPC.to_string(),
        CommitmentConfig::confirmed(),
    ));

    let current_balance = rpc_client.get_balance(&wallet_pubkey).unwrap_or(0);
    if current_balance < 50_000_000 {
        println!("💰 Requesting Airdrop of 1 SOL on Devnet...");
        match rpc_client.request_airdrop(&wallet_pubkey, 1_000_000_000) {
            Ok(sig) => {
                println!("⏳ Airdrop tx submitted: {}", sig);
                for _ in 0..30 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    if let Ok(balance) = rpc_client.get_balance(&wallet_pubkey) {
                        if balance >= 500_000_000 {
                            println!(
                                "✅ Balance confirmed: {} lamports ({} SOL)",
                                balance,
                                balance as f64 / 1e9
                            );
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                println!(
                    "⚠️ Airdrop request returned: {}. Checking existing balance...",
                    e
                );
            }
        }
    } else {
        println!(
            "✅ Wallet has sufficient devnet balance: {} lamports ({} SOL)",
            current_balance,
            current_balance as f64 / 1e9
        );
    }

    let initial_balance = rpc_client.get_balance(&wallet_pubkey)?;
    if initial_balance == 0 {
        bail!(
            "Failed to obtain Devnet SOL via Airdrop. Cannot proceed with transaction execution."
        );
    }

    // 3. Spawn `solbot-signer` process
    println!("⚙️ Spawning `solbot-signer` background process...");
    let _ = std::fs::remove_file(SOCKET_PATH);

    // Get path to binary in target/debug/solbot-signer
    let binary_path = std::env::current_exe()?
        .parent()
        .unwrap()
        .join("solbot-signer");

    let child = Command::new(&binary_path)
        .env("SOLBOT_KEYPAIR_JSON", &keypair_json)
        .env("SOLBOT_SIGNER_SOCKET", SOCKET_PATH)
        .env("SOLBOT_DB_PATH", DB_PATH)
        .spawn()
        .with_context(|| format!("Failed to spawn signer process at {:?}", binary_path))?;

    let _signer_guard = SignerGuard { child };

    // Wait for socket creation
    println!(
        "⏳ Waiting for signer UDS socket creation at {}...",
        SOCKET_PATH
    );
    let mut socket_ready = false;
    for _ in 0..50 {
        if std::path::Path::new(SOCKET_PATH).exists() {
            socket_ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if !socket_ready {
        bail!("Signer failed to create UDS socket within 5 seconds");
    }
    println!("✅ Signer UDS socket ready!");

    // 4. Build transaction using Tx Builder
    println!("🛠 Building real WSOL Wrap instruction using Tx Builder...");
    let blockhash_cache = BlockhashCache::new(rpc_client.clone()).await?;
    let (recent_blockhash, _) = blockhash_cache.get_blockhash().await?;
    println!("✅ Recent blockhash: {}", recent_blockhash);

    let wrap_amount = 10_000_000; // 0.01 SOL
    let (wsol_ata, wrap_ixs) = ata::wrap_sol_instructions(&wallet_pubkey, wrap_amount)?;

    let swap_instructions = SwapInstructions {
        setup_instructions: wrap_ixs,
        swap_instructions: vec![],
        cleanup_instructions: vec![ata::close_wsol_account_instruction(&wallet_pubkey)?],
        additional_signers: vec![],
    };

    let (unsigned_tx, tx_base64) = build_unsigned_transaction(
        swap_instructions,
        200_000,
        1000,
        &wallet_pubkey,
        recent_blockhash,
    )?;

    println!(
        "✅ Unsigned transaction built successfully (base64 len: {})",
        tx_base64.len()
    );

    // 5. Connect via gRPC SignerClient and send SignIntent
    println!("📡 Sending SignIntent to `solbot-signer` via gRPC over UDS...");
    let client = SignerClient::new(SOCKET_PATH.to_string());

    let intent_req = SignerIntentRequest {
        context_key: format!("devnet_test_{}", wallet_pubkey),
        ipc_seq: 1,
        issued_at_unix_ms: chrono::Utc::now().timestamp_millis() as u64,
        intent_type: IntentType::Buy,
        unsigned_transaction_base64: tx_base64,
        program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(), // Raydium AMM v4 allowed
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        max_size_lamports: 1_000_000_000,
        hard_cap_bps: 500,
        destination_addresses: vec![],
        mint: ata::WSOL_MINT.to_string(),
        pool_address: wsol_ata.to_string(),
        position_id: Some("devnet_pos_1".to_string()),
        amount_or_pct: 10_000_000.0,
    };

    let response = client.sign_intent(intent_req).await?;

    if response.status != VerdictStatus::Accepted {
        bail!(
            "Signer REJECTED the intent: {:?}",
            response.rejection_reason
        );
    }

    let signed_b64 = response
        .signed_transaction_base64
        .context("Missing signed tx in response")?;
    let signature_str = response.constructed_tx_signature.unwrap_or_default();
    println!("✅ Signer ACCEPTED and signed transaction!");
    println!("  Signature: {}", signature_str);

    // 6. Submit signed transaction to Devnet RPC
    println!("🌐 Submitting signed transaction to Devnet RPC...");
    let signed_tx_bytes = STANDARD.decode(&signed_b64)?;
    let signed_tx: solana_transaction::Transaction = bincode::deserialize(&signed_tx_bytes)?;

    let confirmed_sig = rpc_client.send_and_confirm_transaction(&signed_tx)?;

    println!("🎉 Devnet Transaction CONFIRMED on-chain!");
    println!(
        "  Explorer URL: https://explorer.solana.com/tx/{}?cluster=devnet",
        confirmed_sig
    );

    println!("\n✅ DEVNET ROUND-TRIP TEST PASSED SUCCESSFULLY!");
    Ok(())
}
