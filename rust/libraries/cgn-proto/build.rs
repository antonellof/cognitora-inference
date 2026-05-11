use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The .proto files live at $CARGO_MANIFEST_DIR/proto. In the workspace
    // checkout these are symlinks pointing to the canonical workspace-root
    // `proto/` tree (which `buf` lints and which is the single source of
    // truth). When `cargo publish` packages this crate, cargo follows the
    // symlinks and copies the actual file content into the .crate archive,
    // so consumers downloading from crates.io get a self-contained crate.
    let crate_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by Cargo"),
    );
    let proto_dir = crate_dir.join("proto");

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
