//! Diagnostic tool for testing sequential signal processing and queue depth.
//!
//! Generates a burst of signals to simulate queue buildup and logs the processing
//! times to verify the "Inbound Stream Age" latency hypothesis.

use anyhow::Result;
use solbot_common::types::Signal;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use tokio::sync::{mpsc, Mutex, Semaphore};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    println!("\n=======================================================");
    println!("🔍 QUEUE DEPTH DIAGNOSTIC TEST");
    println!("   Testing sequential signal processing bottleneck");
    println!("=======================================================\n");

    // Create signal channel with a small capacity to surface backpressure
    // behavior sooner and avoid a large buffer masking the effect.
    let (signal_tx, signal_rx) = mpsc::channel::<Signal>(30);

    // Start the signal processor in a background task
    let processor_handle = tokio::spawn(process_signals_diagnostic(signal_rx));

    // Generate burst of signals
    println!("📤 Generating burst of 200 signals...\n");
    let burst_start = Instant::now();

    for i in 0..200 {
        let signal = Signal {
            mint: format!("mint_{:02}", i),
            pool_address: format!("pool_{:02}", i),
            dex_program: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
            signal_source: "test".to_string(),
            slot: 250_000_000 + i as u64,
            received_at: chrono::Utc::now().timestamp_millis() as u64,
        };

        signal_tx.send(signal).await.ok();

        // Simulate signals arriving in rapid succession (within 10ms)
        if i < 19 {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    }

    let burst_elapsed = burst_start.elapsed();
    println!(
        "✅ All signals sent in {:.2}ms\n",
        burst_elapsed.as_secs_f64() * 1000.0
    );

    // Wait for processor to finish
    drop(signal_tx); // Signal EOF
    let _ = processor_handle.await;

    println!("\n=======================================================");
    println!("✅ Diagnostic test completed");
    println!("   Review logs for [SIGNAL PROCESSING START/END] to see");
    println!("   queue depth correlation with inbound_age_ms");
    println!("=======================================================\n");

    Ok(())
}

async fn process_signals_diagnostic(mut signal_rx: mpsc::Receiver<Signal>) {
    use std::sync::Arc as StdArc;

    let signal_count = StdArc::new(AtomicUsize::new(0));
    let max_concurrent_signals = 30;
    let semaphore = StdArc::new(Semaphore::new(max_concurrent_signals));
    let mut log_count = 0;
    let queued_processing_latencies = StdArc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();

    while let Some(signal) = signal_rx.recv().await {
        let queue_depth = signal_rx.len();
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let queued_age_ms = if signal.received_at > 0 && now_ms >= signal.received_at {
            (now_ms - signal.received_at) as f64
        } else {
            0.0
        };

        let signal_mint = signal.mint.clone();
        let seq_num = signal_count.fetch_add(1, Ordering::SeqCst);
        let record_latency = queue_depth > 25;

        tracing::info!(
            "[SIGNAL QUEUED] seq={}, mint={}, queue_depth={}, inbound_age_ms={:.2}",
            seq_num,
            signal_mint,
            queue_depth,
            queued_age_ms
        );

        let sem = semaphore.clone();
        let queued_processing_latencies = queued_processing_latencies.clone();

        // Acquire an owned permit before spawning so the receiver loop is
        // backpressured when permits are exhausted (true backpressure).
        let permit = sem.acquire_owned().await.unwrap();

        handles.push(tokio::spawn(async move {
            // move the owned permit into the task
            let _permit = permit;
            let process_start = Instant::now();

            let start_ms = chrono::Utc::now().timestamp_millis() as u64;
            let actual_inbound_age = if signal.received_at > 0 && start_ms >= signal.received_at {
                (start_ms - signal.received_at) as f64
            } else {
                0.0
            };

            tracing::info!(
                "[SIGNAL PROCESSING START] seq={}, mint={}, queue_depth={}, inbound_age_ms={:.2}",
                seq_num,
                signal.mint,
                queue_depth,
                actual_inbound_age
            );

            if record_latency {
                let mut guard = queued_processing_latencies.lock().await;
                guard.push(actual_inbound_age);
            }

            // Simulate risk_gate processing (300ms)
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;

            let process_elapsed_ms = process_start.elapsed().as_secs_f64() * 1000.0;
            tracing::info!(
                "[SIGNAL PROCESSING END] seq={}, mint={}, total_processing_ms={:.2}, inbound_age_ms={:.2}",
                seq_num,
                signal.mint,
                process_elapsed_ms,
                actual_inbound_age
            );
        }));

        log_count += 1;

        if log_count % 5 == 0 {
            println!(
                "  📊 Processed {} signals (latest: mint={}, inbound_age={:.2}ms, queue_depth={})",
                log_count,
                signal_mint,
                queued_age_ms,
                queue_depth
            );
        }
    }

    for handle in handles {
        let _ = handle.await;
    }

    println!(
        "\n  ✅ Signal processing complete: {} total signals processed",
        log_count
    );

    let values = queued_processing_latencies.lock().await;
    if !values.is_empty() {
        let mut sorted = values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p95 = sorted[(sorted.len() as f64 * 0.95).ceil() as usize - 1];
        let p99 = sorted[(sorted.len() as f64 * 0.99).ceil() as usize - 1];
        let avg = sorted.iter().sum::<f64>() / sorted.len() as f64;
        println!(
            "\n  📊 queue_depth > 25 samples={} avg={:.2}ms p95={:.2}ms p99={:.2}ms",
            sorted.len(), avg, p95, p99
        );
    } else {
        println!("\n  📊 No signals received with queue_depth > 25");
    }
}
