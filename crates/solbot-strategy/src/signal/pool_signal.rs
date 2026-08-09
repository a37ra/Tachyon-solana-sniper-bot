//! Strict "new pool / new bonding curve" signal extraction.
//!
//! WebSocket `programSubscribe` delivers **every** account update owned by a DEX
//! program (swaps, bin arrays, tick arrays, vaults). Parsers must reject anything
//! that is not a pool-state / bonding-curve account with a plausible mint.

use dashmap::DashSet;
use solana_pubkey::Pubkey;
use solbot_common::types::Signal;
use std::str::FromStr;
use std::sync::Arc;

/// Native SOL wrapped mint — never the sniper "target" mint alone.
pub const NATIVE_MINT: &str = "So11111111111111111111111111111111111111112";
pub const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";

/// Session-scoped set of pool/curve addresses already emitted (first-seen = new).
///
/// On WebSocket subscribe the node often replays **existing** accounts once.
/// During `warmup_secs` we still **record** addresses (so they won't fire later)
/// but do **not** emit signals — only true new accounts after warmup become signals.
#[derive(Clone)]
pub struct SeenPools {
    inner: Arc<DashSet<String>>,
    started: std::time::Instant,
    warmup: std::time::Duration,
}

impl Default for SeenPools {
    fn default() -> Self {
        Self::new()
    }
}

impl SeenPools {
    pub fn new() -> Self {
        let warmup_secs: u64 = std::env::var("SOLBOT_SIGNAL_WARMUP_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15);
        Self::with_warmup_secs(warmup_secs)
    }

    pub fn with_warmup_secs(warmup_secs: u64) -> Self {
        Self {
            inner: Arc::new(DashSet::new()),
            started: std::time::Instant::now(),
            warmup: std::time::Duration::from_secs(warmup_secs),
        }
    }

    pub fn in_warmup(&self) -> bool {
        self.started.elapsed() < self.warmup
    }

    /// Returns true if this is the **first** time we see `pool` **and** warmup is over.
    /// First sightings during warmup are recorded but suppressed (not emitted).
    pub fn try_claim(&self, pool: &str) -> bool {
        let is_new = self.inner.insert(pool.to_string());
        if !is_new {
            return false;
        }
        if self.in_warmup() {
            return false; // seed seen-set, no emit
        }
        true
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// Reject mints that cannot be real SPL token mints for sniping.
pub fn is_plausible_spl_mint(mint: &Pubkey) -> bool {
    let s = mint.to_string();
    if s == SYSTEM_PROGRAM || s == NATIVE_MINT {
        return false;
    }
    // Default / zero pubkey
    if *mint == Pubkey::default() {
        return false;
    }
    // All-zero-ish base58 variants and known junk patterns from mis-parsed layouts
    if s.chars().all(|c| c == '1') {
        return false;
    }
    // System-program-like short base58 (token mints are 32-44 chars; system is 32 ones)
    if s.starts_with("11111111") && s.len() <= 44 {
        // Real mints can theoretically start with 1… but pure system id already excluded.
        // Reject any mint that is mostly '1' (misaligned layout).
        let ones = s.chars().filter(|&c| c == '1').count();
        if ones >= s.len().saturating_sub(4) {
            return false;
        }
    }
    true
}

pub fn read_pubkey(data: &[u8], offset: usize) -> Option<Pubkey> {
    if offset + 32 > data.len() {
        return None;
    }
    let bytes: [u8; 32] = data[offset..offset + 32].try_into().ok()?;
    Some(Pubkey::new_from_array(bytes))
}

/// Prefer the non-WSOL mint of a pair (sniper target).
pub fn pick_target_mint(mint_a: Pubkey, mint_b: Pubkey) -> Option<Pubkey> {
    let wsol = Pubkey::from_str(NATIVE_MINT).ok()?;
    if mint_a == wsol && is_plausible_spl_mint(&mint_b) {
        return Some(mint_b);
    }
    if mint_b == wsol && is_plausible_spl_mint(&mint_a) {
        return Some(mint_a);
    }
    // Non-WSOL pairs: still allow if both look like SPL mints (stable pairs etc.)
    if is_plausible_spl_mint(&mint_a) {
        return Some(mint_a);
    }
    if is_plausible_spl_mint(&mint_b) {
        return Some(mint_b);
    }
    None
}

fn build_signal(
    mint: Pubkey,
    pool: Pubkey,
    owner: Pubkey,
    slot: u64,
    received_at: u64,
    source: &str,
    data: &[u8],
) -> Option<Signal> {
    if !is_plausible_spl_mint(&mint) {
        return None;
    }
    Some(Signal {
        mint: mint.to_string(),
        pool_address: pool.to_string(),
        dex_program: owner.to_string(),
        signal_source: source.to_string(),
        slot,
        received_at,
        raw_pool_account_data: Some(data.to_vec()),
        raw_pool_account_owner: Some(owner.to_string()),
    })
}

// --- Exact-ish account sizes (pool state only; not bin/tick arrays) ---

/// Raydium AMM v4 AmmInfo is 752 bytes.
pub const RAYDIUM_AMM_V4_SIZE: usize = 752;
/// Raydium CPMM PoolState ~637–661; accept tight band.
pub const RAYDIUM_CPMM_SIZE_MIN: usize = 600;
pub const RAYDIUM_CPMM_SIZE_MAX: usize = 700;
/// Raydium CLMM PoolState is 1544 bytes (not tick arrays 10k+).
pub const RAYDIUM_CLMM_POOL_SIZE: usize = 1544;
/// Meteora DLMM LbPair ~904 bytes (bin arrays are ~10k).
pub const METEORA_DLMM_LB_PAIR_MIN: usize = 800;
pub const METEORA_DLMM_LB_PAIR_MAX: usize = 1200;
/// Pump.fun BondingCurve account ~82–150 bytes.
pub const PUMP_CURVE_SIZE_MIN: usize = 81;
pub const PUMP_CURVE_SIZE_MAX: usize = 160;

/// Pump.fun initial virtual token reserves (approx) — used to prefer brand-new curves.
/// Trades change reserves; first-seen + size still primary filter.
const PUMP_INITIAL_VIRTUAL_TOKEN_RESERVES: u64 = 1_073_000_000_000_000;

pub fn try_parse_raydium_amm_v4(
    account: Pubkey,
    owner: Pubkey,
    data: &[u8],
    slot: u64,
    ts_ms: u64,
    source: &str,
    seen: &SeenPools,
) -> Option<Signal> {
    if data.len() != RAYDIUM_AMM_V4_SIZE {
        return None;
    }
    if !seen.try_claim(&account.to_string()) {
        return None;
    }
    // coin_mint @ 400, pc_mint @ 432 (AmmInfo layout)
    let coin = read_pubkey(data, 400)?;
    let pc = read_pubkey(data, 432)?;
    let mint = pick_target_mint(coin, pc)?;
    build_signal(mint, account, owner, slot, ts_ms, source, data)
}

pub fn try_parse_raydium_cpmm(
    account: Pubkey,
    owner: Pubkey,
    data: &[u8],
    slot: u64,
    ts_ms: u64,
    source: &str,
    seen: &SeenPools,
) -> Option<Signal> {
    if data.len() < RAYDIUM_CPMM_SIZE_MIN || data.len() > RAYDIUM_CPMM_SIZE_MAX {
        return None;
    }
    if !seen.try_claim(&account.to_string()) {
        return None;
    }
    // Matches tx_builder::raydium_cpmm PoolState: token_0_mint @ 72, token_1 @ 104
    let m0 = read_pubkey(data, 72)?;
    let m1 = read_pubkey(data, 104)?;
    let mint = pick_target_mint(m0, m1)?;
    build_signal(mint, account, owner, slot, ts_ms, source, data)
}

pub fn try_parse_raydium_clmm(
    account: Pubkey,
    owner: Pubkey,
    data: &[u8],
    slot: u64,
    ts_ms: u64,
    source: &str,
    seen: &SeenPools,
) -> Option<Signal> {
    // Exact pool size — rejects tick arrays (10k), personal positions, etc.
    if data.len() != RAYDIUM_CLMM_POOL_SIZE && data.len() != 1544 + 8 {
        // allow small padding variants around official size
        if !(data.len() >= 1500 && data.len() <= 1600) {
            return None;
        }
    }
    if !seen.try_claim(&account.to_string()) {
        return None;
    }
    // Official: token_mint_0 @ 73, token_mint_1 @ 105 (disc 8 + bump 1 + amm_config 32 + owner 32)
    let m0 = read_pubkey(data, 73)?;
    let m1 = read_pubkey(data, 105)?;
    let mint = pick_target_mint(m0, m1)?;
    build_signal(mint, account, owner, slot, ts_ms, source, data)
}

pub fn try_parse_meteora_dlmm(
    account: Pubkey,
    owner: Pubkey,
    data: &[u8],
    slot: u64,
    ts_ms: u64,
    source: &str,
    seen: &SeenPools,
) -> Option<Signal> {
    // Live mainnet LbPair accounts are exactly 904 bytes (bin arrays ~10k).
    if data.len() != 904 && (data.len() < METEORA_DLMM_LB_PAIR_MIN || data.len() > METEORA_DLMM_LB_PAIR_MAX)
    {
        return None;
    }
    if !seen.try_claim(&account.to_string()) {
        return None;
    }
    // Verified on mainnet LbPair dumps (2026-08-09): token_x @ 88, token_y @ 120 (WSOL pair).
    // Older guess 72/104 produced ghost mints → mint_unavailable at risk-gate.
    let mx = read_pubkey(data, 88)?;
    let my = read_pubkey(data, 120)?;
    let mint = pick_target_mint(mx, my)?;
    build_signal(mint, account, owner, slot, ts_ms, source, data)
}

/// Meteora DAMM v2 (cpamdp...) — not the same as Dynamic AMM Eo7WjKq...
pub fn try_parse_meteora_damm_v2(
    account: Pubkey,
    owner: Pubkey,
    data: &[u8],
    slot: u64,
    ts_ms: u64,
    source: &str,
    seen: &SeenPools,
) -> Option<Signal> {
    // Live mainnet pool state = 1112 bytes. Smaller accounts (e.g. 408) are not pool state.
    if data.len() != 1112 && !(data.len() >= 1100 && data.len() <= 1120) {
        return None;
    }
    if !seen.try_claim(&account.to_string()) {
        return None;
    }
    // Verified: token mints @ 168 and 200 (WSOL often at 200).
    let m0 = read_pubkey(data, 168)?;
    let m1 = read_pubkey(data, 200)?;
    let mint = pick_target_mint(m0, m1)?;
    build_signal(mint, account, owner, slot, ts_ms, source, data)
}

pub fn try_parse_meteora_dynamic(
    account: Pubkey,
    owner: Pubkey,
    data: &[u8],
    slot: u64,
    ts_ms: u64,
    source: &str,
    seen: &SeenPools,
) -> Option<Signal> {
    // Legacy Dynamic AMM (Eo7WjKq...) — keep separate from DAMM v2.
    if data.len() < 200 || data.len() > 2000 {
        return None;
    }
    if !seen.try_claim(&account.to_string()) {
        return None;
    }
    let m0 = read_pubkey(data, 8)?;
    let m1 = read_pubkey(data, 40)?;
    let mint = pick_target_mint(m0, m1)?;
    build_signal(mint, account, owner, slot, ts_ms, source, data)
}

pub fn try_parse_pump_bonding_curve(
    account: Pubkey,
    owner: Pubkey,
    data: &[u8],
    slot: u64,
    ts_ms: u64,
    source: &str,
    seen: &SeenPools,
    migration_only: bool,
) -> Option<Signal> {
    if data.len() < PUMP_CURVE_SIZE_MIN || data.len() > PUMP_CURVE_SIZE_MAX {
        return None;
    }
    let mint = read_pubkey(data, 8)?;
    if !is_plausible_spl_mint(&mint) {
        return None;
    }
    // Verify PDA: bonding_curve = PDA(["bonding-curve", mint], program)
    let (expected_pda, _) =
        Pubkey::find_program_address(&[b"bonding-curve", mint.as_ref()], &owner);
    if expected_pda != account {
        return None;
    }

    let complete = data.get(8 + 72).copied().unwrap_or(0) != 0;
    if migration_only && !complete {
        return None;
    }
    if !migration_only && complete {
        return None;
    }

    // First-seen only — every trade updates the curve; without this queue explodes.
    // Migration uses a distinct key so a curve we tracked as active can still emit migrate.
    let claim_key = if complete {
        format!("{}:migrated", account)
    } else {
        account.to_string()
    };
    if !seen.try_claim(&claim_key) {
        return None;
    }

    let src = if complete {
        "pump_fun_migration"
    } else {
        source
    };

    // Soft preference: brand-new curves still have near-initial virtual token reserves.
    // Do not hard-require (migrations / edge cases) — first-seen PDA is enough.
    let _vtr = u64::from_le_bytes(data.get(40..48).and_then(|s| s.try_into().ok()).unwrap_or([0; 8]));
    let _ = PUMP_INITIAL_VIRTUAL_TOKEN_RESERVES;

    build_signal(mint, account, owner, slot, ts_ms, src, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(s: &str) -> Pubkey {
        Pubkey::from_str(s).unwrap()
    }

    #[test]
    fn rejects_system_and_native_mint() {
        assert!(!is_plausible_spl_mint(&pk(SYSTEM_PROGRAM)));
        assert!(!is_plausible_spl_mint(&pk(NATIVE_MINT)));
        assert!(!is_plausible_spl_mint(&Pubkey::default()));
    }

    #[test]
    fn seen_pools_only_first_claim() {
        let s = SeenPools::with_warmup_secs(0);
        assert!(s.try_claim("pool1"));
        assert!(!s.try_claim("pool1"));
        assert!(s.try_claim("pool2"));
    }

    #[test]
    fn warmup_suppresses_emit_but_records() {
        let s = SeenPools::with_warmup_secs(60);
        assert!(!s.try_claim("pool_warm"), "warmup must suppress emit");
        assert!(!s.try_claim("pool_warm"), "already recorded");
    }

    #[test]
    fn clmm_rejects_tick_array_size() {
        let seen = SeenPools::with_warmup_secs(0);
        let data = vec![0u8; 10240]; // tick array size from live logs
        let owner = pk("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK");
        let account = Pubkey::new_unique();
        assert!(try_parse_raydium_clmm(account, owner, &data, 1, 1, "test", &seen).is_none());
    }

    #[test]
    fn dlmm_rejects_bin_array_size() {
        let seen = SeenPools::with_warmup_secs(0);
        let data = vec![0u8; 10136];
        let owner = pk("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");
        assert!(try_parse_meteora_dlmm(Pubkey::new_unique(), owner, &data, 1, 1, "t", &seen).is_none());
    }

    #[test]
    fn pump_rejects_non_pda_account() {
        let seen = SeenPools::with_warmup_secs(0);
        let mut data = vec![0u8; 90];
        let mint = Pubkey::new_unique();
        data[8..40].copy_from_slice(mint.as_ref());
        let owner = pk("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
        let random_account = Pubkey::new_unique(); // not PDA
        assert!(
            try_parse_pump_bonding_curve(random_account, owner, &data, 1, 1, "t", &seen, false)
                .is_none()
        );
    }

    #[test]
    fn pump_accepts_valid_pda_first_seen_only() {
        let seen = SeenPools::with_warmup_secs(0);
        let mut data = vec![0u8; 90];
        let mint = Pubkey::new_unique();
        data[8..40].copy_from_slice(mint.as_ref());
        // non-zero virtual reserves so layout looks live
        data[40..48].copy_from_slice(&1_000_000u64.to_le_bytes());
        let owner = pk("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
        let (pda, _) = Pubkey::find_program_address(&[b"bonding-curve", mint.as_ref()], &owner);
        let s1 = try_parse_pump_bonding_curve(pda, owner, &data, 1, 1, "helius_ws", &seen, false);
        assert!(s1.is_some(), "first PDA curve should emit");
        assert_eq!(s1.unwrap().mint, mint.to_string());
        let s2 = try_parse_pump_bonding_curve(pda, owner, &data, 2, 2, "helius_ws", &seen, false);
        assert!(s2.is_none(), "second update must be deduped");
    }

    #[test]
    fn garbage_system_mint_bytes_rejected() {
        let seen = SeenPools::with_warmup_secs(0);
        let mut data = vec![0u8; RAYDIUM_AMM_V4_SIZE];
        // leave mints as zeros → not plausible
        let owner = pk("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
        assert!(try_parse_raydium_amm_v4(Pubkey::new_unique(), owner, &data, 1, 1, "t", &seen).is_none());
    }

    #[test]
    fn dlmm_uses_offset_88_120_not_72_104() {
        let seen = SeenPools::with_warmup_secs(0);
        let mut data = vec![0u8; 904];
        let token = Pubkey::new_unique();
        let wsol = pk(NATIVE_MINT);
        data[88..120].copy_from_slice(token.as_ref());
        data[120..152].copy_from_slice(wsol.as_ref());
        let owner = pk("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");
        let sig = try_parse_meteora_dlmm(Pubkey::new_unique(), owner, &data, 1, 1, "t", &seen)
            .expect("valid dlmm layout");
        assert_eq!(sig.mint, token.to_string());
    }

    #[test]
    fn damm_v2_rejects_408_byte_non_pool() {
        let seen = SeenPools::with_warmup_secs(0);
        let data = vec![0u8; 408];
        let owner = pk("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");
        assert!(try_parse_meteora_damm_v2(Pubkey::new_unique(), owner, &data, 1, 1, "t", &seen).is_none());
    }

    #[test]
    fn damm_v2_mints_at_168_200() {
        let seen = SeenPools::with_warmup_secs(0);
        let mut data = vec![0u8; 1112];
        let token = Pubkey::new_unique();
        let wsol = pk(NATIVE_MINT);
        data[168..200].copy_from_slice(token.as_ref());
        data[200..232].copy_from_slice(wsol.as_ref());
        let owner = pk("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");
        let sig = try_parse_meteora_damm_v2(Pubkey::new_unique(), owner, &data, 1, 1, "t", &seen)
            .expect("damm v2");
        assert_eq!(sig.mint, token.to_string());
    }
}
