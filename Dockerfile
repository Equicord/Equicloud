# Build stage
FROM rust:latest AS builder

WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy Cargo files
COPY Cargo.toml Cargo.lock ./

# Copy source code
COPY src ./src
COPY migrations ./migrations

# Build the application
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Copy the binaries from builder stage
COPY --from=builder /app/target/release/equicloud .
COPY --from=builder /app/target/release/migrate_legacy_users .

# Copy migrations
COPY --from=builder /app/migrations ./migrations

# Create non-root user
RUN groupadd -r equicloud && useradd -r -g equicloud equicloud
RUN chown -R equicloud:equicloud /app
USER equicloud

# Run the application
CMD ["./equicloud"]
