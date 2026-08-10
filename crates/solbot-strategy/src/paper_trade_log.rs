//! Dedicated paper-trade journal (separate from tracing).
//!
//! Writes human-readable lines to `logs/paper_trades_YYYYMMDD.log` so you can
//! watch buys, simulated marks, and mock PnL without grepping the full bot log.

use chrono::Utc;
use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn log_path() -> PathBuf {
    let dir = std::env::var("SOLBOT_PAPER_TRADE_LOG_DIR").unwrap_or_else(|_| "logs".into());
    let day = Utc::now().format("%Y%m%d");
    PathBuf::from(dir).join(format!("paper_trades_{day}.log"))
}

fn write_line(line: &str) {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = create_dir_all(parent);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}", line);
        let _ = f.flush();
    }
    // Also mirror to stdout for live tail
    println!("{}", line);
}

pub fn log_accept(mint: &str, pool: &str, dex: &str, anti_rug_ms: f64) {
    write_line(&format!(
        "{} | ACCEPT | mint={} pool={} dex={} anti_rug_ms={:.1}",
        Utc::now().format("%Y-%m-%d %H:%M:%S"),
        mint,
        pool,
        dex,
        anti_rug_ms
    ));
}

pub fn log_reject(mint: &str, reason: &str) {
    write_line(&format!(
        "{} | REJECT | mint={} reason={}",
        Utc::now().format("%Y-%m-%d %H:%M:%S"),
        mint,
        reason.chars().take(120).collect::<String>()
    ));
}

pub fn log_paper_buy(
    mint: &str,
    pool: &str,
    dex: &str,
    size_sol: f64,
    entry_price: f64,
    position_id: &str,
) {
    write_line(&format!(
        "{} | PAPER_BUY | id={} mint={} pool={} dex={} size_sol={:.6} entry={:.10}",
        Utc::now().format("%Y-%m-%d %H:%M:%S"),
        position_id,
        mint,
        pool,
        dex,
        size_sol,
        entry_price
    ));
}

pub fn log_paper_sell(
    mint: &str,
    position_id: &str,
    size_sol: f64,
    entry_price: f64,
    exit_price: f64,
    pnl_sol: f64,
    pnl_pct: f64,
    reason: &str,
) {
    let side = if pnl_sol >= 0.0 { "PLUS" } else { "MINUS" };
    write_line(&format!(
        "{} | PAPER_SELL | id={} mint={} size_sol={:.6} entry={:.10} exit={:.10} pnl_sol={:+.6} pnl_pct={:+.2}% {} reason={}",
        Utc::now().format("%Y-%m-%d %H:%M:%S"),
        position_id,
        mint,
        size_sol,
        entry_price,
        exit_price,
        pnl_sol,
        pnl_pct,
        side,
        reason
    ));
}

pub fn log_summary(accept: u64, reject: u64, buys: u64, sells: u64, total_pnl_sol: f64) {
    write_line(&format!(
        "{} | SUMMARY | accept={} reject={} paper_buys={} paper_sells={} total_pnl_sol={:+.6}",
        Utc::now().format("%Y-%m-%d %H:%M:%S"),
        accept,
        reject,
        buys,
        sells,
        total_pnl_sol
    ));
}

pub fn path_hint() -> String {
    log_path().display().to_string()
}
