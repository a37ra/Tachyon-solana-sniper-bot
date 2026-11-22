//! Passive Pump.fun trade recorder + real-time S4 anti-sybil crowd-breakout flow tracker.
//!
//! Taps the already-open `logsSubscribe` stream (zero extra RPC credits, and zero Helius
//! credits when `HELIUS_WS_URL=wss://api.mainnet-beta.solana.com`), decodes Pump.fun
//! `CreateEvent` / `TradeEvent` from `Program data:` logs, appends compact CSV rows to
//! `logs/trades/YYYY-MM-DD-HH.csv` when `SOLBOT_RECORD_TRADES=true`, and tracks per-mint
//! anti-sybil crowd flow in memory:
//! - `slot0_sol`: max real SOL accumulated in `slot <= birth_slot + 1` (Jito creation bundle)
//! - `organic_sol = real_sol - slot0_sol`: genuine post-creation SOL inflow
//! - `organic_buyers`: distinct wallets buying `>= 0.05 SOL` AFTER `slot > birth_slot + 1`
//!   (filters out both creation-slot bundle sybils and `0.01 SOL` dust-spam bots)
//! - `organic_sell_ratio = organic_sells / organic_buys`
//! - `creator_sold` flag & repeat creator session launch count
//! - pure standard curve invariant (`k = 30e9 * 1.073e15`, no Mayhem `BwWK17cb...` virtual-reserve mutation)

use base64::Engine;
use dashmap::DashMap;
use solana_pubkey::Pubkey;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::str::FromStr;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};

const CREATE_DISC: [u8; 8] = [27, 114, 169, 77, 222, 235, 99, 118];
const TRADE_DISC: [u8; 8] = [189, 219, 127, 211, 78, 230, 97, 238];
const MAYHEM_BOT_STR: &str = "BwWK17cbHxwWBKZkUYvzxLcNQ1YVyaFezduWbtm2de6s";
const INIT_VSOL_LAMPORTS: u64 = 30_000_000_000;
const K0_INVARIANT: f64 = 30_000_000_000.0 * 1_073_000_000_000_000.0;
const MIN_ORGANIC_BUY_LAMPORTS: u64 = 50_000_000; // 0.05 SOL minimum to count as a real buyer (ignores 0.01 SOL dust bots)

#[derive(Debug, Clone)]
pub struct MintFlowSnapshot {
    pub mint: String,
    pub pool: String,
    pub creator: Pubkey,
    pub creator_launch_num: u32,
    pub age_secs: f64,
    pub unique_buyers: usize,
    pub organic_buyers: usize,
    pub n_buys: u32,
    pub n_sells: u32,
    pub sell_ratio: f64,
    pub organic_sell_ratio: f64,
    pub creator_sold: bool,
    pub mayhem_or_nonstd: bool,
    pub slot0_sol: f64,
    pub organic_sol: f64,
    pub max_single_org_buy_sol: f64,
    pub max_wallet_cum_sol: f64,
    pub max_slot_inflow_sol: f64,
    pub real_sol: f64,
    pub peak_real_sol: f64,
    pub drawdown_pct: f64,
    pub inflow_win_sol: f64,
    pub latest_vsol: u64,
    pub latest_vtok: u64,
    pub spot_price_sol: f64,
    pub last_slot: u64,
    pub last_update_ms: i64,
    pub breakout_triggered: bool,
}

struct MintFlowState {
    mint: String,
    pool: String,
    creator: Pubkey,
    creator_launch_num: u32,
    born_ms: i64,
    birth_slot: u64,
    last_slot: u64,
    last_update_ms: i64,
    seen_reserves: HashSet<(u64, u64)>,
    buyers: HashSet<Pubkey>,
    organic_buyers: HashSet<Pubkey>,
    n_buys: u32,
    n_sells: u32,
    organic_buys: u32,
    organic_sells: u32,
    creator_sold: bool,
    mayhem_or_nonstd: bool,
    slot0_sol: f64,
    max_single_org_buy_sol: f64,
    wallet_cum_sol: HashMap<Pubkey, f64>,
    max_wallet_cum_sol: f64,
    cur_inflow_slot: u64,
    cur_slot_inflow_sol: f64,
    max_slot_inflow_sol: f64,
    peak_real_sol: f64,
    latest_real_sol: f64,
    latest_vsol: u64,
    latest_vtok: u64,
    flows: VecDeque<(i64, i64)>,
    signal_emitted: bool,
}

struct FlowTracker {
    mints: DashMap<Pubkey, MintFlowState>,
    by_str: DashMap<String, MintFlowSnapshot>,
    creator_launches: DashMap<Pubkey, u32>,
    ready_signals: Mutex<Vec<MintFlowSnapshot>>,
    mayhem_pk: Option<Pubkey>,
}

static FLOW_TRACKER: OnceLock<FlowTracker> = OnceLock::new();

fn flow_tracker() -> &'static FlowTracker {
    FLOW_TRACKER.get_or_init(|| FlowTracker {
        mints: DashMap::new(),
        by_str: DashMap::new(),
        creator_launches: DashMap::new(),
        ready_signals: Mutex::new(Vec::new()),
        mayhem_pk: Pubkey::from_str(MAYHEM_BOT_STR).ok(),
    })
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(default)
}

pub fn ws_breakout_trigger_enabled() -> bool {
    env_bool("SOLBOT_WS_BREAKOUT_TRIGGER", true)
}

pub fn get_mint_flow_snapshot(mint: &str) -> Option<MintFlowSnapshot> {
    flow_tracker().by_str.get(mint).map(|r| r.value().clone())
}

pub fn take_ready_breakout_signals() -> Vec<MintFlowSnapshot> {
    if let Ok(mut q) = flow_tracker().ready_signals.lock() {
        std::mem::take(&mut *q)
    } else {
        Vec::new()
    }
}

struct Recorder {
    tx: Sender<String>,
    born: Mutex<HashMap<Pubkey, i64>>,
    window_ms: i64,
}

static RECORDER: OnceLock<Option<Recorder>> = OnceLock::new();

fn recorder() -> Option<&'static Recorder> {
    RECORDER
        .get_or_init(|| {
            let enabled = std::env::var("SOLBOT_RECORD_TRADES")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if !enabled {
                return None;
            }
            let dir = std::env::var("SOLBOT_RECORD_DIR").unwrap_or_else(|_| "logs/trades".into());
            let window_ms = std::env::var("SOLBOT_RECORD_WINDOW_SECS")
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(1200)
                * 1000;
            let _ = std::fs::create_dir_all(&dir);
            let (tx, rx) = channel::<String>();
            std::thread::Builder::new()
                .name("trade-recorder".into())
                .spawn(move || {
                    let mut cur_name = String::new();
                    let mut file: Option<std::io::BufWriter<std::fs::File>> = None;
                    let mut since_flush = 0usize;
                    let mut last_flush = std::time::Instant::now();
                    loop {
                        let row = match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                            Ok(r) => Some(r),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                            Err(_) => break,
                        };
                        let got_row = row.is_some();
                        if let Some(row) = row {
                            let name = chrono::Utc::now().format("%Y-%m-%d-%H.csv").to_string();
                            if name != cur_name {
                                if let Some(mut f) = file.take() {
                                    let _ = f.flush();
                                }
                                let path = std::path::Path::new(&dir).join(&name);
                                file = std::fs::OpenOptions::new()
                                    .create(true)
                                    .append(true)
                                    .open(path)
                                    .ok()
                                    .map(|f| std::io::BufWriter::with_capacity(1 << 16, f));
                                cur_name = name;
                            }
                            if let Some(f) = file.as_mut() {
                                let _ = f.write_all(row.as_bytes());
                                since_flush += 1;
                            }
                        }
                        if !got_row || since_flush >= 500 || last_flush.elapsed().as_secs() >= 2 {
                            last_flush = std::time::Instant::now();
                            if let Some(f) = file.as_mut() {
                                let _ = f.flush();
                            }
                            since_flush = 0;
                        }
                    }
                })
                .ok()?;
            tracing::info!(window_ms, "Pump.fun trade recorder enabled");
            Some(Recorder {
                tx,
                born: Mutex::new(HashMap::new()),
                window_ms,
            })
        })
        .as_ref()
}

fn rd_u64(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

fn rd_pk(b: &[u8], o: usize) -> Option<Pubkey> {
    Pubkey::try_from(b.get(o..o + 32)?).ok()
}

fn skip_str(b: &[u8], o: usize) -> Option<usize> {
    let len = u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?) as usize;
    if b.len() < o + 4 + len {
        return None;
    }
    Some(o + 4 + len)
}

/// Feed one raw `logsNotification` JSON text. Updates in-memory S4 flow tracker and CSV recorder.
pub fn record_raw(text: &str) {
    if !text.contains("Program data: ") {
        return;
    }
    let rec_opt = recorder();
    let ft = flow_tracker();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return };
    let res = &v["params"]["result"];
    let value = &res["value"];
    if !value["err"].is_null() {
        return;
    }
    let slot = res["context"]["slot"].as_u64().unwrap_or(0);
    let Some(logs) = value["logs"].as_array() else { return };
    let now_ms = chrono::Utc::now().timestamp_millis();

    let min_age_secs = env_f64("SOLBOT_MIN_POOL_AGE_SECS", 5.0);
    let max_age_secs = env_f64("SOLBOT_MAX_POOL_AGE_SECS", 90.0);
    let min_pool_sol = env_f64("SOLBOT_MIN_POOL_SOL", 24.0);
    let max_pool_sol = env_f64("SOLBOT_MAX_POOL_SOL", 40.0);
    let max_slot0_sol = env_f64("SOLBOT_MAX_INITIAL_POOL_SOL", 19.0);
    let min_organic_sol = env_f64("SOLBOT_MIN_ORGANIC_SOL", 9.0);
    let min_unique_buyers = env_usize("SOLBOT_MIN_UNIQUE_BUYERS", 18);
    let inflow_win_ms = (env_f64("SOLBOT_INFLOW_WIN_SECS", 10.0) * 1000.0) as i64;
    let min_inflow_sol = env_f64("SOLBOT_MIN_INFLOW_SOL", 1.5);
    let max_dd_pct = env_f64("SOLBOT_MAX_CONFIRM_DROP_PCT", 0.08);
    let max_sell_ratio = env_f64("SOLBOT_MAX_SELL_RATIO", 0.30);
    let no_creator_sell = env_bool("SOLBOT_NO_CREATOR_SELL", true);
    let max_single_buy_sol = env_f64("SOLBOT_MAX_SINGLE_BUY_SOL", 0.0);
    let max_wallet_cum_limit = env_f64("SOLBOT_MAX_WALLET_CUM_SOL", 0.0);
    let max_slot_inflow_limit = env_f64("SOLBOT_MAX_SLOT_INFLOW_SOL", 0.0);

    for l in logs {
        let Some(s) = l.as_str() else { continue };
        let Some(b64) = s.strip_prefix("Program data: ") else { continue };
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) else {
            continue;
        };
        if bytes.len() < 8 {
            continue;
        }
        let disc: [u8; 8] = bytes[0..8].try_into().unwrap();
        if disc == CREATE_DISC {
            // name, symbol, uri, mint, bonding_curve, user
            let mut o = 8;
            let mut ok = true;
            for _ in 0..3 {
                match skip_str(&bytes, o) {
                    Some(n) => o = n,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            let (Some(mint), Some(pool), Some(user)) =
                (rd_pk(&bytes, o), rd_pk(&bytes, o + 32), rd_pk(&bytes, o + 64))
            else {
                continue;
            };

            if ft.mints.contains_key(&mint) {
                continue;
            }

            let launch_num = {
                let mut entry = ft.creator_launches.entry(user).or_insert(0);
                *entry += 1;
                *entry
            };

            let mint_str = mint.to_string();
            let pool_str = pool.to_string();
            ft.mints.insert(
                mint,
                MintFlowState {
                    mint: mint_str,
                    pool: pool_str,
                    creator: user,
                    creator_launch_num: launch_num,
                    born_ms: now_ms,
                    birth_slot: slot,
                    last_slot: slot,
                    last_update_ms: now_ms,
                    seen_reserves: HashSet::new(),
                    buyers: HashSet::new(),
                    organic_buyers: HashSet::new(),
                    n_buys: 0,
                    n_sells: 0,
                    organic_buys: 0,
                    organic_sells: 0,
                    creator_sold: false,
                    mayhem_or_nonstd: false,
                    slot0_sol: 0.0,
                    max_single_org_buy_sol: 0.0,
                    wallet_cum_sol: HashMap::new(),
                    max_wallet_cum_sol: 0.0,
                    cur_inflow_slot: slot,
                    cur_slot_inflow_sol: 0.0,
                    max_slot_inflow_sol: 0.0,
                    peak_real_sol: 0.0,
                    latest_real_sol: 0.0,
                    latest_vsol: INIT_VSOL_LAMPORTS,
                    latest_vtok: 1_073_000_000_000_000,
                    flows: VecDeque::new(),
                    signal_emitted: false,
                },
            );
            if ft.mints.len() > 8_000 {
                ft.mints.retain(|_, st| now_ms - st.born_ms < 300_000);
                ft.by_str.retain(|_, sn| now_ms - sn.last_update_ms < 300_000);
            }

            if let Some(rec) = rec_opt {
                if let Ok(mut born) = rec.born.lock() {
                    born.insert(mint, now_ms);
                    if born.len() > 20_000 {
                        let w = rec.window_ms;
                        born.retain(|_, t| now_ms - *t < w);
                    }
                }
                let _ = rec
                    .tx
                    .send(format!("C,{now_ms},{slot},{mint},{user},0,0,0,0,0\n"));
            }
        } else if disc == TRADE_DISC {
            // mint, sol_amount, token_amount, is_buy, user, timestamp, vsol, vtok[, rsol, rtok]
            let Some(mint) = rd_pk(&bytes, 8) else { continue };
            let (Some(sol), Some(tok)) = (rd_u64(&bytes, 40), rd_u64(&bytes, 48)) else {
                continue;
            };
            let is_buy = bytes.get(56).copied().unwrap_or(0) != 0;
            let Some(user) = rd_pk(&bytes, 57) else { continue };
            let (Some(vsol), Some(vtok)) = (rd_u64(&bytes, 97), rd_u64(&bytes, 105)) else {
                continue;
            };

            if let Some(mut st) = ft.mints.get_mut(&mint) {
                // Reject out-of-order stale slots and multi-node batch echoes from public Solana WS cluster
                if slot < st.last_slot || !st.seen_reserves.insert((vsol, vtok)) {
                    continue;
                }
                let age_secs = ((now_ms - st.born_ms).max(0) as f64) / 1000.0;
                st.last_slot = slot;
                st.last_update_ms = now_ms;
                st.latest_vsol = vsol;
                st.latest_vtok = vtok;

                let k_now = (vsol as f64) * (vtok as f64);
                let k_dev = (k_now - K0_INVARIANT).abs() / K0_INVARIANT;
                if ft.mayhem_pk == Some(user) || k_dev > 0.005 || vsol < INIT_VSOL_LAMPORTS {
                    st.mayhem_or_nonstd = true;
                }

                let real_sol = (vsol.saturating_sub(INIT_VSOL_LAMPORTS) as f64) / 1_000_000_000.0;
                st.latest_real_sol = real_sol;
                if real_sol > st.peak_real_sol {
                    st.peak_real_sol = real_sol;
                }

                let is_bundle_slot = slot <= st.birth_slot.saturating_add(1);
                if is_bundle_slot {
                    if real_sol > st.slot0_sol {
                        st.slot0_sol = real_sol;
                    }
                    if !is_buy && user == st.creator {
                        st.creator_sold = true;
                    }
                } else {
                    let sol_f = (sol as f64) / 1_000_000_000.0;
                    if is_buy {
                        if sol >= MIN_ORGANIC_BUY_LAMPORTS {
                            st.organic_buyers.insert(user);
                            st.organic_buys += 1;
                        }
                        if sol_f > st.max_single_org_buy_sol {
                            st.max_single_org_buy_sol = sol_f;
                        }
                        let cum = st.wallet_cum_sol.entry(user).or_insert(0.0);
                        *cum += sol_f;
                        if *cum > st.max_wallet_cum_sol {
                            st.max_wallet_cum_sol = *cum;
                        }
                        if slot == st.cur_inflow_slot {
                            st.cur_slot_inflow_sol += sol_f;
                        } else {
                            st.cur_inflow_slot = slot;
                            st.cur_slot_inflow_sol = sol_f;
                        }
                        if st.cur_slot_inflow_sol > st.max_slot_inflow_sol {
                            st.max_slot_inflow_sol = st.cur_slot_inflow_sol;
                        }
                    } else {
                        st.organic_sells += 1;
                        if user == st.creator {
                            st.creator_sold = true;
                        }
                    }
                    let signed = if is_buy { sol as i64 } else { -(sol as i64) };
                    st.flows.push_back((now_ms, signed));
                }

                if is_buy {
                    st.n_buys += 1;
                    st.buyers.insert(user);
                } else {
                    st.n_sells += 1;
                }

                while let Some(&(t0, _)) = st.flows.front() {
                    if t0 < now_ms - inflow_win_ms {
                        st.flows.pop_front();
                    } else {
                        break;
                    }
                }
                let inflow_win_sol =
                    st.flows.iter().map(|&(_, s)| s as f64).sum::<f64>() / 1_000_000_000.0;
                let organic_sol = (real_sol - st.slot0_sol).max(0.0);
                let drawdown_pct = if st.peak_real_sol > 0.0 {
                    (st.peak_real_sol - real_sol) / st.peak_real_sol
                } else {
                    0.0
                };
                let sell_ratio = if st.n_buys > 0 {
                    (st.n_sells as f64) / (st.n_buys as f64)
                } else {
                    999.0
                };
                let organic_sell_ratio = if st.organic_buys > 0 {
                    (st.organic_sells as f64) / (st.organic_buys as f64)
                } else {
                    999.0
                };
                let unique_buyers = st.buyers.len();
                let organic_buyers = st.organic_buyers.len();
                let spot_price_sol = if vtok > 0 {
                    (vsol as f64 / 1_000_000_000.0) / (vtok as f64 / 1_000_000.0)
                } else {
                    0.0
                };

                let anti_whale_ok = (max_single_buy_sol <= 0.0
                    || st.max_single_org_buy_sol <= max_single_buy_sol)
                    && (max_wallet_cum_limit <= 0.0
                        || st.max_wallet_cum_sol <= max_wallet_cum_limit)
                    && (max_slot_inflow_limit <= 0.0
                        || st.max_slot_inflow_sol <= max_slot_inflow_limit);

                let will_emit = !st.signal_emitted
                    && !st.mayhem_or_nonstd
                    && st.creator_launch_num == 1
                    && !(no_creator_sell && st.creator_sold)
                    && !is_bundle_slot
                    && st.slot0_sol <= max_slot0_sol
                    && organic_sol >= min_organic_sol
                    && anti_whale_ok
                    && age_secs >= min_age_secs
                    && age_secs <= max_age_secs
                    && real_sol >= min_pool_sol
                    && real_sol <= max_pool_sol
                    && organic_buyers >= min_unique_buyers
                    && inflow_win_sol >= min_inflow_sol
                    && drawdown_pct <= max_dd_pct
                    && organic_sell_ratio <= max_sell_ratio;

                if will_emit {
                    st.signal_emitted = true;
                }

                let snap = MintFlowSnapshot {
                    mint: st.mint.clone(),
                    pool: st.pool.clone(),
                    creator: st.creator,
                    creator_launch_num: st.creator_launch_num,
                    age_secs,
                    unique_buyers,
                    organic_buyers,
                    n_buys: st.n_buys,
                    n_sells: st.n_sells,
                    sell_ratio,
                    organic_sell_ratio,
                    creator_sold: st.creator_sold,
                    mayhem_or_nonstd: st.mayhem_or_nonstd,
                    slot0_sol: st.slot0_sol,
                    organic_sol,
                    max_single_org_buy_sol: st.max_single_org_buy_sol,
                    max_wallet_cum_sol: st.max_wallet_cum_sol,
                    max_slot_inflow_sol: st.max_slot_inflow_sol,
                    real_sol,
                    peak_real_sol: st.peak_real_sol,
                    drawdown_pct,
                    inflow_win_sol,
                    latest_vsol: vsol,
                    latest_vtok: vtok,
                    spot_price_sol,
                    last_slot: slot,
                    last_update_ms: now_ms,
                    breakout_triggered: st.signal_emitted,
                };
                ft.by_str.insert(st.mint.clone(), snap.clone());

                if will_emit {
                    if let Ok(mut q) = ft.ready_signals.lock() {
                        q.push(snap);
                    }
                }
            }

            if let Some(rec) = rec_opt {
                let tracked = rec
                    .born
                    .lock()
                    .ok()
                    .and_then(|b| b.get(&mint).copied())
                    .map(|t| now_ms - t < rec.window_ms)
                    .unwrap_or(false);
                if tracked {
                    let _ = rec.tx.send(format!(
                        "T,{now_ms},{slot},{mint},{user},{},{sol},{tok},{vsol},{vtok}\n",
                        is_buy as u8
                    ));
                }
            }
        }
    }
}
