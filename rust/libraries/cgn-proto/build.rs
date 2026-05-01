use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Walk up to the workspace root (rust/libraries/cgn-proto -> rust/libraries
    // -> rust -> workspace).
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = crate_dir
        .parent()
        .unwrap() // rust/libraries
        .parent()
        .unwrap() // rust
        .parent()
        .unwrap(); // workspace root
    let proto_dir = workspace.join("proto");

    let files = [
        "cognitora/v1/common.proto",
        "cognitora/v1/router.proto",
        "cognitora/v1/agent.proto",
        "cognitora/v1/kv.proto",
        "cognitora/v1/control.proto",
        "cognitora/v1/metrics.proto",
    ];
    let full: Vec<_> = files.iter().map(|f| proto_dir.join(f)).collect();

    for f in &full {
        println!("cargo:rerun-if-changed={}", f.display());
    }

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&full, &[proto_dir])?;
    Ok(())
}
