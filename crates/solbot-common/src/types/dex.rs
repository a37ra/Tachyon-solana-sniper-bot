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
