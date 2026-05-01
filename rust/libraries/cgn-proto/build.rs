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

    // Locate the well-known google/protobuf/*.proto files. Protoc normally
    // auto-discovers them next to the binary on macOS/Homebrew, but the
    // Debian/Ubuntu `protobuf-compiler` package does not — the user has to
    // either install `libprotobuf-dev` or set `PROTOC_INCLUDE`. We accept
    // both: respect `PROTOC_INCLUDE` if set, otherwise probe a couple of
    // common system locations so the build works out of the box.
    let mut includes: Vec<PathBuf> = vec![proto_dir.clone()];
    if let Ok(extra) = std::env::var("PROTOC_INCLUDE") {
        for p in std::env::split_paths(&extra) {
            includes.push(p);
        }
    } else {
        for candidate in [
            "/usr/include",
            "/usr/local/include",
            "/opt/homebrew/include",
        ] {
            if PathBuf::from(candidate)
                .join("google/protobuf/empty.proto")
                .is_file()
            {
                includes.push(PathBuf::from(candidate));
                break;
            }
        }
    }

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&full, &includes)?;
    Ok(())
}
