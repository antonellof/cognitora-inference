# cgn-tls

[![crates.io](https://img.shields.io/crates/v/cgn-tls.svg)](https://crates.io/crates/cgn-tls)
[![docs.rs](https://docs.rs/cgn-tls/badge.svg)](https://docs.rs/cgn-tls)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

mTLS helpers for inter-service gRPC.

Cognitora speaks gRPC across hosts and uses mutual TLS for everything
that crosses a network boundary. This crate is a thin layer on top of
`rustls` and `tonic` so the router, agent, kvcached, and operator share
one consistent way of wiring identities, trust roots, and TLS configs.

## Use

```toml
[dependencies]
cgn-tls = "0.1"
```

```rust
use std::path::Path;
use cgn_tls::{load_identity, server_tls, client_tls};

let identity = load_identity(
    Path::new("/etc/cognitora/tls/server.crt"),
    Path::new("/etc/cognitora/tls/server.key"),
)?;
let server = server_tls(identity, Path::new("/etc/cognitora/tls/ca.crt"))?;
```

## API

| Function           | Purpose                                                          |
|--------------------|------------------------------------------------------------------|
| `load_identity`    | Read a PEM cert + key into a tonic `Identity`.                   |
| `server_tls`       | Assemble a tonic `ServerTlsConfig` requiring client certs.       |
| `client_tls`       | Assemble a tonic `ClientTlsConfig` against a CA bundle.          |
| `generate_dev_pki` | Bootstrap a self-signed CA + leaf for `cgn-ctl pki bootstrap`.   |

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).

Part of [Cognitora](https://github.com/antonellof/cognitora-inference).
