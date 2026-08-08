//! Logs parser for Helius logsSubscribe WebSocket messages.
//!
//! Parses transaction logs from DEX programs to extract instruction-level signals
//! like InitializePool, Swap, and bonding curve migrations.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use solana_pubkey::Pubkey;

/// DEX type enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DexType {
    RaydiumAmmV4,
    RaydiumCpmm,
    RaydiumClmm,
    MeteoraDlmm,
    MeteoraDynamic,
    PumpFun,
}

impl DexType {
    pub fn from_pubkey(pubkey: &str) -> Option<Self> {
        match pubkey {
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8" => Some(DexType::RaydiumAmmV4),
            "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C" => Some(DexType::RaydiumCpmm),
            "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK" => Some(DexType::RaydiumClmm),
            "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo" => Some(DexType::MeteoraDlmm),
            "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG" => Some(DexType::MeteoraDynamic),
            "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P" => Some(DexType::PumpFun),
            _ => None,
        }
    }
}

/// Instruction type enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstructionType {
    InitializePool,
    Swap,
    AddLiquidity,
    RemoveLiquidity,
    InitBondingCurve,
    Migrate,
    Unknown,
}

/// Unified signal structure combining account data and instruction data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedSignal {
    pub timestamp: u64,
    pub slot: u64,
    pub dex: DexType,
    pub instruction_type: InstructionType,
    pub mint: Option<Pubkey>,
    pub pool_address: Pubkey,
    pub in_amount: Option<u64>,
    pub out_amount: Option<u64>,
    pub fee_amount: Option<u64>,
    pub signer: Option<Pubkey>,
    pub signature: Option<String>,
}

/// Helius logsSubscribe message structure
#[derive(Debug, Clone, Deserialize)]
pub struct LogsMessage {
    pub jsonrpc: String,
    #[serde(default)]
    pub method: Option<String>,
    pub params: Option<LogsParams>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogsParams {
    pub result: Option<LogsResult>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogsResult {
    pub context: LogsContext,
    pub value: LogsValue,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogsContext {
    pub slot: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogsValue {
    pub signature: String,
    pub err: Option<serde_json::Value>,
    pub logs: Vec<String>,
}

/// Parse logsSubscribe message and extract DEX instructions
pub fn parse_logs_message(text: &str) -> Result<Vec<ParsedSignal>> {
    let json: LogsMessage = serde_json::from_str(text)?;

    // Check if this is a logsNotification
    if json.method.as_deref() != Some("logsNotification") {
        return Ok(vec![]);
    }

    let params = json.params.ok_or_else(|| anyhow!("Missing params"))?;
    let result = params.result.ok_or_else(|| anyhow!("Missing result"))?;

    let slot = result.context.slot;
    let signature = result.value.signature.clone();
    let logs = result.value.logs;

    // Skip failed transactions
    if result.value.err.is_some() {
        return Ok(vec![]);
    }

    let mut signals = Vec::new();
    let mut current_dex: Option<DexType> = None;

    // Parse logs line by line
    for log in &logs {
        // Extract program ID from "Program <program_id> invoke" lines
        if let Some(program_id) = extract_program_id_from_invoke(log) {
            current_dex = DexType::from_pubkey(&program_id);
        }

        // Parse instruction logs
        if let Some(dex) = current_dex {
            if let Some(signal) = parse_instruction_log(log, dex, slot, &signature) {
                signals.push(signal);
            }
        }
    }

    Ok(signals)
}

/// Extract program ID from "Program <program_id> invoke" log line
fn extract_program_id_from_invoke(log: &str) -> Option<String> {
    if log.starts_with("Program ") && log.contains(" invoke") {
        let parts: Vec<&str> = log.split_whitespace().collect();
        if parts.len() >= 2 {
            return Some(parts[1].to_string());
        }
    }
    None
}

/// Parse instruction log line based on DEX type
fn parse_instruction_log(
    log: &str,
    dex: DexType,
    slot: u64,
    signature: &str,
) -> Option<ParsedSignal> {
    // Check for instruction logs
    if !log.starts_with("Program log: Instruction:") {
        return None;
    }

    // Extract instruction name
    let instruction_name = log.split(':').nth(2)?.trim();

    // Log unknown instruction names for debugging
    static UNKNOWN_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let instruction_type = match instruction_name {
        // Swap instructions (various names across DEXs)
        "Swap"
        | "Swap2"
        | "SwapWithPriceImpact2"
        | "SwapBaseInput"
        | "Buy"
        | "Sell"
        | "BuyExactQuoteInV2" => InstructionType::Swap,
        // Pool creation instructions
        "InitializePool" | "CreatePool" => InstructionType::InitializePool,
        "InitBondingCurve" => InstructionType::InitBondingCurve,
        "Migrate" => InstructionType::Migrate,
        "AddLiquidity" => InstructionType::AddLiquidity,
        "RemoveLiquidity" => InstructionType::RemoveLiquidity,
        _ => {
            let count = UNKNOWN_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if count < 20 {
                tracing::warn!(
                    "Unknown instruction: '{}' for DEX: {:?}",
                    instruction_name,
                    dex
                );
            }
            InstructionType::Unknown
        }
    };

    Some(ParsedSignal {
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs(),
        slot,
        dex,
        instruction_type,
        mint: None, // Will need to extract from account data or instruction data
        pool_address: Pubkey::default(),
        in_amount: None,  // Will need to extract from instruction data
        out_amount: None, // Will need to extract from instruction data
        fee_amount: None,
        signer: None,
        signature: Some(signature.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dex_type_from_pubkey() {
        assert_eq!(
            DexType::from_pubkey("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"),
            Some(DexType::RaydiumAmmV4)
        );
        assert_eq!(
            DexType::from_pubkey("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"),
            Some(DexType::PumpFun)
        );
        assert_eq!(DexType::from_pubkey("invalid"), None);
    }

    #[test]
    fn test_extract_program_id_from_invoke() {
        let log = "Program LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo invoke [2]";
        assert_eq!(
            extract_program_id_from_invoke(log),
            Some("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo".to_string())
        );
    }
}
