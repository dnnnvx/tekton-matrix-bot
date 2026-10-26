FROM rust:1.99-slim-trixie AS deps
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs

# fmt, clippy and tests. BuildKit runs this stage alongside `build`, and `build`
# waits for it at the end, so a failing check fails the image (and the pipeline).
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
RUN cargo build --release --locked && rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release --locked
COPY --from=check /checked /checked

FROM gcr.io/distroless/cc-debian13:nonroot
COPY --from=build /src/target/release/tekton-matrix-bot /usr/local/bin/tekton-matrix-bot
ENV DATA_DIR=/data
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/tekton-matrix-bot"]
