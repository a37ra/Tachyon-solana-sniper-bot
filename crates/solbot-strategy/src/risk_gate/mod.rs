pub mod lp_lock_check;
pub mod mint_freeze_authority;
pub mod simulate_sell;
pub mod token2022_extensions;

use dashmap::DashMap;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solbot_common::types::Signal;
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Request coalescing for concurrent pre-buy fetches of the same pool.
static FETCH_COALESCER: LazyLock<DashMap<String, Arc<FetchState>>> =
    LazyLock::new(DashMap::new);

struct FetchState {
    notify: Arc<Notify>,
    /// None = in progress, Some = finished (Ok or Err string).
    result: std::sync::Mutex<Option<Result<PreBuyCheckContext, String>>>,
    /// Only the first waiter performs the fetch.
    claimed: std::sync::atomic::AtomicBool,
}

fn rpc_timeout_ms() -> u64 {
    std::env::var("SOLBOT_PRE_BUY_RPC_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            // Public RPC / Helius free: single call often 200-800ms under load.
            if is_public_rpc_profile() {
                2_000
            } else {
                300
            }
        })
}

fn is_public_rpc_profile() -> bool {
    std::env::var("SOLBOT_PUBLIC_RPC")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true)
}

/// Resolve primary RPC URL (Helius HTTP preferred over public mainnet).
pub fn resolve_rpc_url() -> String {
    if let Ok(url) = std::env::var("SOLBOT_RPC_URL") {
        if !url.is_empty() {
            return url;
        }
    }
    if let Ok(key) = std::env::var("HELIUS_API_KEY").or_else(|_| std::env::var("GEYSER_X_TOKEN")) {
        if !key.is_empty() && key != "YOUR_API_KEY" {
            return format!("https://mainnet.helius-rpc.com/?api-key={}", key);
        }
    }
    if let Ok(url) = std::env::var("RPC_URL_PRIMARY") {
        if !url.is_empty() {
            return url;
        }
    }
    "https://api.mainnet-beta.solana.com".to_string()
}

/// Context holding pre-fetched account data to avoid redundant RPC calls.
#[derive(Debug, Clone)]
pub struct PreBuyCheckContext {
    pub mint_data: Option<Vec<u8>>,
    pub mint_owner: Option<Pubkey>,
    pub pool_data: Option<Vec<u8>>,
    pub pool_owner: Option<Pubkey>,
    pub lp_mint_data: Option<Vec<u8>>,
    pub lp_mint_owner: Option<Pubkey>,
    pub pool_data_from_ws: bool,
}

impl PreBuyCheckContext {
    pub async fn fetch(
        rpc: &Arc<RpcClient>,
        mint: &str,
        pool_address: &str,
        known_lp_mint: Option<Pubkey>,
    ) -> anyhow::Result<Self> {
        let mint_pubkey = Pubkey::try_from(mint)?;
        let pool_pubkey = Pubkey::try_from(pool_address)?;

        let cache_key = format!("{}:{}", mint, pool_address);
        let state = FETCH_COALESCER
            .entry(cache_key.clone())
            .or_insert_with(|| {
                Arc::new(FetchState {
                    notify: Arc::new(Notify::new()),
                    result: std::sync::Mutex::new(None),
                    claimed: std::sync::atomic::AtomicBool::new(false),
                })
            })
            .clone();

        {
            let guard = state.result.lock().unwrap();
            if let Some(ref result) = *guard {
                return result.clone().map_err(|e| anyhow::anyhow!("{}", e));
            }
        }

        let i_am_fetcher = state
            .claimed
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok();

        if i_am_fetcher {
            let result = Self::fetch_internal(
                rpc,
                &mint_pubkey,
                &pool_pubkey,
                mint,
                pool_address,
                known_lp_mint,
            )
            .await;
            let stored = result
                .as_ref()
                .map(|ctx| ctx.clone())
                .map_err(|e| e.to_string());
            {
                let mut guard = state.result.lock().unwrap();
                *guard = Some(stored);
            }
            state.notify.notify_waiters();

            let key = cache_key.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                FETCH_COALESCER.remove(&key);
            });

            result
        } else {
            loop {
                {
                    let guard = state.result.lock().unwrap();
                    if let Some(ref result) = *guard {
                        return result.clone().map_err(|e| anyhow::anyhow!("{}", e));
                    }
                }
                let notified = state.notify.notified();
                tokio::pin!(notified);
                match tokio::time::timeout(Duration::from_secs(5), &mut notified).await {
                    Ok(()) => continue,
                    Err(_) => {
                        return Err(anyhow::anyhow!(
                            "Timed out waiting for coalesced pre-buy fetch"
                        ));
                    }
                }
            }
        }
    }

    async fn fetch_internal(
        rpc: &Arc<RpcClient>,
        mint_pubkey: &Pubkey,
        pool_pubkey: &Pubkey,
        mint: &str,
        pool_address: &str,
        known_lp_mint: Option<Pubkey>,
    ) -> anyhow::Result<Self> {
        let to = Duration::from_millis(rpc_timeout_ms());

        if let Some(lp_pubkey) = known_lp_mint {
            let accounts = match tokio::time::timeout(
                to,
                rpc.get_multiple_accounts(&[*mint_pubkey, *pool_pubkey, lp_pubkey]),
            )
            .await
            {
                Ok(result) => result?,
                Err(_) => {
                    tracing::warn!(
                        mint = %mint,
                        pool = %pool_address,
                        "get_multiple_accounts timed out after {}ms",
                        rpc_timeout_ms()
                    );
                    return Err(anyhow::anyhow!(
                        "Pre-buy check RPC timed out after {}ms",
                        rpc_timeout_ms()
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

        // Fresh create-ix signals often race the RPC indexer: mint may be null for
        // a few hundred ms. Retry mint+pool fetch before treating as unavailable.
        let (mint_data, mint_owner, pool_data, pool_owner) =
            fetch_mint_pool_with_retry(rpc, mint_pubkey, pool_pubkey, mint, pool_address, to)
                .await?;

        let (lp_mint_data, lp_mint_owner) = match (&pool_data, &pool_owner) {
            (Some(p_data), Some(p_owner)) => {
                if let Some(lp_pk) = extract_lp_mint_from_pool(p_data, p_owner) {
                    match tokio::time::timeout(to, rpc.get_account(&lp_pk)).await {
                        Ok(Ok(acc)) => (Some(acc.data), Some(acc.owner)),
                        Ok(Err(e)) => {
                            tracing::warn!(lp_mint = %lp_pk, error = %e, "lp_mint fetch failed");
                            (None, None)
                        }
                        Err(_) => {
                            tracing::warn!(
                                lp_mint = %lp_pk,
                                "get_account(lp_mint) timed out — continuing without LP data"
                            );
                            (None, None)
                        }
                    }
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

    /// WS pool bytes present → 0 RTT for pool, 1 RTT for mint (+ lp if extractable).
    pub async fn from_ws_update(
        rpc: &Arc<RpcClient>,
        mint: &str,
        pool_data: Vec<u8>,
        pool_owner: Pubkey,
    ) -> anyhow::Result<Self> {
        let mint_pubkey = Pubkey::try_from(mint)?;
        let maybe_lp_mint = extract_lp_mint_from_pool(&pool_data, &pool_owner);
        let to = Duration::from_millis(rpc_timeout_ms());

        let (mint_data, mint_owner, lp_mint_data, lp_mint_owner) = match maybe_lp_mint {
            Some(lp_pk) => {
                let accounts = match tokio::time::timeout(
                    to,
                    rpc.get_multiple_accounts(&[mint_pubkey, lp_pk]),
                )
                .await
                {
                    Ok(result) => result?,
                    Err(_) => {
                        return Err(anyhow::anyhow!(
                            "Pre-buy check RPC timed out after {}ms",
                            rpc_timeout_ms()
                        ));
                    }
                };
                let (m_data, m_owner) = unpack_acc(accounts.get(0));
                let (lp_data, lp_owner) = unpack_acc(accounts.get(1));
                (m_data, m_owner, lp_data, lp_owner)
            }
            None => {
                let acc = match tokio::time::timeout(to, rpc.get_account(&mint_pubkey)).await {
                    Ok(result) => result?,
                    Err(_) => {
                        return Err(anyhow::anyhow!(
                            "Pre-buy check RPC timed out after {}ms",
                            rpc_timeout_ms()
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

/// Retry account fetch when mint is missing (indexing lag on brand-new create-ix).
///
/// Verified: post-hoc getAccountInfo on mint_unavailable rejects finds the account —
/// so this is lag, not a bad mint. Uses multi + single get_account fallback.
async fn fetch_mint_pool_with_retry(
    rpc: &Arc<RpcClient>,
    mint_pubkey: &Pubkey,
    pool_pubkey: &Pubkey,
    mint: &str,
    pool_address: &str,
    to: Duration,
) -> anyhow::Result<(
    Option<Vec<u8>>,
    Option<Pubkey>,
    Option<Vec<u8>>,
    Option<Pubkey>,
)> {
    // Cumulative waits ~0+0.2+0.5+1.0+1.5 ≈ 3.2s sleep + RPC RTTs (within public pre_buy 4s).
    const BACKOFFS_MS: &[u64] = &[0, 200, 500, 1000, 1500];
    let mut last_pool: (Option<Vec<u8>>, Option<Pubkey>) = (None, None);

    for (i, wait_ms) in BACKOFFS_MS.iter().enumerate() {
        if *wait_ms > 0 {
            tokio::time::sleep(Duration::from_millis(*wait_ms)).await;
        }

        // Prefer single get_account for mint first (cheaper, clearer null semantics).
        let mint_res = tokio::time::timeout(to, rpc.get_account(mint_pubkey)).await;
        let pool_res = tokio::time::timeout(to, rpc.get_account(pool_pubkey)).await;

        let (mint_data, mint_owner) = match mint_res {
            Ok(Ok(acc)) => (Some(acc.data), Some(acc.owner)),
            Ok(Err(_)) => (None, None), // AccountNotFound → retry
            Err(_) => {
                tracing::warn!(mint = %mint, attempt = i, "get_account(mint) timed out");
                (None, None)
            }
        };
        let (pool_data, pool_owner) = match pool_res {
            Ok(Ok(acc)) => (Some(acc.data), Some(acc.owner)),
            Ok(Err(_)) => (None, None),
            Err(_) => (None, None),
        };
        if pool_data.is_some() {
            last_pool = (pool_data.clone(), pool_owner);
        }

        if mint_data.is_some() {
            if i > 0 {
                tracing::info!(
                    mint = %mint,
                    attempt = i,
                    waited_ms = wait_ms,
                    "mint account available after indexer retry"
                );
            }
            return Ok((
                mint_data,
                mint_owner,
                pool_data.or(last_pool.0.clone()),
                pool_owner.or(last_pool.1),
            ));
        }

        tracing::info!(
            mint = %mint,
            pool = %pool_address,
            attempt = i,
            waited_ms = wait_ms,
            "mint account not yet indexed — retry"
        );
    }

    tracing::warn!(
        mint = %mint,
        pool = %pool_address,
        "mint still unavailable after indexer retries"
    );
    Ok((None, None, last_pool.0, last_pool.1))
}

pub fn extract_lp_mint_from_pool(pool_data: &[u8], pool_owner: &Pubkey) -> Option<Pubkey> {
    const RAYDIUM_V4: Pubkey =
        solana_pubkey::pubkey!("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
    const RAYDIUM_CPMM: Pubkey =
        solana_pubkey::pubkey!("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
    const METEORA_DYNAMIC: Pubkey =
        solana_pubkey::pubkey!("Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB");

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
            entry.inserted_at.elapsed() < self.0.ttl
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
                || lower.contains("failed to fetch")
                || lower.contains("task join error")
                || lower.contains("simulation error")
                || lower.contains("lp_lock:context_unavailable")
                || lower.contains("lp_lock:parse_failed")
                || lower.contains("coalesced"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn bare_config(skip_sim: bool, max_age: u64) -> RiskGateConfig {
        let rpc_url = "http://127.0.0.1:8899".to_string();
        RiskGateConfig {
            rpc: Arc::new(RpcClient::new(rpc_url.clone())),
            rpc_url,
            mint_freeze_timeout_ms: 10,
            lp_lock_timeout_ms: 15,
            token2022_timeout_ms: 10,
            simulate_sell_timeout_ms: 150,
            pre_buy_checks_timeout_ms: 600,
            simulate_size_lamports: 100_000_000,
            token2022_blocklist: vec![],
            token2022_allowlist: vec![],
            max_signal_age_ms: max_age,
            skip_simulate_sell: skip_sim,
            cache: RiskGateCache::new(10),
        }
    }

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

    #[test]
    fn resolve_rpc_url_non_empty() {
        assert!(!resolve_rpc_url().is_empty());
    }

    /// Live hard-guard: public RPC profile must abort (not WARN-only).
    #[test]
    fn assert_safe_for_live_rejects_public_rpc() {
        let cfg = bare_config(false, 500);
        let err = cfg
            .assert_safe_for_live_with(true)
            .expect_err("public_rpc=true must refuse live");
        let msg = err.to_string();
        assert!(
            msg.contains("SOLBOT_PUBLIC_RPC=true"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn assert_safe_for_live_rejects_skip_simulate() {
        let cfg = bare_config(true, 500);
        let err = cfg
            .assert_safe_for_live_with(false)
            .expect_err("skip_simulate must refuse live");
        assert!(err.to_string().contains("skip_simulate_sell") || err.to_string().contains("honeypot"));
    }

    #[test]
    fn assert_safe_for_live_rejects_zero_max_age() {
        let cfg = bare_config(false, 0);
        let err = cfg
            .assert_safe_for_live_with(false)
            .expect_err("max_signal_age_ms=0 must refuse live");
        assert!(err.to_string().contains("max_signal_age_ms=0"));
    }

    #[test]
    fn assert_safe_for_live_ok_when_strict() {
        let cfg = bare_config(false, 500);
        cfg.assert_safe_for_live_with(false)
            .expect("private RPC + sim on + age>0 must pass");
    }
}

fn compute_verdict(
    freeze_result: &CheckResult,
    lp_result: &CheckResult,
    token2022_result: &CheckResult,
    simulate_result: &CheckResult,
) -> RiskGateVerdict {
    if !freeze_result.passed {
        RiskGateVerdict::Reject {
            reason: freeze_result
                .reason
                .clone()
                .unwrap_or_else(|| "Mint freeze authority check failed".to_string()),
        }
    } else if !lp_result.passed {
        RiskGateVerdict::Reject {
            reason: lp_result
                .reason
                .clone()
                .unwrap_or_else(|| "LP lock check failed".to_string()),
        }
    } else if !token2022_result.passed {
        RiskGateVerdict::Reject {
            reason: token2022_result
                .reason
                .clone()
                .unwrap_or_else(|| "Token2022 extensions check failed".to_string()),
        }
    } else if !simulate_result.passed {
        RiskGateVerdict::Reject {
            reason: simulate_result
                .reason
                .clone()
                .unwrap_or_else(|| "Simulate sell check failed".to_string()),
        }
    } else {
        RiskGateVerdict::Accept
    }
}

pub async fn run_pre_buy_checks(signal: &Signal, config: &RiskGateConfig) -> RiskGateVerdict {
    let start = Instant::now();

    if config.max_signal_age_ms > 0 && signal.received_at > 0 {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        if now_ms >= signal.received_at {
            let age = now_ms - signal.received_at;
            if age > config.max_signal_age_ms {
                tracing::warn!(
                    mint = %signal.mint,
                    age_ms = age,
                    max_ms = config.max_signal_age_ms,
                    "Rejecting stale signal before RPC"
                );
                return RiskGateVerdict::Reject {
                    reason: format!("Stale signal: {}ms > {}ms", age, config.max_signal_age_ms),
                };
            }
        }
    }

    tracing::info!("Running pre-buy checks for mint: {}", signal.mint);

    let cache_key = signal.pool_address.clone();
    if let Some(cached_verdict) = config.cache.get(&cache_key) {
        tracing::debug!(pool = %signal.pool_address, "Using cached pre-buy risk gate verdict");
        return cached_verdict;
    }

    let rpc = config.rpc.clone();

    let context = match build_context(signal, &rpc).await {
        Ok(ctx) => ctx,
        Err(e) => {
            // Fail-closed WITHOUT spawning 4 parallel RPC storms (was the main
            // rate-limit / timeout amplifier on public RPC).
            tracing::warn!(
                mint = %signal.mint,
                error = %e,
                "Pre-buy context fetch failed — rejecting (no multi-RPC fallback)"
            );
            return RiskGateVerdict::Reject {
                reason: format!("Failed to fetch pre-buy check context: {}", e),
            };
        }
    };

    tracing::info!(
        mint = %signal.mint,
        pool = %signal.pool_address,
        pool_data_from_ws = %context.pool_data_from_ws,
        "Pre-buy context ready"
    );

    // Live trading must never skip honeypot simulation (defense-in-depth if config was cloned).
    let skip_sim = if config.skip_simulate_sell {
        if std::env::var("SOLBOT_PAPER_TRADING")
            .map(|v| v == "false" || v == "0")
            .unwrap_or(false)
            || std::env::args().any(|a| a == "--live")
        {
            tracing::error!(
                "skip_simulate_sell requested but live trading is active — forcing simulate_sell ON"
            );
            false
        } else {
            true
        }
    } else {
        false
    };

    let (freeze_result, lp_result, token2022_result, simulate_result) = if skip_sim {
        let (f, l, t) = tokio::join!(
            mint_freeze_authority::check_with_context(&signal.mint, &context),
            lp_lock_check::check_with_context(&signal.pool_address, &signal.dex_program, &context),
            token2022_extensions::check_with_context(&signal.mint, config, &context),
        );
        let sim = CheckResult {
            passed: true,
            reason: Some("simulate_sell skipped by config".into()),
            duration_ms: 0,
        };
        (f, l, t, sim)
    } else {
        tokio::join!(
            mint_freeze_authority::check_with_context(&signal.mint, &context),
            lp_lock_check::check_with_context(&signal.pool_address, &signal.dex_program, &context),
            token2022_extensions::check_with_context(&signal.mint, config, &context),
            simulate_sell::check_with_context(
                &signal.mint,
                &signal.pool_address,
                config.simulate_size_lamports,
                config.rpc.clone(),
                config.simulate_sell_timeout_ms,
                Some(&context),
            )
        )
    };

    let elapsed = start.elapsed();
    tracing::info!(
        mint = %signal.mint,
        freeze_ms = freeze_result.duration_ms,
        lp_ms = lp_result.duration_ms,
        token2022_ms = token2022_result.duration_ms,
        simulate_ms = simulate_result.duration_ms,
        total_ms = elapsed.as_millis() as u64,
        freeze_ok = freeze_result.passed,
        lp_ok = lp_result.passed,
        token2022_ok = token2022_result.passed,
        simulate_ok = simulate_result.passed,
        "Pre-buy checks completed"
    );

    let verdict = compute_verdict(
        &freeze_result,
        &lp_result,
        &token2022_result,
        &simulate_result,
    );

    if is_cacheable(&verdict) {
        config.cache.insert(cache_key, verdict.clone());
    }

    verdict
}

async fn build_context(signal: &Signal, rpc: &Arc<RpcClient>) -> anyhow::Result<PreBuyCheckContext> {
    if let (Some(pool_data), Some(pool_owner_str)) = (
        &signal.raw_pool_account_data,
        &signal.raw_pool_account_owner,
    ) {
        if let Ok(pool_owner_pk) = Pubkey::try_from(pool_owner_str.as_str()) {
            match PreBuyCheckContext::from_ws_update(
                rpc,
                &signal.mint,
                pool_data.clone(),
                pool_owner_pk,
            )
            .await
            {
                Ok(ctx) => return Ok(ctx),
                Err(e) => {
                    tracing::warn!(
                        "from_ws_update failed ({}), falling back to batch fetch",
                        e
                    );
                }
            }
        }
    }

    PreBuyCheckContext::fetch(rpc, &signal.mint, &signal.pool_address, None).await
}

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
            tracing::warn!(
                mint = %signal.mint,
                pool = %signal.pool_address,
                "Pre-buy checks timed out after {}ms - rejecting signal",
                config.pre_buy_checks_timeout_ms
            );
            RiskGateVerdict::Reject {
                reason: format!(
                    "Pre-buy checks timeout after {}ms",
                    config.pre_buy_checks_timeout_ms
                ),
            }
        }
    }
}

#[derive(Clone)]
pub struct RiskGateConfig {
    pub rpc: Arc<RpcClient>,
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
    pub skip_simulate_sell: bool,
    pub cache: RiskGateCache,
}

impl RiskGateConfig {
    /// Build config from env with a shared nonblocking RpcClient (keep-alive).
    ///
    /// Timeout defaults:
    /// - `SOLBOT_PUBLIC_RPC=true` (default): pre_buy=4000ms, simulate=2000ms — paper/debug only
    /// - `SOLBOT_PUBLIC_RPC=false`: pre_buy=600ms, simulate=150ms — private/co-located RPC
    pub fn from_shared_rpc(rpc: Arc<RpcClient>, rpc_url: String) -> Self {
        let public = is_public_rpc_profile();
        let skip_simulate_sell = std::env::var("SOLBOT_SKIP_SIMULATE_SELL")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        if skip_simulate_sell {
            tracing::error!(
                "SOLBOT_SKIP_SIMULATE_SELL is enabled — honeypot simulation DISABLED. \
                 Paper/debug only. Live start will refuse to boot."
            );
        }
        if public {
            tracing::warn!(
                "SOLBOT_PUBLIC_RPC profile active: pre_buy timeout default 4000ms, \
                 simulate 2000ms. Not suitable for competitive live sniping. \
                 Set SOLBOT_PUBLIC_RPC=false after switching to private RPC."
            );
        }

        Self {
            rpc,
            rpc_url,
            mint_freeze_timeout_ms: 10,
            lp_lock_timeout_ms: 15,
            token2022_timeout_ms: 10,
            simulate_sell_timeout_ms: env_u64(
                "SOLBOT_SIMULATE_SELL_TIMEOUT_MS",
                if public { 2_000 } else { 150 },
            ),
            pre_buy_checks_timeout_ms: env_u64(
                "SOLBOT_PRE_BUY_TIMEOUT_MS",
                // Public: budget for create-ix mint indexer retries (~3s sleep + RPC).
                if public { 6_000 } else { 600 },
            ),
            simulate_size_lamports: 100_000_000,
            token2022_blocklist: vec![
                "transferFee".to_string(),
                "permanentDelegate".to_string(),
                "transferHook".to_string(),
            ],
            token2022_allowlist: vec![],
            max_signal_age_ms: env_u64("SOLBOT_MAX_SIGNAL_AGE_MS", if public { 2_000 } else { 500 }),
            skip_simulate_sell,
            cache: RiskGateCache::new(30),
        }
    }

    pub fn from_env() -> Self {
        let url = resolve_rpc_url();
        let rpc = Arc::new(RpcClient::new(url.clone()));
        Self::from_shared_rpc(rpc, url)
    }

    /// Refuse live trading when honeypot simulation is disabled or public-RPC profile is on.
    /// Single entry-point for all live hard-guards (called from main before trading starts).
    pub fn assert_safe_for_live(&self) -> anyhow::Result<()> {
        self.assert_safe_for_live_with(is_public_rpc_profile())
    }

    /// Testable core of live hard-guards (public_rpc injected — no env race in unit tests).
    pub fn assert_safe_for_live_with(&self, public_rpc: bool) -> anyhow::Result<()> {
        if self.skip_simulate_sell {
            anyhow::bail!(
                "Refusing LIVE start: SOLBOT_SKIP_SIMULATE_SELL / skip_simulate_sell=true \
                 disables honeypot (simulate_sell) protection. Unset the flag for live trading."
            );
        }
        if public_rpc {
            anyhow::bail!(
                "Refusing LIVE start: SOLBOT_PUBLIC_RPC=true. Use a private/fast RPC and set \
                 SOLBOT_PUBLIC_RPC=false (live sniping with 4s budgets is unsafe)."
            );
        }
        if self.max_signal_age_ms == 0 {
            anyhow::bail!(
                "Refusing LIVE start: max_signal_age_ms=0 disables pre-submit freshness checks. \
                 Set SOLBOT_MAX_SIGNAL_AGE_MS (e.g. 500) for live."
            );
        }
        Ok(())
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

impl Default for RiskGateConfig {
    fn default() -> Self {
        Self::from_env()
    }
}
