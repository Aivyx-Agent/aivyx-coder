# syntax=docker/dockerfile:1

# ---- Builder stage: compiles the static x86_64-unknown-linux-musl
# release binary, mirroring .github/workflows/release.yml's own build
# steps exactly (same target, same musl-gcc CC override, same cargo
# invocation) so this image's binary matches what the real release
# pipeline ships, not a separate/divergent build path.
FROM rust:latest AS builder

RUN apt-get update && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /build
COPY . .

ENV CC_x86_64_unknown_linux_musl=musl-gcc
RUN cargo build --release --target x86_64-unknown-linux-musl -p aivyx

# ---- Runtime stage: a minimal, real Alpine base with git/bash/coreutils
# so the agent's own run_command/run_shell/git_* tools have real
# programs on PATH to invoke -- not scratch/distroless, which would
# leave most of the agent's real capability unusable.
FROM alpine:latest

RUN apk add --no-cache git bash coreutils

COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/aivyx-coder /usr/local/bin/aivyx-coder

ENTRYPOINT ["aivyx-coder"]
