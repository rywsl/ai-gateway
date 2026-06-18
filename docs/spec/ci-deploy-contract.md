# CI Deploy Contract

This checklist preserves the Rust-to-Go deploy invariants that used to live in
`scripts/check-k3s-image-sync-ci.rb`. That script was removed on 2026-06-17
because it was not wired into CI and required a Ruby runtime the CI images do
not provide.

When editing either repository's `.gitlab-ci.yml`, keep these invariants true.

## Rust Repository

`sync-k3s-rust-gateway` must:

- Exist as a job.
- Depend on `push-image` through `needs`.
- Set the Rust gateway image with `kubectl set image`.
- Wait for rollout completion with `kubectl rollout status`.

`sync-k3s-rust-gateway` must not manage Go-owned or stateful resources:

- No `kubectl apply -f`.
- No `ingress.yaml`.
- No `statefulset/`.
- No `deployment/go-allinone`.
- No `postgres`.
- No `redis`.
- No `clickhouse`.

## Go Repository

The Go deploy pipeline must:

- Not run `rollout restart deployment/rust-gateway` on every Go deploy.
- Preserve the live Rust gateway image before rendering Go-owned manifests via
  `current_rust_gateway_image=`.
- Carry `RUST_GATEWAY_DEPLOY_IMAGE=`.
- Substitute `__RUST_GATEWAY_IMAGE__` with
  `${RUST_GATEWAY_DEPLOY_IMAGE}` using
  `s#__RUST_GATEWAY_IMAGE__#${RUST_GATEWAY_DEPLOY_IMAGE}#g`.

Read this file before changing either repository's deploy jobs.
