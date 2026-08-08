pub mod lp_lock_check;
pub mod mint_freeze_authority;
pub mod simulate_sell;
pub mod token2022_extensions;

use dashmap::DashMap;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solbot_common::types::Signal;
use std::sync::Arc;
use std::time::{Duration, Instant};

// TEMP: Increased for current non-co-located network conditions.
// Current measured RPC RTT is around 120-130ms, so 80ms is too aggressive
// for real Helius/remote RPC traffic. Reduce back to 80ms after moving the
// bot nearer to the RPC provider or colocating with a low-latency endpoint.
const PRE_BUY_CHECK_RPC_TIMEOUT_MS: u64 = 200;

/// Context holding pre-fetched account data to avoid redundant RPC calls.
/// This optimization reduces latency by:
/// 1. Using account data from Geyser/Helius WS payload when available (no RPC needed)
/// 2. Fetching missing accounts via batch RPC requests (get_multiple_accounts)
/// 3. Using nonblocking RpcClient with connection pooling
#[derive(Debug, Clone)]
pub struct PreBuyCheckContext {
    pub mint_data: Option<Vec<u8>>,
    pub mint_owner: Option<Pubkey>,
    pub pool_data: Option<Vec<u8>>,
    pub pool_owner: Option<Pubkey>,
    pub lp_mint_data: Option<Vec<u8>>,
    pub lp_mint_owner: Option<Pubkey>,
    /// Whether pool data came from WS (no RPC needed) or was fetched via RPC
    pub pool_data_from_ws: bool,
}

impl PreBuyCheckContext {
    /// Загружает все необходимые аккаунты.
    /// Если lp_mint известна заранее (или передана из dex_listeners), сокращает сеть до 1 RTT.
    pub async fn fetch(
        rpc: &Arc<RpcClient>,
        mint: &str,
        pool_address: &str,
        known_lp_mint: Option<Pubkey>,
    ) -> anyhow::Result<Self> {
        let mint_pubkey = Pubkey::try_from(mint)?;
        let pool_pubkey = Pubkey::try_from(pool_address)?;

        // Вариант А: Если LP Mint известна заранее, запрашиваем 3 аккаунта за 1 RTT
        if let Some(lp_pubkey) = known_lp_mint {
            let accounts = match tokio::time::timeout(
                Duration::from_millis(PRE_BUY_CHECK_RPC_TIMEOUT_MS),
                rpc.get_multiple_accounts(&[mint_pubkey, pool_pubkey, lp_pubkey]),
            )
            .await
            {
                Ok(result) => result?,
                Err(_) => {
                    tracing::warn!(
                        mint = %mint,
                        pool = %pool_address,
                        lp_mint = %lp_pubkey,
                        "Pre-buy check get_multiple_accounts timed out after {}ms",
                        PRE_BUY_CHECK_RPC_TIMEOUT_MS
                    );
                    return Err(anyhow::anyhow!(
                        "Pre-buy check RPC timed out after {}ms",
                        PRE_BUY_CHECK_RPC_TIMEOUT_MS
                    ));
                }
            };

            let (mint_data, mint_owner) = unpack_acc(accounts.get(0));
            let (pool_data, pool_owner) = unpack_acc(accounts.get(1));
            let (lp_mint_data, lp_mint_owner) = unpack_acc(accounts.get(2));

            return Ok(Self {
                mint_data,
                mint_owner,
                pool_data,
                pool_owner,
                lp_mint_data,
                lp_mint_owner,
                pool_data_from_ws: false,
            });
        }

        // Вариант Б: LP Mint не известна. Шаг 1 — батч mint + pool (1 RTT)
        let accounts = match tokio::time::timeout(
            Duration::from_millis(PRE_BUY_CHECK_RPC_TIMEOUT_MS),
            rpc.get_multiple_accounts(&[mint_pubkey, pool_pubkey]),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                tracing::warn!(
                    mint = %mint,
                    pool = %pool_address,
                    "Pre-buy check get_multiple_accounts timed out after {}ms",
                    PRE_BUY_CHECK_RPC_TIMEOUT_MS
                );
                return Err(anyhow::anyhow!(
                    "Pre-buy check RPC timed out after {}ms",
                    PRE_BUY_CHECK_RPC_TIMEOUT_MS
                ));
            }
        };

        let (mint_data, mint_owner) = unpack_acc(accounts.get(0));
        let (pool_data, pool_owner) = unpack_acc(accounts.get(1));

        // Шаг 2 — Догрузка LP Mint только если удалось извлечь адрес из pool_data
        let (lp_mint_data, lp_mint_owner) = match (&pool_data, &pool_owner) {
            (Some(p_data), Some(p_owner)) => {
                if let Some(lp_pk) = extract_lp_mint_from_pool(p_data, p_owner) {
                    let lp_mint_account = match tokio::time::timeout(
                        Duration::from_millis(PRE_BUY_CHECK_RPC_TIMEOUT_MS),
                        rpc.get_account(&lp_pk),
                    )
                    .await
                    {
                        Ok(result) => result?,
                        Err(_) => {
                            tracing::warn!(
                                lp_mint = %lp_pk,
                                "Pre-buy check get_account(lp_mint) timed out after {}ms",
                                PRE_BUY_CHECK_RPC_TIMEOUT_MS
                            );
                            return Err(anyhow::anyhow!(
                                "Pre-buy check RPC timed out after {}ms",
                                PRE_BUY_CHECK_RPC_TIMEOUT_MS
                            ));
                        }
                    };

                    (Some(lp_mint_account.data), Some(lp_mint_account.owner))
                } else {
                    (None, None)
                }
            }
            _ => (None, None),
        };

        Ok(Self {
            mint_data,
            mint_owner,
            pool_data,
            pool_owner,
            lp_mint_data,
            lp_mint_owner,
            pool_data_from_ws: false,
        })
    }

    /// Создает контекст на основе данных из WS (0 RTT для pool, 1 RTT для mint + lp_mint)
    pub async fn from_ws_update(
        rpc: &Arc<RpcClient>,
        mint: &str,
        pool_data: Vec<u8>,
        pool_owner: Pubkey,
    ) -> anyhow::Result<Self> {
        let mint_pubkey = Pubkey::try_from(mint)?;

        // Извлекаем LP Mint ДО сетевого запроса!
        let maybe_lp_mint = extract_lp_mint_from_pool(&pool_data, &pool_owner);

        let (mint_data, mint_owner, lp_mint_data, lp_mint_owner) = match maybe_lp_mint {
            Some(lp_pk) => {
                // Запрашиваем mint и lp_mint одновременно за 1 RTT
                let accounts = match tokio::time::timeout(
                    Duration::from_millis(PRE_BUY_CHECK_RPC_TIMEOUT_MS),
                    rpc.get_multiple_accounts(&[mint_pubkey, lp_pk]),
                )
                .await
                {
                    Ok(result) => result?,
                    Err(_) => {
                        tracing::warn!(
                            lp_mint = %lp_pk,
                            "Pre-buy check from_ws_update get_multiple_accounts timed out after {}ms",
                            PRE_BUY_CHECK_RPC_TIMEOUT_MS
                        );
                        return Err(anyhow::anyhow!(
                            "Pre-buy check RPC timed out after {}ms",
                            PRE_BUY_CHECK_RPC_TIMEOUT_MS
                        ));
                    }
                };

                let (m_data, m_owner) = unpack_acc(accounts.get(0));
                let (lp_data, lp_owner) = unpack_acc(accounts.get(1));
                (m_data, m_owner, lp_data, lp_owner)
            }
            None => {
                // Извлечь LP Mint не удалось/не требуется (например, Pump.fun) — запрашиваем только mint
                let acc = match tokio::time::timeout(
                    Duration::from_millis(PRE_BUY_CHECK_RPC_TIMEOUT_MS),
                    rpc.get_account(&mint_pubkey),
                )
                .await
                {
                    Ok(result) => result?,
                    Err(_) => {
                        tracing::warn!(
                            mint = %mint,
                            "Pre-buy check from_ws_update get_account(mint) timed out after {}ms",
                            PRE_BUY_CHECK_RPC_TIMEOUT_MS
                        );
                        return Err(anyhow::anyhow!(
                            "Pre-buy check RPC timed out after {}ms",
                            PRE_BUY_CHECK_RPC_TIMEOUT_MS
                        ));
                    }
                };
                (Some(acc.data), Some(acc.owner), None, None)
            }
        };

        Ok(Self {
            mint_data,
            mint_owner,
            pool_data: Some(pool_data),
            pool_owner: Some(pool_owner),
            lp_mint_data,
            lp_mint_owner,
            pool_data_from_ws: true,
        })
    }
}

#[inline]
fn unpack_acc(
    acc_opt: Option<&Option<solana_sdk::account::Account>>,
) -> (Option<Vec<u8>>, Option<Pubkey>) {
    match acc_opt {
        Some(Some(acc)) => (Some(acc.data.clone()), Some(acc.owner)),
        _ => (None, None),
    }
}

/// Extract LP mint address from pool account data based on DEX type.
/// Returns None if LP mint cannot be extracted (e.g., CLMM, DLMM, Pump.fun).
/// Optimized with byte comparison instead of string formatting.
pub fn extract_lp_mint_from_pool(pool_data: &[u8], pool_owner: &Pubkey) -> Option<Pubkey> {
    const RAYDIUM_V4: Pubkey =
        solana_pubkey::pubkey!("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
    const RAYDIUM_CPMM: Pubkey =
        solana_pubkey::pubkey!("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
    const METEORA_DYNAMIC: Pubkey =
        solana_pubkey::pubkey!("Eo7WjKq67rjJQSZxS6z3YKapzY3eMj6Xy8X5EQVn5UaB");

    if *pool_owner == RAYDIUM_V4 || *pool_owner == RAYDIUM_CPMM || *pool_owner == METEORA_DYNAMIC {
        if pool_data.len() >= 64 {
            let lp_mint_bytes: [u8; 32] = pool_data[32..64].try_into().ok()?;
            return Some(Pubkey::from(lp_mint_bytes));
        }
    }

    None
}

#[derive(Debug, Clone, PartialEq)]
pub enum RiskGateVerdict {
    Accept,
    Reject { reason: String },
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub passed: bool,
    pub reason: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub struct RiskGateCache(Arc<RiskGateCacheInner>);

#[derive(Debug)]
struct RiskGateCacheInner {
    inner: DashMap<String, CacheEntry>,
    ttl: Duration,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    verdict: RiskGateVerdict,
    inserted_at: Instant,
}

impl RiskGateCache {
    pub fn new(ttl_secs: u64) -> Self {
        Self(Arc::new(RiskGateCacheInner {
            inner: DashMap::new(),
            ttl: Duration::from_secs(ttl_secs),
        }))
    }

    pub fn get(&self, key: &str) -> Option<RiskGateVerdict> {
        let is_fresh = self.0.inner.get(key).map_or(false, |entry| {
            let is_fresh = entry.inserted_at.elapsed() < self.0.ttl;
            if !is_fresh {
                drop(entry);
            }
            is_fresh
        });

        if is_fresh {
            return self.0.inner.get(key).map(|entry| entry.verdict.clone());
        }

        self.0.inner.remove(key);
        None
    }

    pub fn insert(&self, key: String, verdict: RiskGateVerdict) {
        self.0.inner.insert(
            key,
            CacheEntry {
                verdict,
                inserted_at: Instant::now(),
            },
        );
    }
}

fn is_cacheable(verdict: &RiskGateVerdict) -> bool {
    match verdict {
        RiskGateVerdict::Accept => true,
        RiskGateVerdict::Reject { reason } => {
            let lower = reason.to_lowercase();
            !(lower.contains("timeout")
                || lower.contains("rpc error")
                || lower.contains("failed to fetch pre-buy check context")
                || lower.contains("task join error")
                || lower.contains("simulation error")
                || lower.contains("lp_lock:context_unavailable")
                || lower.contains("lp_lock:parse_failed"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn risk_gate_cache_expires_after_ttl() {
        let cache = RiskGateCache::new(1);
        let key = "test-pool".to_string();
        let verdict = RiskGateVerdict::Accept;

        cache.insert(key.clone(), verdict.clone());
        assert_eq!(cache.get(&key), Some(verdict.clone()));

        thread::sleep(Duration::from_secs(2));

        assert!(cache.get(&key).is_none());
    }
}

pub async fn run_pre_buy_checks(signal: &Signal, config: &RiskGateConfig) -> RiskGateVerdict {
    let start = std::time::Instant::now();

    tracing::info!("Running pre-buy checks for mint: {}", signal.mint);

    let cache_key = signal.pool_address.clone();
    if let Some(cached_verdict) = config.cache.get(&cache_key) {
        tracing::debug!(pool = %signal.pool_address, "Using cached pre-buy risk gate verdict");
        return cached_verdict;
    }

    // Create shared nonblocking RPC client with connection pooling
    let rpc = Arc::new(RpcClient::new(config.rpc_url.clone()));

    // Try 1-RTT path when WS-provided pool bytes are available.
    let context = {
        if let (Some(pool_data), Some(pool_owner_str)) = (
            &signal.raw_pool_account_data,
            &signal.raw_pool_account_owner,
        ) {
            if let Ok(pool_owner_pk) = Pubkey::try_from(pool_owner_str.as_str()) {
                match PreBuyCheckContext::from_ws_update(&rpc, &signal.mint, pool_data.clone(), pool_owner_pk).await {
                    Ok(ctx) => ctx,
                    Err(e) => {
                        tracing::warn!("Failed to build pre-buy context from WS update: {}, falling back to RPC fetch", e);
                        match PreBuyCheckContext::fetch(&rpc, &signal.mint, &signal.pool_address, None).await {
                            Ok(ctx2) => ctx2,
                            Err(e2) => {
                                tracing::warn!(
                                    "Failed to fetch pre-buy check context: {}, falling back to individual RPC calls",
                                    e2
                                );
                                // Fallback to individual checks if both optimized paths failed
                                let (freeze_result, lp_result, token2022_result, simulate_result) = tokio::join!(
                                    mint_freeze_authority::check(&signal.mint, &config.rpc_url),
                                    lp_lock_check::check(&signal.pool_address, &signal.dex_program, &config.rpc_url),
                                    token2022_extensions::check(&signal.mint, &config.rpc_url, config),
                                    simulate_sell::check(
                                        &signal.mint,
                                        &signal.pool_address,
                                        config.simulate_size_lamports,
                                        &config.rpc_url
                                    )
                                );

                                let elapsed = start.elapsed();
                                tracing::info!(
                                    "Pre-buy checks completed in {}ms (fallback mode)",
                                    elapsed.as_millis()
                                );

                                let verdict = if !freeze_result.passed {
                                    RiskGateVerdict::Reject {
                                        reason: freeze_result
                                            .reason
                                            .unwrap_or_else(|| "Mint freeze authority check failed".to_string()),
                                    }
                                } else if !lp_result.passed {
                                    RiskGateVerdict::Reject {
                                        reason: lp_result
                                            .reason
                                            .unwrap_or_else(|| "LP lock check failed".to_string()),
                                    }
                                } else if !token2022_result.passed {
                                    RiskGateVerdict::Reject {
                                        reason: token2022_result
                                            .reason
                                            .unwrap_or_else(|| "Token2022 extensions check failed".to_string()),
                                    }
                                } else if !simulate_result.passed {
                                    RiskGateVerdict::Reject {
                                        reason: simulate_result
                                            .reason
                                            .unwrap_or_else(|| "Simulate sell check failed".to_string()),
                                    }
                                } else {
                                    RiskGateVerdict::Accept
                                };

                                if is_cacheable(&verdict) {
                                    config.cache.insert(cache_key.clone(), verdict.clone());
                                }

                                return verdict;
                            }
                        }
                    }
                }
            } else {
                // Owner string parse failed - fallback to fetch
                match PreBuyCheckContext::fetch(&rpc, &signal.mint, &signal.pool_address, None).await {
                    Ok(ctx) => ctx,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to fetch pre-buy check context: {}, falling back to individual RPC calls",
                            e
                        );
                        let (freeze_result, lp_result, token2022_result, simulate_result) = tokio::join!(
                            mint_freeze_authority::check(&signal.mint, &config.rpc_url),
                            lp_lock_check::check(&signal.pool_address, &signal.dex_program, &config.rpc_url),
                            token2022_extensions::check(&signal.mint, &config.rpc_url, config),
                            simulate_sell::check(
                                &signal.mint,
                                &signal.pool_address,
                                config.simulate_size_lamports,
                                &config.rpc_url
                            )
                        );

                        let elapsed = start.elapsed();
                        tracing::info!(
                            "Pre-buy checks completed in {}ms (fallback mode)",
                            elapsed.as_millis()
                        );

                        let verdict = if !freeze_result.passed {
                            RiskGateVerdict::Reject {
                                reason: freeze_result
                                    .reason
                                    .unwrap_or_else(|| "Mint freeze authority check failed".to_string()),
                            }
                        } else if !lp_result.passed {
                            RiskGateVerdict::Reject {
                                reason: lp_result
                                    .reason
                                    .unwrap_or_else(|| "LP lock check failed".to_string()),
                            }
                        } else if !token2022_result.passed {
                            RiskGateVerdict::Reject {
                                reason: token2022_result
                                    .reason
                                    .unwrap_or_else(|| "Token2022 extensions check failed".to_string()),
                            }
                        } else if !simulate_result.passed {
                            RiskGateVerdict::Reject {
                                reason: simulate_result
                                    .reason
                                    .unwrap_or_else(|| "Simulate sell check failed".to_string()),
                            }
                        } else {
                            RiskGateVerdict::Accept
                        };

                        if is_cacheable(&verdict) {
                            config.cache.insert(cache_key.clone(), verdict.clone());
                        }

                        return verdict;
                    }
                }
            }
        } else {
            // No WS-provided pool bytes; perform standard fetch
            match PreBuyCheckContext::fetch(&rpc, &signal.mint, &signal.pool_address, None).await {
                Ok(ctx) => ctx,
                Err(e) => {
                    tracing::warn!(
                        "Failed to fetch pre-buy check context: {}, falling back to individual RPC calls",
                        e
                    );

                    let (freeze_result, lp_result, token2022_result, simulate_result) = tokio::join!(
                        mint_freeze_authority::check(&signal.mint, &config.rpc_url),
                        lp_lock_check::check(&signal.pool_address, &signal.dex_program, &config.rpc_url),
                        token2022_extensions::check(&signal.mint, &config.rpc_url, config),
                        simulate_sell::check(
                            &signal.mint,
                            &signal.pool_address,
                            config.simulate_size_lamports,
                            &config.rpc_url
                        )
                    );

                    let elapsed = start.elapsed();
                    tracing::info!(
                        "Pre-buy checks completed in {}ms (fallback mode)",
                        elapsed.as_millis()
                    );

                    let verdict = if !freeze_result.passed {
                        RiskGateVerdict::Reject {
                            reason: freeze_result
                                .reason
                                .unwrap_or_else(|| "Mint freeze authority check failed".to_string()),
                        }
                    } else if !lp_result.passed {
                        RiskGateVerdict::Reject {
                            reason: lp_result
                                .reason
                                .unwrap_or_else(|| "LP lock check failed".to_string()),
                        }
                    } else if !token2022_result.passed {
                        RiskGateVerdict::Reject {
                            reason: token2022_result
                                .reason
                                .unwrap_or_else(|| "Token2022 extensions check failed".to_string()),
                        }
                    } else if !simulate_result.passed {
                        RiskGateVerdict::Reject {
                            reason: simulate_result
                                .reason
                                .unwrap_or_else(|| "Simulate sell check failed".to_string()),
                        }
                    } else {
                        RiskGateVerdict::Accept
                    };

                    if is_cacheable(&verdict) {
                        config.cache.insert(cache_key.clone(), verdict.clone());
                    }

                    return verdict;
                }
            }
        }
    };

    // Log whether we used WS-provided pool bytes (1-RTT) or RPC fetch (2-RTT)
    tracing::info!(
        mint = %signal.mint,
        pool = %signal.pool_address,
        pool_data_from_ws = %context.pool_data_from_ws,
        "Pre-buy context source"
    );

    // Use optimized checks with pre-fetched account data
    let (freeze_result, lp_result, token2022_result, simulate_result) = tokio::join!(
        mint_freeze_authority::check_with_context(&signal.mint, &context),
        lp_lock_check::check_with_context(&signal.pool_address, &signal.dex_program, &context),
        token2022_extensions::check_with_context(&signal.mint, config, &context),
        simulate_sell::check(
            &signal.mint,
            &signal.pool_address,
            config.simulate_size_lamports,
            &config.rpc_url
        )
    );

    let elapsed = start.elapsed();
    tracing::info!(
        "Pre-buy checks completed in {}ms (optimized mode)",
        elapsed.as_millis()
    );

    if !freeze_result.passed {
        return RiskGateVerdict::Reject {
            reason: freeze_result
                .reason
                .unwrap_or_else(|| "Mint freeze authority check failed".to_string()),
        };
    }

    if !lp_result.passed {
        return RiskGateVerdict::Reject {
            reason: lp_result
                .reason
                .unwrap_or_else(|| "LP lock check failed".to_string()),
        };
    }

    if !token2022_result.passed {
        return RiskGateVerdict::Reject {
            reason: token2022_result
                .reason
                .unwrap_or_else(|| "Token2022 extensions check failed".to_string()),
        };
    }

    let verdict = if !simulate_result.passed {
        RiskGateVerdict::Reject {
            reason: simulate_result
                .reason
                .unwrap_or_else(|| "Simulate sell check failed".to_string()),
        }
    } else {
        RiskGateVerdict::Accept
    };

    if is_cacheable(&verdict) {
        config.cache.insert(cache_key, verdict.clone());
    }

    verdict
}

/// Run pre-buy checks with a hard timeout wrapper (Risk Gate 3.5).
///
/// Wraps `run_pre_buy_checks` with a 300ms hard timeout (temporary for paper-trading).
/// TODO: Reduce back to 150ms after batch RPC optimization is implemented.
/// If the checks exceed this time, the signal is rejected to prevent
/// slow RPC responses from blocking the trading pipeline.
pub async fn run_pre_buy_checks_with_timeout(
    signal: &Signal,
    config: &RiskGateConfig,
) -> RiskGateVerdict {
    if config.pre_buy_checks_timeout_ms == 0 {
        return run_pre_buy_checks(signal, config).await;
    }

    match tokio::time::timeout(
        Duration::from_millis(config.pre_buy_checks_timeout_ms),
        run_pre_buy_checks(signal, config),
    )
    .await
    {
        Ok(verdict) => verdict,
        Err(_) => {
            let elapsed = Duration::from_millis(config.pre_buy_checks_timeout_ms);
            tracing::warn!(
                mint = %signal.mint,
                pool = %signal.pool_address,
                "Pre-buy checks timed out after {}ms - rejecting signal",
                config.pre_buy_checks_timeout_ms
            );
            RiskGateVerdict::Reject {
                reason: format!("Pre-buy checks timeout after {}ms", elapsed.as_millis()),
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct RiskGateConfig {
    pub rpc_url: String,
    pub mint_freeze_timeout_ms: u64,
    pub lp_lock_timeout_ms: u64,
    pub token2022_timeout_ms: u64,
    pub simulate_sell_timeout_ms: u64,
    pub pre_buy_checks_timeout_ms: u64,
    pub simulate_size_lamports: u64,
    pub token2022_blocklist: Vec<String>,
    pub token2022_allowlist: Vec<String>,
    pub max_signal_age_ms: u64,
    pub cache: RiskGateCache,
}

impl Default for RiskGateConfig {
    fn default() -> Self {
        Self {
            rpc_url: "https://api.mainnet-beta.solana.com".to_string(),
            mint_freeze_timeout_ms: 10,     // Further optimized: 15 → 10
            lp_lock_timeout_ms: 15,         // Further optimized: 20 → 15
            token2022_timeout_ms: 10,       // Further optimized: 15 → 10
            simulate_sell_timeout_ms: 30,   // Further optimized: 50 → 30
            pre_buy_checks_timeout_ms: 300, // Temporary paper-trading default; TODO reduce to 150ms after batch RPC optimization
            simulate_size_lamports: 100_000_000, // Reduced: 1B → 100M (faster simulation)
            token2022_blocklist: vec![
                "transferFee".to_string(),
                "permanentDelegate".to_string(),
                "transferHook".to_string(),
            ],
            token2022_allowlist: vec![],
            max_signal_age_ms: 0,
            cache: RiskGateCache::new(10),
        }
    }
}
