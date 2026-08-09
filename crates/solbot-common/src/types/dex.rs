use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DexProgram {
    RaydiumAmmV4,
    RaydiumCpMm,
    RaydiumClmm,
    MeteoraDlmm,
    MeteoraDynamicAmm,
    PumpFun,
}

impl DexProgram {
    pub fn program_id(&self) -> &'static str {
        match self {
            DexProgram::RaydiumAmmV4 => "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
            DexProgram::RaydiumCpMm => "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C",
            DexProgram::RaydiumClmm => "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK",
            // Mainnet DLMM v2 (live WS owner). Legacy kVn14… kept in all_program_ids.
            DexProgram::MeteoraDlmm => "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
            // DAMM v2 (cpamdp). Legacy Dynamic AMM Eo7WjKq… also listed below.
            DexProgram::MeteoraDynamicAmm => "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG",
            DexProgram::PumpFun => "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
        }
    }

    pub fn all_program_ids() -> &'static [&'static str] {
        &[
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
            "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C",
            "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK",
            "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
            "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp",
            "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG",
            "Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB",
            "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
        ]
    }
}
