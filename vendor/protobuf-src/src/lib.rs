use std::path::PathBuf;

pub fn protoc() -> PathBuf {
    std::env::var_os("PROTOC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("protoc"))
}

