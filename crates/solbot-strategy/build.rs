fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_client(true)
        .build_server(true) // Enable server code for mock gRPC tests
        .compile_protos(&["../../proto/signer.proto"], &["../../proto"])?;
    Ok(())
}
