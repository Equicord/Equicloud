# Build stage
FROM rust:1.90-slim-bookworm AS builder

WORKDIR /app

# Copy Cargo files first to leverage Docker cache for dependency builds
COPY Cargo.toml Cargo.lock ./

# Copy source and migrations (migrations are embedded by include_dir! at compile time)
COPY src ./src
COPY migrations ./migrations

# Build the application
RUN cargo build --release && \
    cp target/release/equicloud /equicloud-bin

# Runtime stage
FROM debian:bookworm-slim

WORKDIR /app

# Runtime needs ca-certificates (TLS via rustls; no openssl needed) + wget for HEALTHCHECK
RUN apt-get update && apt-get install -y \
    ca-certificates \
    wget \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /equicloud-bin ./equicloud

# Create non-root user
RUN groupadd -r equicloud && useradd -r -g equicloud equicloud
RUN chown -R equicloud:equicloud /app
USER equicloud

EXPOSE 9000

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD wget --quiet --tries=1 --spider --timeout=4 \
        "http://localhost:${SERVER_PORT:-9000}/health" || exit 1

CMD ["./equicloud"]
