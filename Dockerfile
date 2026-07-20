FROM rust:bookworm AS builder
WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY app ./app
COPY docs ./docs
COPY tools.json README.md LICENSE ./

RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /home/mcp mcp

COPY --from=builder /build/target/release/mcp-filesystem /usr/local/bin/mcp-filesystem

USER 10001:10001
EXPOSE 3001

ENTRYPOINT ["/usr/local/bin/mcp-filesystem"]
CMD ["--directories", "/data", "--host", "0.0.0.0", "--http-port", "3001", "--access-mode", "readonly", "--enable-read"]
