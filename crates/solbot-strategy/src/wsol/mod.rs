use anyhow::{anyhow, Result};
use solana_sdk::{instruction::Instruction, pubkey::Pubkey};
use spl_associated_token_account::get_associated_token_address;
use spl_token::instruction as spl_token_ix;
use std::str::FromStr;

/// Manages automatic WSOL (Wrapped SOL) unwrapping for position closes.
///
/// When a position closes, the proceeds come back as WSOL tokens.
/// This manager handles:
/// - Checking if WSOL balance exceeds threshold
/// - Building unwrap instruction (CloseAccount)
/// - Submitting unwrap transaction
pub struct WsolManager {
    unwrap_threshold_sol: f64,
}

impl WsolManager {
    pub fn new(unwrap_threshold_sol: f64) -> Self {
        Self {
            unwrap_threshold_sol,
        }
    }

    /// Check WSOL balance and create unwrap instruction if needed.
    pub async fn check_and_unwrap(
        &self,
        current_wsol_balance: f64,
        wallet_address: &str,
    ) -> Result<Option<Vec<Instruction>>> {
        if current_wsol_balance >= self.unwrap_threshold_sol {
            tracing::info!(
                balance = current_wsol_balance,
                threshold = self.unwrap_threshold_sol,
                "WSOL balance threshold exceeded, building auto-unwrap instructions"
            );

            let wallet = Pubkey::from_str(wallet_address)
                .map_err(|e| anyhow!("Invalid wallet address: {}", e))?;

            // WSOL token account (Wrapped SOL token = native SOL wrapped)
            // Standard WSOL mint is the native SOL (11111111111111111111111111111111)
            // But typically we interact with WSOL via the user's ATA for token mint
            // For unwrapping, we need to close the WSOL ATA and recover the SOL

            // Build CloseAccount instruction to unwrap and recover SOL
            let close_account_ix = spl_token_ix::close_account(
                &spl_token::id(),
                &self.derive_wsol_ata(&wallet)?,
                &wallet,
                &wallet,
                &[],
            )
            .map_err(|e| anyhow!("Failed to build close account instruction: {}", e))?;

            tracing::info!("Built WSOL unwrap instruction (CloseAccount)");
            return Ok(Some(vec![close_account_ix]));
        }

        Ok(None)
    }

    /// Derive the Associated Token Account (ATA) for WSOL.
    fn derive_wsol_ata(&self, wallet: &Pubkey) -> Result<Pubkey> {
        // WSOL Mint on Solana: So11111111111111111111111111111111111111112
        let wsol_mint = Pubkey::from_str("So11111111111111111111111111111111111111112")
            .map_err(|e| anyhow!("Failed to parse WSOL mint: {}", e))?;

        Ok(get_associated_token_address(wallet, &wsol_mint))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wsol_manager_creation() {
        let manager = WsolManager::new(0.1);
        assert_eq!(manager.unwrap_threshold_sol, 0.1);
    }

    #[test]
    fn test_wsol_ata_derivation() {
        let manager = WsolManager::new(0.1);
        let wallet = Pubkey::new_unique();

        let ata_result = manager.derive_wsol_ata(&wallet);
        assert!(ata_result.is_ok());

        let ata = ata_result.unwrap();
        // Verify ATA is deterministically derived from wallet
        let ata2 = manager.derive_wsol_ata(&wallet).unwrap();
        assert_eq!(ata, ata2);
    }
}
