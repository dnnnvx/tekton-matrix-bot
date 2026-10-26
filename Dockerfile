FROM rust:1.99-slim-trixie AS deps
WORKDIR /src
# Cap rustc parallelism to keep the build pod's peak memory down. Override with
# `--build-arg CARGO_BUILD_JOBS=<n>`; ENV carries it into the stages below.
ARG CARGO_BUILD_JOBS=2
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs

# fmt, clippy and tests. `build` depends on this stage up front, so the two run
# one after the other (not in parallel) and a failing check fails fast.
FROM deps AS check
RUN rustup component add clippy rustfmt
# Dependencies in their own layer, so source changes don't rebuild matrix-sdk.
RUN cargo test --locked --no-run && cargo clippy --locked --all-targets && rm -rf src
COPY src ./src
RUN touch src/main.rs \
    && cargo fmt --check \
    && cargo clippy --locked --all-targets -- -D warnings \
    && cargo test --locked \
    && touch /checked

FROM deps AS build
COPY --from=check /checked /checked
RUN cargo build --release --locked && rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

FROM gcr.io/distroless/cc-debian13:nonroot
COPY --from=build /src/target/release/tekton-matrix-bot /usr/local/bin/tekton-matrix-bot
ENV DATA_DIR=/data
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/tekton-matrix-bot"]
