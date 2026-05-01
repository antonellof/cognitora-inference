# SECURITY/

Anchor directory for release-artifact verification.

- `cosign.pub` — the public half of the cosign keypair used to sign
  every Cognitora release tarball and container image. The first
  release tag will overwrite the placeholder content.

  The signing private key lives in the maintainers' offline keystore
  and is never copied to CI runners. CI signs against a short-lived
  Sigstore Fulcio identity via OIDC (`cosign sign-blob --yes`).

The repository's general security policy is in
[../SECURITY.md](../SECURITY.md).
