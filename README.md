# NesOli Gallery

High-performance photography asset service — part of the [NesOli](https://github.com/nestorlai1994/nesoli-forge) platform.

Built with **Rust + Axum**. Handles image ingestion, RAW decoding, EXIF extraction, and asset streaming.

## Quick Start

```bash
# Dev
cargo run

# Health check
curl http://localhost:8080/health

# Container (from nesoli-forge root)
podman-compose up -d --build gallery
```

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `PORT` | `8080` | HTTP listen port |
| `DATABASE_URL` | — | Postgres connection (Day 9+) |

## Roadmap

- [x] Day 5: `/health` endpoint
- [ ] Day 8: File system watcher (`notify` crate)
- [ ] Day 9: RAW decode (`libraw-rs`) + EXIF → DB
- [ ] Day 10: `nesoli://` custom protocol (Tauri)
