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
            DexProgram::MeteoraDlmm => "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp",
            DexProgram::MeteoraDynamicAmm => "Eo7WjKq67rjJQSZxS6z3YKapzY3eMj6Xy8X5EQVn5UaB",
            DexProgram::PumpFun => "6EF8rrecthR5Dkzon8Nwu78hRvfX4LctQnUBjWdK8nN2",
        }
    }

    pub fn all_program_ids() -> &'static [&'static str] {
        &[
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
            "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C",
            "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK",
            "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp",
            "Eo7WjKq67rjJQSZxS6z3YKapzY3eMj6Xy8X5EQVn5UaB",
            "6EF8rrecthR5Dkzon8Nwu78hRvfX4LctQnUBjWdK8nN2",
        ]
    }
}
