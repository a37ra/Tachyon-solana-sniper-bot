//! Instruction parser for DEX transactions.
//!
//! Parses InitializePool and Swap instructions from transaction data
//! for Raydium, Meteora, and Pump.fun protocols.

use anyhow::Result;
use solana_pubkey::Pubkey;
use solana_sdk::transaction::Transaction;

/// Parsed DEX instruction data.
#[derive(Debug, Clone)]
pub enum ParsedInstruction {
    /// InitializePool instruction
    InitializePool {
        mint: String,
        pool_address: String,
        dex_program: String,
    },
    /// Swap instruction
    Swap {
        mint: String,
        pool_address: String,
        dex_program: String,
    },
}

/// Raydium AMM v4 instruction discriminator.
const RAYDIUM_AMM_V4_INITIALIZE: u8 = 9;
const RAYDIUM_AMM_V4_SWAP: u8 = 9;

/// Raydium CP-MM instruction discriminator.
const RAYDIUM_CPMM_INITIALIZE: u8 = 0;
const RAYDIUM_CPMM_SWAP: u8 = 1;

/// Raydium CLMM instruction discriminator.
const RAYDIUM_CLMM_INITIALIZE: u8 = 0;
const RAYDIUM_CLMM_SWAP: u8 = 1;

/// Meteora DLMM instruction discriminator.
const METEORA_DLMM_INITIALIZE: u8 = 0;
const METEORA_DLMM_SWAP: u8 = 1;

/// Meteora Dynamic AMM instruction discriminator.
const METEORA_DYNAMIC_INITIALIZE: u8 = 0;
const METEORA_DYNAMIC_SWAP: u8 = 1;

/// Pump.fun instruction discriminator.
const PUMP_FUN_CREATE: u8 = 0;
const PUMP_FUN_BUY: u8 = 1;
const PUMP_FUN_SELL: u8 = 2;

/// Parse transaction instructions for DEX operations.
pub fn parse_transaction_instructions(
    tx: &Transaction,
    program_ids: &[Pubkey],
) -> Vec<ParsedInstruction> {
    let mut parsed = Vec::new();

    for instruction in &tx.message.instructions {
        let program_index = instruction.program_id_index as usize;
        if program_index >= tx.message.account_keys.len() {
            continue;
        }

        let program_id = &tx.message.account_keys[program_index];
        
        if !program_ids.contains(program_id) {
            continue;
        }

        let data = &instruction.data;
        
        // Try to parse based on program ID
        if let Ok(parsed_inst) = parse_instruction_by_program(program_id, data, &tx.message.account_keys, &instruction.accounts) {
            parsed.push(parsed_inst);
        }
    }

    parsed
}

/// Parse instruction based on program ID.
fn parse_instruction_by_program(
    program_id: &Pubkey,
    data: &[u8],
    account_keys: &[Pubkey],
    account_indices: &[u8],
) -> Result<ParsedInstruction> {
    let program_str = program_id.to_string();

    // Raydium AMM v4
    if program_str == "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8" {
        return parse_raydium_amm_v4(data, account_keys, account_indices, program_str);
    }

    // Raydium CP-MM
    if program_str == "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C" {
        return parse_raydium_cpmm(data, account_keys, account_indices, program_str);
    }

    // Raydium CLMM
    if program_str == "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK" {
        return parse_raydium_clmm(data, account_keys, account_indices, program_str);
    }

    // Meteora DLMM (mainnet v2 + legacy)
    if program_str == "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo"
        || program_str == "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp"
    {
        return parse_meteora_dlmm(data, account_keys, account_indices, program_str);
    }

    // Meteora DAMM v2 + legacy Dynamic AMM
    if program_str == "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG"
        || program_str == "Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB"
        || program_str == "Eo7WjKq67rjJQSZxS6z3YKapzY3eMj6Xy8X5EQVn5UaB"
    {
        return parse_meteora_dynamic(data, account_keys, account_indices, program_str);
    }

    // Pump.fun
    if program_str == "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P" {
        return parse_pump_fun(data, account_keys, account_indices, program_str);
    }

    anyhow::bail!("Unknown program ID: {}", program_str)
}

/// Parse Raydium AMM v4 instruction.
fn parse_raydium_amm_v4(
    data: &[u8],
    account_keys: &[Pubkey],
    account_indices: &[u8],
    program_str: String,
) -> Result<ParsedInstruction> {
    if data.is_empty() {
        anyhow::bail!("Empty instruction data");
    }

    let discriminator = data[0];

    // InitializePool (discriminator 9)
    if discriminator == RAYDIUM_AMM_V4_INITIALIZE && data.len() >= 8 {
        // Account layout: 
        // 0: program_id
        // 1: amm_id (pool)
        // 2: amm_authority
        // 3: amm_open_orders
        // 4: amm_target_orders
        // 5: lp_mint
        // 6: coin_mint (token A)
        // 7: pc_mint (token B/SOL)
        if account_indices.len() > 7 {
            let pool_address = account_keys[account_indices[1] as usize].to_string();
            let mint = account_keys[account_indices[6] as usize].to_string();
            
            return Ok(ParsedInstruction::InitializePool {
                mint,
                pool_address,
                dex_program: program_str,
            });
        }
    }

    // Swap (discriminator 9 - same as initialize, need to check account count)
    if discriminator == RAYDIUM_AMM_V4_SWAP && account_indices.len() >= 10 {
        // Swap has more accounts than initialize
        let pool_address = account_keys[account_indices[1] as usize].to_string();
        let mint = account_keys[account_indices[6] as usize].to_string();
        
        return Ok(ParsedInstruction::Swap {
            mint,
            pool_address,
            dex_program: program_str,
        });
    }

    anyhow::bail!("Unknown Raydium AMM v4 instruction")
}

/// Parse Raydium CP-MM instruction.
fn parse_raydium_cpmm(
    data: &[u8],
    account_keys: &[Pubkey],
    account_indices: &[u8],
    program_str: String,
) -> Result<ParsedInstruction> {
    if data.is_empty() {
        anyhow::bail!("Empty instruction data");
    }

    let discriminator = data[0];

    // InitializePool (discriminator 0)
    if discriminator == RAYDIUM_CPMM_INITIALIZE && account_indices.len() > 5 {
        let pool_address = account_keys[account_indices[1] as usize].to_string();
        let mint = account_keys[account_indices[3] as usize].to_string();
        
        return Ok(ParsedInstruction::InitializePool {
            mint,
            pool_address,
            dex_program: program_str,
        });
    }

    // Swap (discriminator 1)
    if discriminator == RAYDIUM_CPMM_SWAP && account_indices.len() > 8 {
        let pool_address = account_keys[account_indices[1] as usize].to_string();
        let mint = account_keys[account_indices[3] as usize].to_string();
        
        return Ok(ParsedInstruction::Swap {
            mint,
            pool_address,
            dex_program: program_str,
        });
    }

    anyhow::bail!("Unknown Raydium CP-MM instruction")
}

/// Parse Raydium CLMM instruction.
fn parse_raydium_clmm(
    data: &[u8],
    account_keys: &[Pubkey],
    account_indices: &[u8],
    program_str: String,
) -> Result<ParsedInstruction> {
    if data.is_empty() {
        anyhow::bail!("Empty instruction data");
    }

    let discriminator = data[0];

    // InitializePool (discriminator 0)
    if discriminator == RAYDIUM_CLMM_INITIALIZE && account_indices.len() > 8 {
        let pool_address = account_keys[account_indices[1] as usize].to_string();
        let mint = account_keys[account_indices[4] as usize].to_string();
        
        return Ok(ParsedInstruction::InitializePool {
            mint,
            pool_address,
            dex_program: program_str,
        });
    }

    // Swap (discriminator 1)
    if discriminator == RAYDIUM_CLMM_SWAP && account_indices.len() > 12 {
        let pool_address = account_keys[account_indices[1] as usize].to_string();
        let mint = account_keys[account_indices[4] as usize].to_string();
        
        return Ok(ParsedInstruction::Swap {
            mint,
            pool_address,
            dex_program: program_str,
        });
    }

    anyhow::bail!("Unknown Raydium CLMM instruction")
}

/// Parse Meteora DLMM instruction.
fn parse_meteora_dlmm(
    data: &[u8],
    account_keys: &[Pubkey],
    account_indices: &[u8],
    program_str: String,
) -> Result<ParsedInstruction> {
    if data.is_empty() {
        anyhow::bail!("Empty instruction data");
    }

    let discriminator = data[0];

    // InitializePool (discriminator 0)
    if discriminator == METEORA_DLMM_INITIALIZE && account_indices.len() > 6 {
        let pool_address = account_keys[account_indices[1] as usize].to_string();
        let mint = account_keys[account_indices[3] as usize].to_string();
        
        return Ok(ParsedInstruction::InitializePool {
            mint,
            pool_address,
            dex_program: program_str,
        });
    }

    // Swap (discriminator 1)
    if discriminator == METEORA_DLMM_SWAP && account_indices.len() > 10 {
        let pool_address = account_keys[account_indices[1] as usize].to_string();
        let mint = account_keys[account_indices[3] as usize].to_string();
        
        return Ok(ParsedInstruction::Swap {
            mint,
            pool_address,
            dex_program: program_str,
        });
    }

    anyhow::bail!("Unknown Meteora DLMM instruction")
}

/// Parse Meteora Dynamic AMM instruction.
fn parse_meteora_dynamic(
    data: &[u8],
    account_keys: &[Pubkey],
    account_indices: &[u8],
    program_str: String,
) -> Result<ParsedInstruction> {
    if data.is_empty() {
        anyhow::bail!("Empty instruction data");
    }

    let discriminator = data[0];

    // InitializePool (discriminator 0)
    if discriminator == METEORA_DYNAMIC_INITIALIZE && account_indices.len() > 5 {
        let pool_address = account_keys[account_indices[1] as usize].to_string();
        let mint = account_keys[account_indices[2] as usize].to_string();
        
        return Ok(ParsedInstruction::InitializePool {
            mint,
            pool_address,
            dex_program: program_str,
        });
    }

    // Swap (discriminator 1)
    if discriminator == METEORA_DYNAMIC_SWAP && account_indices.len() > 8 {
        let pool_address = account_keys[account_indices[1] as usize].to_string();
        let mint = account_keys[account_indices[2] as usize].to_string();
        
        return Ok(ParsedInstruction::Swap {
            mint,
            pool_address,
            dex_program: program_str,
        });
    }

    anyhow::bail!("Unknown Meteora Dynamic AMM instruction")
}

/// Parse Pump.fun instruction.
fn parse_pump_fun(
    data: &[u8],
    account_keys: &[Pubkey],
    account_indices: &[u8],
    program_str: String,
) -> Result<ParsedInstruction> {
    if data.is_empty() {
        anyhow::bail!("Empty instruction data");
    }

    let discriminator = data[0];

    // Create bonding curve (discriminator 0)
    if discriminator == PUMP_FUN_CREATE && account_indices.len() > 3 {
        let pool_address = account_keys[account_indices[2] as usize].to_string();
        let mint = account_keys[account_indices[1] as usize].to_string();
        
        return Ok(ParsedInstruction::InitializePool {
            mint,
            pool_address,
            dex_program: program_str,
        });
    }

    // Buy (discriminator 1)
    if discriminator == PUMP_FUN_BUY && account_indices.len() > 4 {
        let pool_address = account_keys[account_indices[2] as usize].to_string();
        let mint = account_keys[account_indices[1] as usize].to_string();
        
        return Ok(ParsedInstruction::Swap {
            mint,
            pool_address,
            dex_program: program_str,
        });
    }

    // Sell (discriminator 2)
    if discriminator == PUMP_FUN_SELL && account_indices.len() > 4 {
        let pool_address = account_keys[account_indices[2] as usize].to_string();
        let mint = account_keys[account_indices[1] as usize].to_string();
        
        return Ok(ParsedInstruction::Swap {
            mint,
            pool_address,
            dex_program: program_str,
        });
    }

    anyhow::bail!("Unknown Pump.fun instruction")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_parse_raydium_amm_v4_program_id() {
        let program_id = Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8").unwrap();
        assert_eq!(
            program_id.to_string(),
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"
        );
    }

    #[test]
    fn test_parse_pump_fun_program_id() {
        let program_id = Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P").unwrap();
        assert_eq!(
            program_id.to_string(),
            "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
        );
    }
}
