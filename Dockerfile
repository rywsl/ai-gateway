# ----- Builder stage -----
FROM rust:1.88-bookworm AS builder
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

# Build the release binary.
RUN cargo build --release --locked -p gw-gateway \
 && strip /build/target/release/gw-gateway

# ----- Runtime stage -----
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3 curl \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /build/target/release/gw-gateway /usr/local/bin/gw-gateway
COPY --from=builder /build/config /app/config
COPY crates/gw-store/migrations /app/migrations

# Health probe expects these
EXPOSE 8000 9090 8080

# Use non-root user
RUN useradd --system --create-home --shell /bin/false gateway \
 && chown -R gateway:gateway /app
USER gateway

ENV RUST_LOG=info,gw=debug
ENV RUST_BACKTRACE=1

ENTRYPOINT ["/usr/local/bin/gw-gateway"]
