# Security policy

## Supported versions

We provide security fixes for the latest minor release of Cognitora.
During the alpha (M1–M2) we may also patch the previous minor release
when the fix is straightforward to backport.

| Version | Supported          |
|---------|--------------------|
| 0.1.x   | yes (current)      |
| < 0.1   | no                 |

## Reporting a vulnerability

Please report any potential security issue **privately** to
`security@cognitora.dev` (GPG key on the website). Do not open a
public GitHub issue. We aim to:

- acknowledge the report within 48 hours
- share an initial assessment within 5 business days
- coordinate a release window with the reporter when a fix is ready

If you don't hear back within 7 days, you can also reach the
maintainers via a **private** GitHub Security Advisory on this
repository.

## Scope

In scope:

- Memory safety bugs in `cgn-*` Rust crates.
- Auth / mTLS / API-key handling in `cgn-router`, `cgn-auth`,
  `cgn-tls`.
- Privilege escalation through the gRPC control plane (`cgn-ctl`,
  `cgn-operator`).
- Container image and installer integrity (cosign, sha256).
- KV transport (gRPC, QUIC, RDMA) — both confidentiality and
  integrity.

Out of scope:

- Issues in third-party engine binaries (vLLM, TensorRT-LLM, …) —
  please report those upstream.
- DoS through resource exhaustion when admission control is
  disabled.
- Self-XSS in the docs site.

## Release artifact verification

Release tarballs are signed with cosign. The public key lives at
[`SECURITY/cosign.pub`](SECURITY/cosign.pub) and is mirrored at
`https://raw.githubusercontent.com/<org>/<repo>/main/SECURITY/cosign.pub`,
which is the URL the [`install.sh`](deploy/installer/install.sh)
fetches by default.

To verify a release manually:

```bash
cosign verify-blob \
  --key SECURITY/cosign.pub \
  --signature cognitora-<ver>-linux-x86_64.tar.gz.sig \
  cognitora-<ver>-linux-x86_64.tar.gz
```

Container images are signed with the same key and an SBOM is
attached:

```bash
cosign verify --key SECURITY/cosign.pub \
  ghcr.io/<org>/<repo>/cgn-router:<tag>
cosign download sbom \
  ghcr.io/<org>/<repo>/cgn-router:<tag>
```

## Threat model

A short summary lives at
[`docs/architecture/security.md`](docs/architecture/security.md).
It covers trust boundaries, PKI, API auth, container hardening, and
audit logging.
