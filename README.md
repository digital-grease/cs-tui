# cs-tui

A terminal client for [cyberspace.online](https://cyberspace.online), targeting the v0.3.7 API.

Personal, human-driven client. Per the [Cyberspace API terms](docs/api-v0.3.7.md): no scraping, no bots, no LLM-driven agents. This is software you drive with your own keystrokes.

## Status

Early development. Most of the documented v0.3.7 REST surface is implemented; live testing against the API is still pending. Chat and DMs await the published Firebase RTDB schema.

## Build

```sh
cargo build --release
./target/release/cs-tui --help
```

Requires Rust 1.80+ (stable channel; see `rust-toolchain.toml`).

## Layout

| Path | Purpose |
|---|---|
| `crates/cs-api/` | HTTP client + types for the Cyberspace REST API |
| `crates/cs-tui/` | Ratatui application (binary) |
| `docs/api-v0.3.7.md` | Authoritative API specification (do not modify) |

## License

MIT OR Apache-2.0 (dual-licensed).
