mod audit_log;
mod compromise_detection;
mod ipc_server;
mod key_material;
mod sequence_ledger;
mod signing;
mod validation;

use anyhow::Result;
use sqlx::sqlite::SqlitePoolOptions;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

use crate::audit_log::AuditLog;
use crate::compromise_detection::CompromiseDetection;
use crate::ipc_server::{signer_proto::transaction_signer_service_server::TransactionSignerServiceServer, SignerService};
use crate::key_material::vault_kms_client::load_keypair;
use crate::sequence_ledger::SequenceLedger;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("Starting solbot-signer");

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect("sqlite:../../solbot.db")
        .await?;

    let ledger = Arc::new(SequenceLedger::new(pool.clone()));
    ledger.hydrate_all().await?;

    let audit_log = Arc::new(AuditLog::new(pool.clone()));
    let compromise_detection = Arc::new(CompromiseDetection::new(300, 100)); // 5 mins, 100 threshold placeholder
    let keypair = Arc::new(load_keypair().await?);

    // Mlock keypair here in production (skipped for skeleton simplicity as we can't get raw ptr easily from solana Keypair)

    let socket_path = "/tmp/solbot-signer.sock";
    let _ = std::fs::remove_file(socket_path); // Remove old socket if exists
    let uds = UnixListener::bind(socket_path)?;
    let stream = UnixListenerStream::new(uds);

    let service = SignerService::new(ledger, audit_log, compromise_detection, keypair, pool);

    tracing::info!("Signer listening on {}", socket_path);
    
    Server::builder()
        .add_service(TransactionSignerServiceServer::new(service))
        .serve_with_incoming(stream)
        .await?;

    Ok(())
}
