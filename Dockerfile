# Build stage — dep-cache layer trick keeps rebuilds fast
FROM rust:alpine AS builder
WORKDIR /app
RUN apk add --no-cache musl-dev

# Cache dependencies: copy manifests, build a dummy binary, then replace with real source
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

# Build real binary
COPY src ./src
RUN touch src/main.rs && cargo build --release

# Runtime stage — minimal alpine image with libvips CLI for thumbnail generation
FROM alpine:3.20
WORKDIR /app
RUN apk add --no-cache vips-tools libraw-tools \
    && addgroup -S nesoli && adduser -S nesoli -G nesoli
COPY --from=builder /app/target/release/nesoli-gallery .
USER nesoli
EXPOSE 8080
CMD ["./nesoli-gallery"]
