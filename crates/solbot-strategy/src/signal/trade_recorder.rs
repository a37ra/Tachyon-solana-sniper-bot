//! Passive Pump.fun trade recorder for offline backtesting.
//!
//! Taps the already-open `logsSubscribe` stream (zero extra RPC credits),
//! decodes Pump.fun `CreateEvent` / `TradeEvent` from `Program data:` logs and
//! appends compact CSV rows to `logs/trades/YYYY-MM-DD-HH.csv`.
//!
//! Only trades of mints whose Create we observed (and only during the first
//! `SOLBOT_RECORD_WINDOW_SECS`, default 1200s, of their life) are recorded, to
//! keep disk usage bounded.
//!
//! Row formats:
//! `C,recv_ms,slot,mint,creator,0,0,0,0,0`
//! `T,recv_ms,slot,mint,user,is_buy,sol_lamports,token_amount,vsol,vtok`
//!
//! Enabled with `SOLBOT_RECORD_TRADES=true`. Never blocks the caller: rows are
//! handed to a background writer thread over an unbounded channel.

use base64::Engine;
use solana_pubkey::Pubkey;
use std::collections::HashMap;
use std::io::Write;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};

const CREATE_DISC: [u8; 8] = [27, 114, 169, 77, 222, 235, 99, 118];
const TRADE_DISC: [u8; 8] = [189, 219, 127, 211, 78, 230, 97, 238];

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
                        if !got_row || since_flush >= 500 {
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

/// Feed one raw `logsNotification` JSON text. Cheap no-op when disabled.
pub fn record_raw(text: &str) {
    let Some(rec) = recorder() else { return };
    if !text.contains("Program data: ") {
        return;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return };
    let res = &v["params"]["result"];
    let value = &res["value"];
    if !value["err"].is_null() {
        return;
    }
    let slot = res["context"]["slot"].as_u64().unwrap_or(0);
    let Some(logs) = value["logs"].as_array() else { return };
    let now_ms = chrono::Utc::now().timestamp_millis();

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
            let (Some(mint), Some(user)) = (rd_pk(&bytes, o), rd_pk(&bytes, o + 64)) else {
                continue;
            };
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
        } else if disc == TRADE_DISC {
            // mint, sol_amount, token_amount, is_buy, user, timestamp, vsol, vtok
            let Some(mint) = rd_pk(&bytes, 8) else { continue };
            let tracked = rec
                .born
                .lock()
                .ok()
                .and_then(|b| b.get(&mint).copied())
                .map(|t| now_ms - t < rec.window_ms)
                .unwrap_or(false);
            if !tracked {
                continue;
            }
            let (Some(sol), Some(tok)) = (rd_u64(&bytes, 40), rd_u64(&bytes, 48)) else {
                continue;
            };
            let is_buy = bytes.get(56).copied().unwrap_or(0) != 0;
            let Some(user) = rd_pk(&bytes, 57) else { continue };
            let (Some(vsol), Some(vtok)) = (rd_u64(&bytes, 97), rd_u64(&bytes, 105)) else {
                continue;
            };
            let _ = rec.tx.send(format!(
                "T,{now_ms},{slot},{mint},{user},{},{sol},{tok},{vsol},{vtok}\n",
                is_buy as u8
            ));
        }
    }
}
