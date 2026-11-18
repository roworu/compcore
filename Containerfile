FROM docker.io/library/rust:1.98

RUN rustup component add clippy rustfmt \
    && rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl

ENV CARGO_TARGET_DIR=/src/target/podman
WORKDIR /src
