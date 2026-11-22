# Builds the Linux release files: the static daemon, and the app's .deb, .rpm and tarball.
# Ubuntu 22.04 links the app against glibc 2.35: Ubuntu 22.04 and newer, Debian 12, and the other
# distros of the last few years run it.
FROM ubuntu:22.04

ARG RUST_VERSION=1.99.1
ARG CARGO_DEB_VERSION=3.8.0
ARG CARGO_GENERATE_RPM_VERSION=0.21.1

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential ca-certificates cmake curl dpkg-dev file git musl-tools patchelf pkg-config \
        libasound2-dev libgtk-3-dev libjavascriptcoregtk-4.1-dev libsoup-3.0-dev \
        libwebkit2gtk-4.1-dev libxdo-dev \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
        --default-toolchain "$RUST_VERSION" --target x86_64-unknown-linux-musl,aarch64-unknown-linux-musl \
    && cargo install --locked cargo-deb --version "$CARGO_DEB_VERSION" \
    && cargo install --locked cargo-generate-rpm --version "$CARGO_GENERATE_RPM_VERSION"

WORKDIR /src
