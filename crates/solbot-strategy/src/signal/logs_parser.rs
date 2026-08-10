//! Logs parser for Helius logsSubscribe WebSocket messages.
//!
//! Parses transaction logs from DEX programs to extract **create-only** instruction
//! signals (InitializePool / InitBondingCurve). Swap/liquidity logs are classified
//! but must not become new-pool signals at the source layer.
//!
//! ## Exact log / discriminator matchers used
//!
//! | DEX | Program | Create log marker(s) | Notes |
//! |-----|---------|----------------------|-------|
//! | Pump.fun | `6EF8…F6P` | `Instruction: Create` / `CreateV2` / `create` | + `Program data:` borsh event |
//! | Raydium AMM v4 | `675k…Mp8` | **`Program log: ray_log: <b64>` with first byte `0` = `LogType::Init`** (raydium-amm `log.rs`) | No Anchor `Instruction:` line. Swap = `ray_log` type 3/4. Ix disc `1` = Initialize2 (verified upstream). |
//! | Raydium CPMM | `CPMM…P1C` | `Instruction: Initialize` | Anchor; accounts verified from `cp-swap` `Initialize` |
//! | Raydium CLMM | `CAMM…rqK` | `Instruction: CreatePool` | Anchor; accounts from `clmm` `CreatePool` |
//! | Meteora DLMM | `LBUZ…wxo` | `InitializeLbPair` / `InitializeCustomizablePermissionlessLbPair` | Anchor msg |
//! | Meteora DAMM v2 | `cpam…sGG` | `InitializePool` / `InitializePoolWithDynamicConfig` | Anchor; `CreateConfig` is **not** pool create |

use anyhow::{anyhow, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use solana_pubkey::Pubkey;
use std::str::FromStr;

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

    pub fn program_id(self) -> &'static str {
        match self {
            DexType::RaydiumAmmV4 => "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
            DexType::RaydiumCpmm => "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C",
            DexType::RaydiumClmm => "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK",
            DexType::MeteoraDlmm => "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
            DexType::MeteoraDynamic => "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG",
            DexType::PumpFun => "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
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

impl InstructionType {
    pub fn is_create_event(self) -> bool {
        matches!(
            self,
            InstructionType::InitializePool
                | InstructionType::InitBondingCurve
                | InstructionType::Migrate
        )
    }
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
    /// True when mint/pool were decoded from log `Program data:` (no getTransaction needed).
    pub resolved_from_log_data: bool,
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

    if json.method.as_deref() != Some("logsNotification") {
        return Ok(vec![]);
    }

    let params = json.params.ok_or_else(|| anyhow!("Missing params"))?;
    let result = params.result.ok_or_else(|| anyhow!("Missing result"))?;

    let slot = result.context.slot;
    let signature = result.value.signature.clone();
    let logs = &result.value.logs;

    if result.value.err.is_some() {
        return Ok(vec![]);
    }

    Ok(parse_log_lines(logs, slot, &signature))
}

/// Parse a raw log line list (unit-testable without full WS JSON envelope).
pub fn parse_log_lines(logs: &[String], slot: u64, signature: &str) -> Vec<ParsedSignal> {
    let mut signals = Vec::new();
    let mut current_dex: Option<DexType> = None;
    let mut pending_create: Option<(DexType, InstructionType)> = None;

    for log in logs {
        if let Some(program_id) = extract_program_id_from_invoke(log) {
            current_dex = DexType::from_pubkey(&program_id);
        }

        // Raydium AMM v4: native program — create is `ray_log` LogType::Init (byte 0),
        // not `Program log: Instruction: …`. This is the primary create detector for AMM v4.
        if matches!(current_dex, Some(DexType::RaydiumAmmV4)) {
            if let Some(ray_ty) = classify_ray_log(log) {
                match ray_ty {
                    RayLogKind::Init => {
                        signals.push(base_signal(
                            DexType::RaydiumAmmV4,
                            InstructionType::InitializePool,
                            slot,
                            signature,
                            None,
                            None,
                            false,
                        ));
                        pending_create = Some((DexType::RaydiumAmmV4, InstructionType::InitializePool));
                    }
                    RayLogKind::Swap | RayLogKind::Other => {
                        signals.push(base_signal(
                            DexType::RaydiumAmmV4,
                            if matches!(ray_ty, RayLogKind::Swap) {
                                InstructionType::Swap
                            } else {
                                InstructionType::Unknown
                            },
                            slot,
                            signature,
                            None,
                            None,
                            false,
                        ));
                    }
                }
                continue;
            }
        }

        if let Some(dex) = current_dex {
            if let Some(ix_type) = classify_instruction_log(log) {
                if ix_type.is_create_event() {
                    pending_create = Some((dex, ix_type));
                    signals.push(base_signal(dex, ix_type, slot, signature, None, None, false));
                } else if matches!(
                    ix_type,
                    InstructionType::Swap
                        | InstructionType::AddLiquidity
                        | InstructionType::RemoveLiquidity
                ) {
                    signals.push(base_signal(dex, ix_type, slot, signature, None, None, false));
                    pending_create = None;
                }
            }
        }

        // Pump.fun Create emits `Program data:` right after Instruction: Create.
        if let Some((DexType::PumpFun, ix_type)) = pending_create {
            if let Some((mint, curve, user)) = try_parse_pump_create_program_data(log) {
                if let Some(last) = signals.last_mut() {
                    if last.dex == DexType::PumpFun && last.instruction_type.is_create_event() {
                        last.mint = Some(mint);
                        last.pool_address = curve;
                        last.signer = Some(user);
                        last.resolved_from_log_data = true;
                        let _ = ix_type;
                    }
                }
                pending_create = None;
            }
        }
    }

    signals
}

/// Raydium AMM `ray_log` payload kinds (first byte of bincode blob).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RayLogKind {
    Init,
    Swap,
    Other,
}

/// Detect Raydium AMM v4 `Program log: ray_log: <base64>`.
/// Source: raydium-amm `program/src/log.rs` — LogType::Init=0, Deposit=1, Withdraw=2,
/// SwapBaseIn=3, SwapBaseOut=4.
fn classify_ray_log(log: &str) -> Option<RayLogKind> {
    let b64 = log.strip_prefix("Program log: ray_log: ")?.trim();
    if b64.is_empty() {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let tag = *bytes.first()?;
    Some(match tag {
        0 => RayLogKind::Init,
        3 | 4 => RayLogKind::Swap,
        _ => RayLogKind::Other,
    })
}

fn base_signal(
    dex: DexType,
    instruction_type: InstructionType,
    slot: u64,
    signature: &str,
    mint: Option<Pubkey>,
    pool: Option<Pubkey>,
    resolved: bool,
) -> ParsedSignal {
    ParsedSignal {
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        slot,
        dex,
        instruction_type,
        mint,
        pool_address: pool.unwrap_or_default(),
        in_amount: None,
        out_amount: None,
        fee_amount: None,
        signer: None,
        signature: Some(signature.to_string()),
        resolved_from_log_data: resolved,
    }
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

/// Map `Program log: Instruction: <Name>` → InstructionType.
fn classify_instruction_log(log: &str) -> Option<InstructionType> {
    let name = if let Some(rest) = log.strip_prefix("Program log: Instruction:") {
        rest.trim()
    } else if let Some(rest) = log.strip_prefix("Program log: instruction:") {
        rest.trim()
    } else {
        return None;
    };

    Some(match name {
        "Swap"
        | "Swap2"
        | "SwapWithPriceImpact2"
        | "SwapBaseInput"
        | "SwapBaseOutput"
        | "Buy"
        | "Sell"
        | "BuyExactQuoteInV2" => InstructionType::Swap,

        // Pool / curve creation — exact names observed on mainnet Anchor programs.
        "InitializePool"
        | "CreatePool"
        | "Initialize"
        | "InitializeCustomizablePermissionlessLbPair"
        | "InitializeLbPair"
        | "InitializePermissionLbPair"
        | "Create"
        | "CreateV2"
        | "initialize2"
        | "Initialize2" => InstructionType::InitializePool,

        "InitBondingCurve" | "create" => InstructionType::InitBondingCurve,

        "Migrate" | "MigratePool" | "migrate" => InstructionType::Migrate,

        "AddLiquidity" | "AddLiquidityByWeight" | "AddLiquidityOneSide" => {
            InstructionType::AddLiquidity
        }
        "RemoveLiquidity" | "RemoveLiquidityByRange" => InstructionType::RemoveLiquidity,

        // Config-only — not a pool create.
        "CreateConfig" | "CreateConfig2" | "InitializeConfig" => InstructionType::Unknown,

        _ => InstructionType::Unknown,
    })
}

/// Parse pump.fun Create `Program data:` payload.
///
/// Layout (Anchor event body, with or without 8-byte discriminator prefix):
/// `name: String, symbol: String, uri: String, mint: [u8;32], bonding_curve: [u8;32], user: [u8;32]`
pub fn try_parse_pump_create_program_data(log: &str) -> Option<(Pubkey, Pubkey, Pubkey)> {
    let b64 = log.strip_prefix("Program data: ")?.trim();
    if b64.is_empty() {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    parse_pump_create_event_bytes(&bytes)
}

fn parse_pump_create_event_bytes(bytes: &[u8]) -> Option<(Pubkey, Pubkey, Pubkey)> {
    // Try with 8-byte Anchor event discriminator, then raw body.
    if bytes.len() > 8 {
        if let Some(v) = parse_pump_create_event_body(&bytes[8..]) {
            return Some(v);
        }
    }
    parse_pump_create_event_body(bytes)
}

fn parse_pump_create_event_body(mut data: &[u8]) -> Option<(Pubkey, Pubkey, Pubkey)> {
    // name, symbol, uri
    for _ in 0..3 {
        data = skip_borsh_string(data)?;
    }
    if data.len() < 96 {
        return None;
    }
    let mint = Pubkey::try_from(&data[0..32]).ok()?;
    let curve = Pubkey::try_from(&data[32..64]).ok()?;
    let user = Pubkey::try_from(&data[64..96]).ok()?;
    Some((mint, curve, user))
}

fn skip_borsh_string(data: &[u8]) -> Option<&[u8]> {
    if data.len() < 4 {
        return None;
    }
    let len = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
    if data.len() < 4 + len {
        return None;
    }
    Some(&data[4 + len..])
}

/// Build a logsNotification JSON fixture for tests.
pub fn make_logs_notification_json(slot: u64, signature: &str, logs: &[&str]) -> String {
    let logs_json: Vec<String> = logs.iter().map(|l| format!("\"{}\"", l.replace('\"', "\\\""))).collect();
    format!(
        r#"{{"jsonrpc":"2.0","method":"logsNotification","params":{{"result":{{"context":{{"slot":{slot}}},"value":{{"signature":"{signature}","err":null,"logs":[{logs}]}}}},"subscription":1}}}}"#,
        logs = logs_json.join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUMP: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

    fn encode_pump_create_data(mint: &Pubkey, curve: &Pubkey, user: &Pubkey) -> String {
        let mut body = Vec::new();
        for s in ["TestCoin", "TST", "https://example.com/meta.json"] {
            let b = s.as_bytes();
            body.extend_from_slice(&(b.len() as u32).to_le_bytes());
            body.extend_from_slice(b);
        }
        body.extend_from_slice(mint.as_ref());
        body.extend_from_slice(curve.as_ref());
        body.extend_from_slice(user.as_ref());
        // Prepend fake 8-byte event disc
        let mut full = vec![0u8; 8];
        full.extend_from_slice(&body);
        base64::engine::general_purpose::STANDARD.encode(full)
    }

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

    #[test]
    fn pump_create_log_emits_create_signal_with_mint_curve() {
        let mint = Pubkey::new_unique();
        let curve = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let data_b64 = encode_pump_create_data(&mint, &curve, &user);

        let logs = [
            format!("Program {PUMP} invoke [1]"),
            "Program log: Instruction: Create".into(),
            format!("Program data: {data_b64}"),
            format!("Program {PUMP} success"),
        ];
        let sigs = parse_log_lines(&logs, 12345, "5".repeat(64).as_str());
        let creates: Vec<_> = sigs
            .iter()
            .filter(|s| s.instruction_type.is_create_event())
            .collect();
        assert_eq!(creates.len(), 1, "exactly one create signal");
        assert_eq!(creates[0].dex, DexType::PumpFun);
        assert_eq!(creates[0].mint, Some(mint));
        assert_eq!(creates[0].pool_address, curve);
        assert!(creates[0].resolved_from_log_data);
    }

    #[test]
    fn pump_swap_log_must_not_be_create() {
        let logs = [
            format!("Program {PUMP} invoke [1]"),
            "Program log: Instruction: Buy".into(),
            format!("Program {PUMP} success"),
        ];
        let sigs = parse_log_lines(&logs, 1, "sigBuy");
        assert!(
            sigs.iter().all(|s| !s.instruction_type.is_create_event()),
            "Buy must not produce create signal: {:?}",
            sigs
        );
        assert!(sigs.iter().any(|s| s.instruction_type == InstructionType::Swap));
    }

    #[test]
    fn pump_sell_log_must_not_be_create() {
        let logs = [
            format!("Program {PUMP} invoke [1]"),
            "Program log: Instruction: Sell".into(),
            format!("Program {PUMP} success"),
        ];
        let sigs = parse_log_lines(&logs, 1, "sigSell");
        assert!(sigs.iter().all(|s| !s.instruction_type.is_create_event()));
    }

    #[test]
    fn full_ws_json_create_parses() {
        let mint = Pubkey::new_unique();
        let curve = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let data_b64 = encode_pump_create_data(&mint, &curve, &user);
        let json = make_logs_notification_json(
            99,
            "TestSig111",
            &[
                &format!("Program {PUMP} invoke [1]"),
                "Program log: Instruction: Create",
                &format!("Program data: {data_b64}"),
            ],
        );
        let sigs = parse_logs_message(&json).unwrap();
        assert_eq!(sigs.len(), 1);
        assert!(sigs[0].instruction_type.is_create_event());
        assert_eq!(sigs[0].mint, Some(mint));
        assert_eq!(sigs[0].pool_address, curve);
    }

    #[test]
    fn full_ws_json_swap_not_create_for_pipeline() {
        let json = make_logs_notification_json(
            99,
            "TestSigSwap",
            &[
                &format!("Program {PUMP} invoke [1]"),
                "Program log: Instruction: Buy",
            ],
        );
        let sigs = parse_logs_message(&json).unwrap();
        assert!(sigs.iter().all(|s| !s.instruction_type.is_create_event()));
    }

    #[test]
    fn meteora_initialize_lb_pair_is_create() {
        let pid = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
        let logs = [
            format!("Program {pid} invoke [1]"),
            "Program log: Instruction: InitializeLbPair".into(),
        ];
        let sigs = parse_log_lines(&logs, 1, "m1");
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].instruction_type, InstructionType::InitializePool);
        assert_eq!(sigs[0].dex, DexType::MeteoraDlmm);
    }

    #[test]
    fn create_config_is_not_pool_create() {
        let pid = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG";
        let logs = [
            format!("Program {pid} invoke [1]"),
            "Program log: Instruction: CreateConfig".into(),
        ];
        let sigs = parse_log_lines(&logs, 1, "c1");
        assert!(sigs.iter().all(|s| !s.instruction_type.is_create_event()));
    }

    #[test]
    fn raydium_amm_v4_ray_log_init_is_create() {
        // LogType::Init = 0 as first byte of bincode payload.
        let payload = base64::engine::general_purpose::STANDARD.encode([0u8, 1, 2, 3, 4, 5, 6, 7]);
        let pid = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
        let logs = [
            format!("Program {pid} invoke [1]"),
            format!("Program log: ray_log: {payload}"),
            format!("Program {pid} success"),
        ];
        let sigs = parse_log_lines(&logs, 99, "rayInit");
        let creates: Vec<_> = sigs
            .iter()
            .filter(|s| s.instruction_type.is_create_event())
            .collect();
        assert_eq!(creates.len(), 1);
        assert_eq!(creates[0].dex, DexType::RaydiumAmmV4);
    }

    #[test]
    fn raydium_amm_v4_ray_log_swap_is_not_create() {
        // LogType::SwapBaseIn = 3
        let payload = base64::engine::general_purpose::STANDARD.encode([3u8, 0, 0, 0]);
        let pid = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
        let logs = [
            format!("Program {pid} invoke [1]"),
            format!("Program log: ray_log: {payload}"),
        ];
        let sigs = parse_log_lines(&logs, 1, "raySwap");
        assert!(
            sigs.iter().all(|s| !s.instruction_type.is_create_event()),
            "swap ray_log must not create: {:?}",
            sigs
        );
        assert!(sigs.iter().any(|s| s.instruction_type == InstructionType::Swap));
    }

    #[test]
    fn raydium_amm_v4_no_instruction_line_still_detects_via_ray_log() {
        // Prove we do NOT require "Instruction: initialize2"
        let payload = base64::engine::general_purpose::STANDARD.encode([0u8]);
        let pid = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
        let logs = [
            format!("Program {pid} invoke [1]"),
            // deliberately no Instruction: line
            format!("Program log: ray_log: {payload}"),
        ];
        assert!(
            parse_log_lines(&logs, 1, "x")
                .iter()
                .any(|s| s.instruction_type.is_create_event())
        );
    }
}
