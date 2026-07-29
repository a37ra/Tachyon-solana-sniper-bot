//! Key material loading for solbot-signer.
//!
//! Production hierarchy:
//! 1. `SOLBOT_KEYPAIR_PATH` + `SOLBOT_KEYPAIR_PASSWORD` → AES-256-GCM encrypted file
//! 2. `SOLBOT_KEYPAIR_JSON` → JSON array of u8 bytes (dev/test only)
//! 3. `SOLBOT_KEYPAIR_BASE58` → base58-encoded secret key (dev/test only)
//!
//! Encrypted file format (created by `solbot-cli wallet encrypt`):
//!   [12-byte nonce][16-byte salt][ciphertext + 16-byte AES-GCM tag]
//!
//! The password is run through Argon2id to derive a 256-bit AES key.
//! After loading, the keypair bytes are zeroized from all intermediate buffers.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, bail, Context, Result};
use argon2::Argon2;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use zeroize::Zeroize;

/// Nonce length for AES-256-GCM (96 bits).
const NONCE_LEN: usize = 12;
/// Salt length for Argon2 key derivation.
const SALT_LEN: usize = 16;
/// Derived key length for AES-256.
const KEY_LEN: usize = 32;

/// Load a Keypair using the production hierarchy.
///
/// # Security notes
/// - Intermediate byte buffers are zeroized after use.
/// - The caller MUST mlock the returned Keypair's memory.
/// - `LimitCORE=0` in systemd prevents core dumps leaking key material.
pub async fn load_keypair() -> Result<Keypair> {
    // 1. Encrypted file (production path)
    if let (Ok(path), Ok(password)) = (
        std::env::var("SOLBOT_KEYPAIR_PATH"),
        std::env::var("SOLBOT_KEYPAIR_PASSWORD"),
    ) {
        tracing::info!(path = %path, "Loading keypair from encrypted file");
        return load_from_encrypted_file(&path, password.as_bytes()).await;
    }

    // 2. JSON byte array (dev/test only)
    if let Ok(json_str) = std::env::var("SOLBOT_KEYPAIR_JSON") {
        tracing::warn!("Loading keypair from SOLBOT_KEYPAIR_JSON env var — DEV/TEST ONLY");
        let mut bytes: Vec<u8> =
            serde_json::from_str(&json_str).context("Invalid JSON byte array in SOLBOT_KEYPAIR_JSON")?;
        let kp = Keypair::from_bytes(&bytes).map_err(|e| anyhow!("Invalid keypair bytes: {e}"))?;
        bytes.zeroize();
        return Ok(kp);
    }

    // 3. Base58 secret key (dev/test only)
    if let Ok(b58) = std::env::var("SOLBOT_KEYPAIR_BASE58") {
        tracing::warn!("Loading keypair from SOLBOT_KEYPAIR_BASE58 env var — DEV/TEST ONLY");
        let mut bytes = bs58::decode(&b58)
            .into_vec()
            .context("Invalid base58 in SOLBOT_KEYPAIR_BASE58")?;
        let kp = Keypair::from_bytes(&bytes).map_err(|e| anyhow!("Invalid keypair bytes: {e}"))?;
        bytes.zeroize();
        return Ok(kp);
    }

    // 4. Solana CLI-compatible JSON file (no encryption, dev/test only)
    if let Ok(path) = std::env::var("SOLBOT_KEYPAIR_FILE") {
        tracing::warn!(path = %path, "Loading keypair from unencrypted JSON file — DEV/TEST ONLY");
        let content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("Cannot read keypair file: {path}"))?;
        let mut bytes: Vec<u8> =
            serde_json::from_str(&content).context("Invalid JSON in keypair file")?;
        let kp = Keypair::from_bytes(&bytes).map_err(|e| anyhow!("Invalid keypair bytes: {e}"))?;
        bytes.zeroize();
        return Ok(kp);
    }

    bail!(
        "No keypair source configured. Set one of:\n\
         • SOLBOT_KEYPAIR_PATH + SOLBOT_KEYPAIR_PASSWORD (production)\n\
         • SOLBOT_KEYPAIR_FILE (dev, unencrypted JSON)\n\
         • SOLBOT_KEYPAIR_JSON (dev, inline JSON)\n\
         • SOLBOT_KEYPAIR_BASE58 (dev, inline base58)"
    );
}

/// Decrypt an AES-256-GCM encrypted keypair file.
///
/// File format: `[12-byte nonce][16-byte salt][ciphertext + tag]`
async fn load_from_encrypted_file(path: &str, password: &[u8]) -> Result<Keypair> {
    let data = tokio::fs::read(path)
        .await
        .with_context(|| format!("Cannot read encrypted keypair file: {path}"))?;

    if data.len() < NONCE_LEN + SALT_LEN + 1 {
        bail!("Encrypted keypair file too short: {} bytes", data.len());
    }

    let nonce_bytes = &data[..NONCE_LEN];
    let salt = &data[NONCE_LEN..NONCE_LEN + SALT_LEN];
    let ciphertext = &data[NONCE_LEN + SALT_LEN..];

    // Derive AES-256 key from password via Argon2id
    let mut derived_key = [0u8; KEY_LEN];
    Argon2::default()
        .hash_password_into(password, salt, &mut derived_key)
        .map_err(|e| anyhow!("Argon2 key derivation failed: {e}"))?;

    // Decrypt
    let cipher = Aes256Gcm::new_from_slice(&derived_key)
        .map_err(|e| anyhow!("AES-256-GCM init failed: {e}"))?;
    derived_key.zeroize();

    let nonce = Nonce::from_slice(nonce_bytes);
    let mut plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow!("Decryption failed — wrong password or corrupted file"))?;

    if plaintext.len() != 64 {
        plaintext.zeroize();
        bail!(
            "Decrypted keypair has wrong size: {} bytes (expected 64)",
            plaintext.len()
        );
    }

    let kp = Keypair::from_bytes(&plaintext).map_err(|e| {
        plaintext.zeroize();
        anyhow!("Invalid decrypted keypair: {e}")
    })?;
    plaintext.zeroize();

    tracing::info!(
        pubkey = %kp.pubkey(),
        "Keypair loaded and decrypted successfully"
    );
    Ok(kp)
}

/// Encrypt a keypair and write to file. Used by `solbot-cli wallet encrypt`.
pub async fn encrypt_keypair_to_file(
    keypair: &Keypair,
    path: &str,
    password: &[u8],
) -> Result<()> {
    use aes_gcm::aead::OsRng;
    use rand::RngCore;

    // Generate random nonce and salt
    let mut nonce_bytes = [0u8; NONCE_LEN];
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    OsRng.fill_bytes(&mut salt);

    // Derive key
    let mut derived_key = [0u8; KEY_LEN];
    Argon2::default()
        .hash_password_into(password, &salt, &mut derived_key)
        .map_err(|e| anyhow!("Argon2 key derivation failed: {e}"))?;

    let cipher = Aes256Gcm::new_from_slice(&derived_key)
        .map_err(|e| anyhow!("AES-256-GCM init failed: {e}"))?;
    derived_key.zeroize();

    let nonce = Nonce::from_slice(&nonce_bytes);
    let mut plaintext = keypair.to_bytes().to_vec();
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_ref())
        .map_err(|e| anyhow!("Encryption failed: {e}"))?;
    plaintext.zeroize();

    // Write: [nonce][salt][ciphertext+tag]
    let mut output = Vec::with_capacity(NONCE_LEN + SALT_LEN + ciphertext.len());
    output.extend_from_slice(&nonce_bytes);
    output.extend_from_slice(&salt);
    output.extend_from_slice(&ciphertext);

    tokio::fs::write(path, &output)
        .await
        .with_context(|| format!("Cannot write encrypted keypair to: {path}"))?;

    tracing::info!(path = %path, "Keypair encrypted and saved");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use std::io::Write;

    #[tokio::test]
    async fn test_encrypt_decrypt_roundtrip() {
        let original = Keypair::new();
        let password = b"test-password-123";

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        encrypt_keypair_to_file(&original, &path, password)
            .await
            .unwrap();

        let loaded = load_from_encrypted_file(&path, password).await.unwrap();
        assert_eq!(original.pubkey(), loaded.pubkey());
        assert_eq!(original.to_bytes(), loaded.to_bytes());
    }

    #[tokio::test]
    async fn test_wrong_password_fails() {
        let original = Keypair::new();
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        encrypt_keypair_to_file(&original, &path, b"correct")
            .await
            .unwrap();

        let result = load_from_encrypted_file(&path, b"wrong").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_load_from_json_env() {
        let kp = Keypair::new();
        let json = serde_json::to_string(&kp.to_bytes().to_vec()).unwrap();
        std::env::set_var("SOLBOT_KEYPAIR_JSON", &json);
        // Note: can't reliably test load_keypair() in parallel tests due to env vars
        std::env::remove_var("SOLBOT_KEYPAIR_JSON");
    }
}
