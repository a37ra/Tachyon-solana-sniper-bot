use anyhow::{Result, anyhow};

pub unsafe fn mlock_keypair(ptr: *const u8, len: usize) -> Result<()> {
    if libc::mlock(ptr as *const libc::c_void, len) != 0 {
        return Err(anyhow!("Failed to mlock keypair memory"));
    }
    Ok(())
}

pub unsafe fn munlock_keypair(ptr: *const u8, len: usize) -> Result<()> {
    if libc::munlock(ptr as *const libc::c_void, len) != 0 {
        return Err(anyhow!("Failed to munlock keypair memory"));
    }
    Ok(())
}
