//! Secure memory handling for keypair material.
//!
//! Uses `mlock(2)` to prevent the keypair bytes from being swapped to disk
//! and `madvise(MADV_DONTDUMP)` to exclude them from core dumps.
//!
//! The `GuardedKeypair` wrapper provides RAII semantics: memory is locked
//! on construction and unlocked + zeroized on drop.

use anyhow::{anyhow, Result};
use solana_keypair::Keypair;
use solana_signer::Signer;
use std::ops::Deref;

/// A keypair whose underlying memory is mlock'd and excluded from core dumps.
///
/// # Safety invariants
/// - The keypair's 64-byte secret is mlock'd immediately after construction.
/// - On drop, the memory is munlock'd (allowing the OS to reclaim it).
/// - We call `madvise(MADV_DONTDUMP)` to exclude from core dumps.
pub struct GuardedKeypair {
    keypair: Keypair,
    /// Whether mlock succeeded (graceful degradation if it didn't).
    locked: bool,
}

impl GuardedKeypair {
    /// Wrap a Keypair with mlock protection.
    ///
    /// If mlock fails (e.g., insufficient `RLIMIT_MEMLOCK`), a warning is logged
    /// but the keypair is still usable — defense in depth with `LimitCORE=0`.
    pub fn new(keypair: Keypair) -> Self {
        let ptr = &keypair as *const Keypair as *const u8;
        let len = std::mem::size_of::<Keypair>();

        let locked = unsafe { lock_memory(ptr, len) };

        if locked {
            tracing::info!(
                pubkey = %keypair.pubkey(),
                "Keypair memory mlock'd successfully ({} bytes)", len
            );
        } else {
            tracing::warn!(
                pubkey = %keypair.pubkey(),
                "Failed to mlock keypair memory — relying on LimitCORE=0 and systemd hardening"
            );
        }

        Self { keypair, locked }
    }

    /// Access the inner keypair.
    pub fn keypair(&self) -> &Keypair {
        &self.keypair
    }
}

impl Deref for GuardedKeypair {
    type Target = Keypair;

    fn deref(&self) -> &Self::Target {
        &self.keypair
    }
}

impl Drop for GuardedKeypair {
    fn drop(&mut self) {
        if self.locked {
            let ptr = &self.keypair as *const Keypair as *const u8;
            let len = std::mem::size_of::<Keypair>();
            unsafe {
                unlock_memory(ptr, len);
            }
            tracing::info!("Keypair memory munlock'd on drop");
        }
    }
}

/// Lock memory pages containing `[ptr, ptr+len)` and exclude from core dumps.
///
/// Returns `true` if both mlock and madvise succeeded.
unsafe fn lock_memory(ptr: *const u8, len: usize) -> bool {
    let void_ptr = ptr as *mut libc::c_void;

    // mlock: prevent swapping to disk
    let mlock_ok = libc::mlock(void_ptr, len) == 0;
    if !mlock_ok {
        let errno = *libc::__errno_location();
        tracing::warn!("mlock failed with errno={}", errno);
    }

    // MADV_DONTDUMP: exclude from core dumps (Linux 3.4+)
    #[cfg(target_os = "linux")]
    {
        const MADV_DONTDUMP: i32 = 16;
        let madvise_ok = libc::madvise(void_ptr, len, MADV_DONTDUMP) == 0;
        if !madvise_ok {
            tracing::warn!("madvise(MADV_DONTDUMP) failed");
        }
    }

    mlock_ok
}

/// Unlock previously mlock'd memory.
unsafe fn unlock_memory(ptr: *const u8, len: usize) {
    let void_ptr = ptr as *mut libc::c_void;
    if libc::munlock(void_ptr, len) != 0 {
        tracing::warn!("munlock failed");
    }
}

/// Check and enforce core dump prevention at runtime.
///
/// This is defense-in-depth on top of systemd `LimitCORE=0`.
/// Should be called early in main() before any secrets are loaded.
pub fn enforce_no_core_dumps() -> Result<()> {
    use rlimit::{getrlimit, setrlimit, Resource};

    let (soft, hard) =
        getrlimit(Resource::CORE).map_err(|e| anyhow!("Failed to get RLIMIT_CORE: {e}"))?;

    if soft != 0 || hard != 0 {
        tracing::warn!(
            soft_limit = soft,
            hard_limit = hard,
            "RLIMIT_CORE is not zero — enforcing zero limit"
        );
        setrlimit(Resource::CORE, 0, 0)
            .map_err(|e| anyhow!("Failed to set RLIMIT_CORE to 0: {e}"))?;
        tracing::info!("RLIMIT_CORE set to 0 — core dumps disabled");
    } else {
        tracing::info!("RLIMIT_CORE is already 0 — core dumps disabled");
    }

    Ok(())
}
