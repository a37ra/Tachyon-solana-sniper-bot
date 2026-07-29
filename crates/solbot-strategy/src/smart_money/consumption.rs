use solbot_common::types::signal::SmartMoneyHit;

pub fn process_hit(hit: &SmartMoneyHit) {
    tracing::info!(
        wallet = %hit.wallet_address,
        mint = %hit.mint,
        tx = %hit.tx_signature,
        slot = hit.detected_at_slot,
        "Smart Money hit detected"
    );
}
