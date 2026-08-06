use solana_keypair::Keypair;
use solana_signer::Signer;
use std::fs;

fn main() {
    let keypair = Keypair::new();
    let bytes = keypair.to_bytes();
    
    // JSON array format for SOLBOT_KEYPAIR_JSON
    let json_array = serde_json::to_string(&bytes.to_vec()).unwrap();
    
    fs::write("/tmp/test-keypair.json", json_array).unwrap();
    println!("Generated test keypair at /tmp/test-keypair.json");
    println!("Pubkey: {}", keypair.pubkey());
}
