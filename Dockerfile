# syntax=docker/dockerfile:1.7
#
# The callix command line, as two images:
#
#   slim   callix + the Python it embeds. Resolves Python and TypeScript (ty and
#          typescript-go are linked into the binary), PHP, C, C++ and YAML.
#   full   slim + the Go and Rust toolchains (and Node, for `--prepare`), so the
#          Go and Rust resolvers run too. This is the default: `docker build .`
#
# Why Python at all: the language adapters are Python classes, so the binary
# embeds the interpreter that drives them. It links libpython3.X dynamically,
# which is why both images derive from the same python:3.X Debian base and why
# PYTHON_VERSION is one argument for the whole file.
#
#   docker build -t callix .                       # full
#   docker build --target slim -t callix:slim .
#   docker run --rm -v "$PWD:/workspace" callix export . -f lsif -o dump.lsif

ARG PYTHON_VERSION=3.13
ARG GO_VERSION=1.26.0
ARG RUST_VERSION=1.97.0
ARG NODE_VERSION=22

# -----------------------------------------------------------------------------
# Toolchains. Go is needed to build the typescript-go bridge (build.rs) and, in
# the full image, to resolve Go projects; the Rust toolchain builds the binary
# and, with rust-analyzer and the standard library's sources, resolves Rust.
# Pinned, because what a resolver answers depends on its version.
# -----------------------------------------------------------------------------
FROM python:${PYTHON_VERSION}-bookworm AS toolchains
ARG GO_VERSION
ARG RUST_VERSION
ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/usr/local/go/bin:${PATH}
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    curl -fsSL "https://go.dev/dl/go${GO_VERSION}.linux-${arch}.tar.gz" | tar -C /usr/local -xz; \
    curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal \
        --default-toolchain "${RUST_VERSION}" -c rust-analyzer -c rust-src; \
    chmod -R a+rwX /opt/rustup /opt/cargo; \
    go version; rustc --version; rust-analyzer --version

# -----------------------------------------------------------------------------
# The binary.
# -----------------------------------------------------------------------------
FROM toolchains AS builder
WORKDIR /src
# Only what cargo needs, so editing the docs or the tests does not rebuild ty.
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY go ./go
COPY crates ./crates
ENV PYO3_PYTHON=/usr/local/bin/python3
# .ts-go is the typescript-go checkout build.rs clones (~370 MB); the cache
# mounts keep it, the registry and the compiled ty tree between local builds.
RUN --mount=type=cache,target=/opt/cargo/registry \
    --mount=type=cache,target=/opt/cargo/git \
    --mount=type=cache,target=/src/.ts-go \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release -p callix-cli \
 && install -D -s target/release/callix /out/callix

# -----------------------------------------------------------------------------
# slim
# -----------------------------------------------------------------------------
FROM python:${PYTHON_VERSION}-slim-bookworm AS slim
# git: cargo and Go fetch some dependencies through it.
# safe.directory: a workspace mounted from the host belongs to another uid, and
# git (and with it `go list`) refuses such a tree otherwise.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git \
 && rm -rf /var/lib/apt/lists/* \
 && git config --system --add safe.directory '*'
COPY --from=builder /out/callix /usr/local/bin/callix
WORKDIR /workspace
LABEL org.opencontainers.image.title="callix" \
      org.opencontainers.image.description="Polyglot code analysis: graph, LSIF, SBOM and reports" \
      org.opencontainers.image.source="https://github.com/Callix-Tools/callix" \
      org.opencontainers.image.licenses="MIT"
ENTRYPOINT ["callix"]
CMD ["--help"]

# -----------------------------------------------------------------------------
# full
# -----------------------------------------------------------------------------
FROM node:${NODE_VERSION}-bookworm-slim AS node

FROM slim AS full
# A C toolchain: rust-analyzer runs the project's build scripts to see what they
# generate, and cgo needs one for Go projects.
RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc libc6-dev \
 && rm -rf /var/lib/apt/lists/*
COPY --from=toolchains /usr/local/go /usr/local/go
COPY --from=toolchains /opt/rustup /opt/rustup
COPY --from=toolchains /opt/cargo /opt/cargo
# Node and npm, for `callix ... --prepare` on a project without node_modules.
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
 && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx \
 && mkdir -p /go /tmp/callix-cache \
 && chmod 1777 /go /tmp/callix-cache
ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    GOPATH=/go \
    GOCACHE=/tmp/callix-cache/go-build \
    GOFLAGS=-buildvcs=false \
    PATH=/opt/cargo/bin:/usr/local/go/bin:${PATH}
