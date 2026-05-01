//! Cognitora generated gRPC stubs.
//!
//! All `*.proto` files under `proto/cognitora/v1/` are compiled by `tonic-build`
//! at crate build time. The generated module hierarchy mirrors the proto
//! `package` declarations.

#![allow(clippy::all)]
#![allow(unknown_lints)]

pub mod cognitora {
    pub mod v1 {
        tonic::include_proto!("cognitora.v1");
    }
}

/// Convenience aliases used throughout the workspace.
pub use cognitora::v1 as v1;

/// File descriptor set hash used for buf reflection / debug.
pub const PROTO_PACKAGE: &str = "cognitora.v1";
