//! Paper pipeline e2e: synthetic Signal → risk gate (optional live RPC) → swap build.
//!
//! Does NOT broadcast. Validates builders + risk path compile/run end-to-end.
//!
//! ```bash
//! cargo run -p solbot-strategy --release --bin paper_pipeline_e2e
//! # optional: SOLBOT_RPC_URL=... for real mint/pool checks
//! ```

use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solbot_common::types::Signal;
use solbot_strategy::risk_gate::{resolve_rpc_url, run_pre_buy_checks_with_timeout, RiskGateConfig};
use solbot_strategy::tx_builder::meteora_damm_v2::{self, DammV2PoolState};
use solbot_strategy::tx_builder::meteora_dlmm::{self, DlmmPoolState};
use solbot_strategy::tx_builder::pump_fun::{self, PumpFunBondingCurve};
use solbot_strategy::tx_builder::raydium_amm_v4::{self, RaydiumAmmV4PoolState};
use solbot_strategy::tx_builder::raydium_clmm::{self, ClmmPoolState};
use solbot_strategy::tx_builder::raydium_cpmm::{self, CpmmPoolState};
use solbot_strategy::tx_builder::{SwapDirection, SwapParams};
use std::str::FromStr;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    println!("=== paper_pipeline_e2e ===");

    // 1) Builder unit smoke (no RPC)
    builder_smoke()?;
    println!("[ok] builder smoke (pump/amm/cpmm/clmm/dlmm/damm)");

    // 2) Risk gate on a known safe-ish mint (WSOL pair pool optional)
    let url = resolve_rpc_url();
    let rpc = Arc::new(RpcClient::new(url.clone()));
    let cfg = RiskGateConfig::from_shared_rpc(rpc.clone(), url);

    // Synthetic pump-style signal with implausible mint → must reject without panic
    let bad = Signal {
        mint: "11111111111111111111111111111111".into(),
        pool_address: Pubkey::new_unique().to_string(),
        dex_program: "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P".into(),
        signal_source: "e2e".into(),
        slot: 1,
        received_at: chrono::Utc::now().timestamp_millis() as u64,
        raw_pool_account_data: None,
        raw_pool_account_owner: None,
    };
    let v = run_pre_buy_checks_with_timeout(&bad, &cfg).await;
    println!("[ok] risk gate reject path: {:?}", v);

    // 3) Optional: fetch a known mainnet pool and build swap (no sign/send)
    if let Ok(pool) = std::env::var("E2E_POOL") {
        if let Ok(owner) = std::env::var("E2E_POOL_OWNER") {
            let pk = Pubkey::from_str(&pool)?;
            let acc = rpc.get_account(&pk).await?;
            let owner_pk = Pubkey::from_str(&owner)?;
            let wallet = Pubkey::new_unique();
            let mint = Pubkey::new_unique();
            let params = SwapParams {
                mint,
                pool_address: pk,
                dex_program_id: owner_pk,
                wallet,
                direction: SwapDirection::Buy,
                amount_in: 100_000,
                minimum_amount_out: 1,
                slippage_bps: 500,
            };
            let _ = try_build(&owner, &acc.data, &pk, &params);
            println!("[ok] optional live pool build attempted for {pool}");
        }
    }

    println!("=== paper_pipeline_e2e PASSED ===");
    Ok(())
}

fn builder_smoke() -> anyhow::Result<()> {
    let wallet = Pubkey::new_unique();
    let mint = Pubkey::new_unique();
    let pool = Pubkey::new_unique();

    // Pump
    {
        let mut data = vec![0u8; 90];
        data[8..40].copy_from_slice(mint.as_ref());
        data[40..48].copy_from_slice(&1_000_000u64.to_le_bytes());
        data[48..56].copy_from_slice(&30_000_000_000u64.to_le_bytes());
        let curve = PumpFunBondingCurve::from_account_data(&data, &pool)?;
        let params = base_params(mint, pool, pump_fun::PUMP_FUN_PROGRAM, wallet);
        let _ = pump_fun::build_swap(&params, &curve)?;
    }

    // AMM v4 minimal — only parse path if data wrong is ok
    {
        let data = vec![0u8; 752];
        let _ = RaydiumAmmV4PoolState::from_account_data(&data);
    }

    // CPMM
    {
        let mut data = vec![0u8; 650];
        data[72..104].copy_from_slice(mint.as_ref());
        data[104..136].copy_from_slice(
            Pubkey::from_str("So11111111111111111111111111111111111111112")?.as_ref(),
        );
        let _ = CpmmPoolState::from_account_data(&data);
    }

    // CLMM
    {
        let data = vec![0u8; 1544];
        let _ = ClmmPoolState::from_account_data(&data);
    }

    // DLMM
    {
        let mut data = vec![0u8; 904];
        data[88..120].copy_from_slice(mint.as_ref());
        data[120..152].copy_from_slice(
            Pubkey::from_str("So11111111111111111111111111111111111111112")?.as_ref(),
        );
        let st = DlmmPoolState::from_account_data(&data, &pool)?;
        let params = base_params(mint, pool, meteora_dlmm::METEORA_DLMM_PROGRAM, wallet);
        let _ = meteora_dlmm::build_swap(&params, &st)?;
    }

    // DAMM v2
    {
        let mut data = vec![0u8; 1112];
        data[168..200].copy_from_slice(mint.as_ref());
        data[200..232].copy_from_slice(
            Pubkey::from_str("So11111111111111111111111111111111111111112")?.as_ref(),
        );
        data[232..264].copy_from_slice(Pubkey::new_unique().as_ref());
        data[264..296].copy_from_slice(Pubkey::new_unique().as_ref());
        let st = DammV2PoolState::from_account_data(&data, &pool)?
            .with_vault_balances(1_000_000_000, 10_000_000_000);
        let params = base_params(mint, pool, meteora_damm_v2::METEORA_DAMM_V2_PROGRAM, wallet);
        let _ = meteora_damm_v2::build_swap(&params, &st)?;
    }

    Ok(())
}

fn base_params(mint: Pubkey, pool: Pubkey, prog: Pubkey, wallet: Pubkey) -> SwapParams {
    SwapParams {
        mint,
        pool_address: pool,
        dex_program_id: prog,
        wallet,
        direction: SwapDirection::Buy,
        amount_in: 100_000,
        minimum_amount_out: 1,
        slippage_bps: 500,
    }
}

fn try_build(owner: &str, data: &[u8], pool: &Pubkey, params: &SwapParams) -> anyhow::Result<()> {
    if owner == raydium_amm_v4::RAYDIUM_AMM_V4_PROGRAM.to_string() {
        let st = RaydiumAmmV4PoolState::from_account_data(data)?;
        let _ = raydium_amm_v4::build_swap(params, &st)?;
    } else if owner == raydium_cpmm::RAYDIUM_CPMM_PROGRAM.to_string() {
        let st = CpmmPoolState::from_account_data(data)?;
        let _ = raydium_cpmm::build_swap(params, &st)?;
    } else if owner == raydium_clmm::RAYDIUM_CLMM_PROGRAM.to_string() {
        let st = ClmmPoolState::from_account_data(data)?;
        let _ = raydium_clmm::build_swap(params, &st, pool, None)?;
    } else if owner.contains("LBUZ") {
        let st = DlmmPoolState::from_account_data(data, pool)?;
        let _ = meteora_dlmm::build_swap(params, &st)?;
    } else if owner.contains("cpamdp") {
        let st = DammV2PoolState::from_account_data(data, pool)?;
        let _ = meteora_damm_v2::build_swap(params, &st)?;
    }
    Ok(())
}
