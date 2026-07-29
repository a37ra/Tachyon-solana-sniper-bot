use anyhow::Result;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::Transaction;
use base64::{Engine as _, engine::general_purpose::STANDARD};

pub fn sign_transaction(unsigned_tx_base64: &str, keypair: &Keypair) -> Result<(String, String)> {
    let tx_bytes = STANDARD.decode(unsigned_tx_base64)?;
    let mut tx: Transaction = bincode::deserialize(&tx_bytes)?;
    
    tx.try_sign(&[keypair], tx.message.recent_blockhash)?;
    
    let signed_bytes = bincode::serialize(&tx)?;
    let signed_b64 = STANDARD.encode(signed_bytes);
    
    let signature = tx.signatures.first()
        .map(|sig| sig.to_string())
        .unwrap_or_default();
        
    Ok((signed_b64, signature))
}
