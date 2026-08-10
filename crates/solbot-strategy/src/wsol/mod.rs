//! WSOL unwrap + empty ATA rent reclaim (TZ §4.7).
//!
//! Sell proceeds often land as WSOL. Above `unwrap_threshold_sol`, close the WSOL ATA
//! (unwrap to native SOL). Optionally close empty SPL token ATAs to reclaim rent.

use crate::tx_builder::ata::{self, WSOL_MINT};
use anyhow::{anyhow, Result};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use std::str::FromStr;

/// Default threshold from TZ tree comment (~0.1 SOL).
pub const DEFAULT_UNWRAP_THRESHOLD_SOL: f64 = 0.1;

pub struct WsolManager {
    unwrap_threshold_sol: f64,
}

impl Default for WsolManager {
    fn default() -> Self {
        Self::new(DEFAULT_UNWRAP_THRESHOLD_SOL)
    }
}

impl WsolManager {
    pub fn new(unwrap_threshold_sol: f64) -> Self {
        Self {
            unwrap_threshold_sol,
        }
    }

    pub fn threshold_sol(&self) -> f64 {
        self.unwrap_threshold_sol
    }

    pub fn derive_wsol_ata(wallet: &Pubkey) -> Pubkey {
        ata::derive_ata(wallet, &WSOL_MINT)
    }

    /// Build CloseAccount on WSOL ATA (unwrap full balance to wallet).
    pub fn build_unwrap_instruction(wallet: &Pubkey) -> Result<Instruction> {
        ata::close_wsol_account_instruction(wallet)
    }

    /// If balance ≥ threshold, return unwrap (+ optional empty token ATA closes).
    pub fn instructions_if_above_threshold(
        &self,
        current_wsol_balance_sol: f64,
        wallet: &Pubkey,
    ) -> Result<Option<Vec<Instruction>>> {
        if current_wsol_balance_sol + f64::EPSILON < self.unwrap_threshold_sol {
            return Ok(None);
        }
        tracing::info!(
            balance = current_wsol_balance_sol,
            threshold = self.unwrap_threshold_sol,
            "WSOL balance ≥ threshold — building unwrap CloseAccount"
        );
        Ok(Some(vec![Self::build_unwrap_instruction(wallet)?]))
    }

    /// Async helper: check balance string wallet + optional empty mint ATA reclaim.
    pub async fn check_and_unwrap(
        &self,
        current_wsol_balance: f64,
        wallet_address: &str,
    ) -> Result<Option<Vec<Instruction>>> {
        let wallet = Pubkey::from_str(wallet_address)
            .map_err(|e| anyhow!("Invalid wallet address: {e}"))?;
        self.instructions_if_above_threshold(current_wsol_balance, &wallet)
    }

    /// Fetch WSOL ATA ui amount via RPC (amount/1e9 for native decimals=9).
    pub async fn fetch_wsol_balance_sol(
        rpc: &RpcClient,
        wallet: &Pubkey,
    ) -> Result<f64> {
        let ata = Self::derive_wsol_ata(wallet);
        match rpc.get_token_account_balance(&ata).await {
            Ok(bal) => {
                let ui = bal
                    .ui_amount
                    .unwrap_or_else(|| bal.amount.parse::<f64>().unwrap_or(0.0) / 1e9);
                Ok(ui)
            }
            Err(_) => Ok(0.0), // missing ATA
        }
    }

    /// Close empty classic SPL token ATA to reclaim rent (not Token-2022).
    pub fn build_close_empty_token_ata(
        wallet: &Pubkey,
        token_mint: &Pubkey,
    ) -> Result<Instruction> {
        let token_ata = ata::derive_ata(wallet, token_mint);
        Ok(spl_token::instruction::close_account(
            &spl_token::id(),
            &token_ata,
            wallet,
            wallet,
            &[],
        )?)
    }

    /// After position close: unwrap WSOL if needed + close empty position token ATA.
    pub fn post_close_reclaim_instructions(
        &self,
        wallet: &Pubkey,
        wsol_balance_sol: f64,
        closed_token_mint: Option<&Pubkey>,
        token_ata_is_empty: bool,
    ) -> Result<Vec<Instruction>> {
        let mut out = Vec::new();
        if let Some(ixs) = self.instructions_if_above_threshold(wsol_balance_sol, wallet)? {
            out.extend(ixs);
        }
        if token_ata_is_empty {
            if let Some(mint) = closed_token_mint {
                out.push(Self::build_close_empty_token_ata(wallet, mint)?);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_threshold() {
        let m = WsolManager::new(0.1);
        assert!((m.threshold_sol() - 0.1).abs() < 1e-12);
    }

    #[test]
    fn ata_deterministic() {
        let w = Pubkey::new_unique();
        assert_eq!(WsolManager::derive_wsol_ata(&w), WsolManager::derive_wsol_ata(&w));
    }

    #[test]
    fn below_threshold_no_ix() {
        let m = WsolManager::new(0.1);
        let w = Pubkey::new_unique();
        let r = m.instructions_if_above_threshold(0.05, &w).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn above_threshold_close_ix() {
        let m = WsolManager::new(0.1);
        let w = Pubkey::new_unique();
        let r = m.instructions_if_above_threshold(0.5, &w).unwrap().unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].program_id, spl_token::id());
    }

    #[tokio::test]
    async fn check_and_unwrap_async() {
        let m = WsolManager::new(0.1);
        let w = Pubkey::new_unique();
        let ixs = m
            .check_and_unwrap(1.0, &w.to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ixs.len(), 1);
    }

    #[test]
    fn post_close_reclaim_both() {
        let m = WsolManager::new(0.05);
        let w = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ixs = m
            .post_close_reclaim_instructions(&w, 0.2, Some(&mint), true)
            .unwrap();
        assert_eq!(ixs.len(), 2);
    }
}
