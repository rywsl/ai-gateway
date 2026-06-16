# ----- Builder stage -----
ARG RUST_IMAGE=rust:1.88-bookworm
ARG DEBIAN_IMAGE=debian:bookworm-slim
FROM ${RUST_IMAGE} AS builder
WORKDIR /build

# pingora pulls in `libz-ng-sys` via `cmake`, plus assorted *-sys crates that
# need a C toolchain, pkg-config, and perl (for openssl-sys build scripts
# even when we end up linking against rustls). The egress impersonate path
# additionally depends on `wreq` → `boring-sys`, which bundles BoringSSL and
# runs bindgen at build time — that needs `libclang-dev` for bindgen and a
# C++ toolchain (`build-essential`) for the BoringSSL build. Install these
# once up front.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        cmake \
        make \
        build-essential \
        pkg-config \
        perl \
        libssl-dev \
        libclang-dev \
        ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Copy the whole workspace and build. The dep-cache stub trick (copy
# manifests, fake out src/, prefetch) is brittle here because several
# crates declare `[[bench]]` targets whose source files cargo wants to see
# even during `cargo fetch`. Stay simple: copy everything, let Docker's
# layer cache handle reruns when sources don't change.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY config ./config

# The `rust:1.88-bookworm` base image already ships the 1.88 toolchain. The
# repo `rust-toolchain.toml` declares `components = ["rustfmt", "clippy"]`,
# which makes rustup reach out to static.rust-lang.org to download those
# components on the first cargo invocation. That download is (a) unnecessary —
# neither rustfmt nor clippy is needed to `cargo build` — and (b) flaky behind
# some networks (rustup's downloader intermittently hits "tls handshake eof"
# even when plain HTTPS to the same host succeeds). Strip the components line
# in-image so the build uses the pre-installed toolchain with no network round
# trip. The host/CI rust-toolchain.toml is untouched (it still pins the
# components for the fmt/clippy gates).
RUN sed -i '/^components = /d' rust-toolchain.toml

# Build the release binary.
RUN cargo build --release --locked -p gw-gateway \
 && strip /build/target/release/gw-gateway

# ----- Runtime stage -----
FROM ${DEBIAN_IMAGE} AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3 curl \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /build/target/release/gw-gateway /usr/local/bin/gw-gateway
COPY --from=builder /build/config /app/config
COPY crates/gw-store/migrations /app/migrations

# Health probe expects these
EXPOSE 8000 9090 8080

# Container-level liveness/readiness probe against the health server on 8080.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8080/readyz || exit 1

# Use non-root user. Runtime spill/backlog defaults live under /data/cnexus;
# create and own them in-image so the default container can boot.
RUN useradd --system --create-home --shell /bin/false gateway \
 && mkdir -p /data/cnexus/spill/archive \
 && chown -R gateway:gateway /app /data/cnexus
USER gateway

ENV RUST_LOG=info,gw=debug
ENV RUST_BACKTRACE=1

ENTRYPOINT ["/usr/local/bin/gw-gateway"]
