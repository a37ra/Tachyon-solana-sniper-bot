//! Integration tests for SignerClient with real mock gRPC server.
//!
//! Tests the SignerClient's communication with the solbot-signer
//! using an in-process mock gRPC server over UnixStream.

use base64::Engine;
use solbot_common::types::intent::{IntentType, SignerIntentRequest, VerdictStatus};
use solbot_strategy::signer_client::SignerClient;
use std::time::Duration;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tonic::transport::Server;
use tonic::{Request, Response, Status};

// Include the generated proto code
pub mod signer_proto {
    tonic::include_proto!("signer");
}

use signer_proto::transaction_signer_service_server::TransactionSignerServiceServer;

// Mock gRPC service implementation
struct MockSignerService {
    response_behavior: ResponseBehavior,
}

#[derive(Clone)]
enum ResponseBehavior {
    Accept { signed_tx: String },
    Reject { reason: String },
    Timeout,
}

#[tonic::async_trait]
impl signer_proto::transaction_signer_service_server::TransactionSignerService
    for MockSignerService
{
    async fn sign_intent(
        &self,
        request: Request<signer_proto::SignerIntentRequest>,
    ) -> Result<Response<signer_proto::SignerIntentResponse>, Status> {
        let req = request.into_inner();

        match &self.response_behavior {
            ResponseBehavior::Accept { signed_tx } => {
                let response = signer_proto::SignerIntentResponse {
                    status: signer_proto::VerdictStatus::VerdictAccepted as i32,
                    signed_transaction_base64: signed_tx.clone(),
                    rejection_reason: String::new(),
                    last_ipc_seq: req.ipc_seq,
                    constructed_tx_signature: String::new(),
                };
                Ok(Response::new(response))
            }
            ResponseBehavior::Reject { reason } => {
                let response = signer_proto::SignerIntentResponse {
                    status: signer_proto::VerdictStatus::VerdictRejected as i32,
                    signed_transaction_base64: String::new(),
                    rejection_reason: reason.clone(),
                    last_ipc_seq: req.ipc_seq,
                    constructed_tx_signature: String::new(),
                };
                Ok(Response::new(response))
            }
            ResponseBehavior::Timeout => {
                // Simulate timeout by sleeping longer than client timeout
                tokio::time::sleep(Duration::from_secs(10)).await;
                Err(Status::deadline_exceeded("Mock timeout"))
            }
        }
    }
}

/// Helper to spawn a mock gRPC server on a Unix socket.
async fn spawn_mock_server(socket_path: String, behavior: ResponseBehavior) -> JoinHandle<()> {
    tokio::spawn(async move {
        let listener = UnixListener::bind(&socket_path).expect("Failed to bind Unix socket");

        let svc = TransactionSignerServiceServer::new(MockSignerService {
            response_behavior: behavior,
        });

        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
            .await
            .expect("Mock server failed");
    })
}

#[tokio::test]
async fn test_signer_client_successful_sign_intent() {
    let temp_file = tempfile::NamedTempFile::new().expect("Failed to create temp file");
    let socket_path = temp_file.path().to_str().unwrap().to_string();
    drop(temp_file);

    let signed_tx_data = vec![1, 2, 3, 4, 5];
    let signed_tx_base64 = base64::engine::general_purpose::STANDARD.encode(&signed_tx_data);
    let server_handle = spawn_mock_server(
        socket_path.clone(),
        ResponseBehavior::Accept {
            signed_tx: signed_tx_base64.clone(),
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = SignerClient::new(socket_path.clone());

    let request = SignerIntentRequest {
        context_key: "test_mint".to_string(),
        intent_type: IntentType::Buy,
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        unsigned_transaction_base64: "mock_unsigned_tx".to_string(),
        ipc_seq: 0,
        issued_at_unix_ms: 0,
        program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        max_size_lamports: 1000000,
        hard_cap_bps: 1000,
        destination_addresses: vec![],
        position_id: None,
        amount_or_pct: 1.0,
    };

    let result = client.sign_intent(request).await;

    assert!(result.is_ok(), "SignerClient request failed: {:?}", result);

    let response = result.unwrap();
    assert_eq!(response.status, VerdictStatus::Accepted);
    assert_eq!(response.signed_transaction_base64, Some(signed_tx_base64));
    assert_eq!(response.last_ipc_seq, 1);

    server_handle.abort();
    let _ = std::fs::remove_file(socket_path);
}

#[tokio::test]
async fn test_signer_client_rejection_handling() {
    let temp_file = tempfile::NamedTempFile::new().expect("Failed to create temp file");
    let socket_path = temp_file.path().to_str().unwrap().to_string();
    drop(temp_file);

    let server_handle = spawn_mock_server(
        socket_path.clone(),
        ResponseBehavior::Reject {
            reason: "Insufficient balance".to_string(),
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = SignerClient::with_timeout(socket_path.clone(), Duration::from_secs(5));

    let request = SignerIntentRequest {
        context_key: "test_mint".to_string(),
        intent_type: IntentType::Buy,
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        unsigned_transaction_base64: "mock_unsigned_tx".to_string(),
        ipc_seq: 0,
        issued_at_unix_ms: 0,
        program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        max_size_lamports: 1000000,
        hard_cap_bps: 1000,
        destination_addresses: vec![],
        position_id: None,
        amount_or_pct: 1.0,
    };

    let result = client.sign_intent(request).await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert_eq!(response.status, VerdictStatus::Rejected);
    assert_eq!(
        response.rejection_reason,
        Some("Insufficient balance".to_string())
    );

    server_handle.abort();
    let _ = std::fs::remove_file(socket_path);
}

#[tokio::test]
async fn test_signer_client_timeout_handling() {
    let temp_file = tempfile::NamedTempFile::new().expect("Failed to create temp file");
    let socket_path = temp_file.path().to_str().unwrap().to_string();
    drop(temp_file);

    let server_handle = spawn_mock_server(socket_path.clone(), ResponseBehavior::Timeout).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Client with short timeout
    let client = SignerClient::with_timeout(socket_path.clone(), Duration::from_millis(500));

    let request = SignerIntentRequest {
        context_key: "test_mint".to_string(),
        intent_type: IntentType::Buy,
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        unsigned_transaction_base64: "mock_unsigned_tx".to_string(),
        ipc_seq: 0,
        issued_at_unix_ms: 0,
        program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        max_size_lamports: 1000000,
        hard_cap_bps: 1000,
        destination_addresses: vec![],
        position_id: None,
        amount_or_pct: 1.0,
    };

    let result = client.sign_intent(request).await;

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    // Note: SignerClient has retry logic (MAX_RETRIES=3) that wraps deadline_exceeded errors
    // The actual error message will be "Signer RPC failed after 3 retries: gRPC SignIntent call failed"
    // which indicates the timeout occurred but was caught by the retry mechanism
    assert!(
        error_msg.contains("failed")
            || error_msg.contains("timeout")
            || error_msg.contains("deadline"),
        "Expected timeout/failure error, got: {}",
        error_msg
    );

    server_handle.abort();
    let _ = std::fs::remove_file(socket_path);
}
