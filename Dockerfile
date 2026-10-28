# The image targets linux/amd64 only (the cluster). The build stage runs on the
# build host's own arch (no emulation) and cross-compiles when it isn't amd64.
# fmt, clippy and tests run locally in the pre-commit hook (.githooks/pre-commit), not here.
FROM --platform=$BUILDPLATFORM rust:1.99-slim-trixie AS build
WORKDIR /src
# One rustc at a time, override with `--build-arg CARGO_BUILD_JOBS=<n>`.
ARG CARGO_BUILD_JOBS=1
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
# Debian's gcc ships triplet-prefixed binaries, so these also hold natively.
ENV CARGO_BUILD_TARGET=x86_64-unknown-linux-gnu \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
    CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
    AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar
RUN rustup target add x86_64-unknown-linux-gnu \
    && if [ "$(dpkg --print-architecture)" != amd64 ]; then \
         apt-get update && apt-get install -y --no-install-recommends gcc-x86-64-linux-gnu libc6-dev-amd64-cross \
         && rm -rf /var/lib/apt/lists/*; \
       fi
COPY Cargo.toml Cargo.lock ./
# Dependencies in their own layer, so source changes don't rebuild matrix-sdk.
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked && rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

FROM --platform=linux/amd64 gcr.io/distroless/cc-debian13:nonroot
COPY --from=build /src/target/x86_64-unknown-linux-gnu/release/tekton-matrix-bot /usr/local/bin/tekton-matrix-bot
ENV DATA_DIR=/data
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/tekton-matrix-bot"]
