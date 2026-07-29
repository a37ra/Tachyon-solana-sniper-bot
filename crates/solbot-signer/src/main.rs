mod audit_log;
mod compromise_detection;
mod ipc_server;
mod key_material;
mod sequence_ledger;
mod signing;
mod validation;

#[cfg(test)]
mod fuzz_tests;

use anyhow::Result;
use sqlx::sqlite::SqlitePoolOptions;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

use crate::audit_log::AuditLog;
use crate::compromise_detection::CompromiseDetection;
use crate::ipc_server::{signer_proto::transaction_signer_service_server::TransactionSignerServiceServer, SignerService};
use crate::key_material::mlock::{enforce_no_core_dumps, GuardedKeypair};
use crate::key_material::vault_kms_client::load_keypair;
use crate::sequence_ledger::SequenceLedger;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("Starting solbot-signer");

    // Defense-in-depth: enforce no core dumps before loading any secrets
    if let Err(e) = enforce_no_core_dumps() {
        tracing::error!(error = %e, "Failed to enforce core dump prevention");
        // Continue anyway — systemd LimitCORE=0 is the primary protection
    }

    // Database connection
    let db_path = std::env::var("SOLBOT_DB_PATH")
        .unwrap_or_else(|_| "sqlite:../../solbot.db".to_string());

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_path)
        .await?;

    // Step 1 (ТЗ 2.4): Hydrate IPC sequence ledger BEFORE accepting any intents
    let ledger = Arc::new(SequenceLedger::new(pool.clone()));
    ledger.hydrate_all().await?;
    tracing::info!("IPC sequence ledger hydrated — anti-replay protection active");

    let audit_log = Arc::new(AuditLog::new(pool.clone()));

    // Load tier-specific threshold from config, defaulting to micro tier
    let threshold_5min: u32 = std::env::var("SOLBOT_SIGNING_RATE_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(68); // micro tier default

    let compromise_detection = Arc::new(CompromiseDetection::new(300, threshold_5min));

    // Load keypair from configured source and mlock it
    let raw_keypair = load_keypair().await?;
    let guarded = GuardedKeypair::new(raw_keypair);
    let keypair = Arc::new(guarded);

    // Socket path from env or default
    let socket_path = std::env::var("SOLBOT_SIGNER_SOCKET")
        .unwrap_or_else(|_| "/tmp/solbot-signer.sock".to_string());

    // Remove old socket if exists
    let _ = std::fs::remove_file(&socket_path);
    let uds = UnixListener::bind(&socket_path)?;

    // Set restrictive permissions on the socket (owner only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
    }

    let stream = UnixListenerStream::new(uds);

    let service = SignerService::new(ledger, audit_log, compromise_detection, keypair, pool);

    tracing::info!(socket = %socket_path, "Signer listening");
    
    Server::builder()
        .add_service(TransactionSignerServiceServer::new(service))
        .serve_with_incoming(stream)
        .await?;

    Ok(())
}
