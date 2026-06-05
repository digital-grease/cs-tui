# cs-tui

A terminal client for [cyberspace.online](https://cyberspace.online), targeting the v0.5.0 API.

Personal, human-driven client. Per the [Cyberspace API terms](docs/api-v0.5.0.md): no scraping, no bots, no LLM-driven agents. This is software you drive with your own keystrokes.

## Status

Early development. Most of the documented v0.5.0 REST surface is implemented; live testing against the API is ongoing. Chat and DMs await the published Firebase RTDB schema.

## Features

- **Feed** with cursor-based infinite scroll and entry titles
- **Post detail** with threaded replies
- **Notifications** with read/unread filtering and an unread badge
- **Bookmarks**, **Topics**, and per-topic feeds
- **Profiles** (info / posts / replies / followers / following) with follow & unfollow
- **Compose** posts and replies via your `$EDITOR`; delete your own entries
- **Guilds** — browse member groups, view threads/members, join/leave, and post threads
- **Journal** (private notes) with revision history
- **Settings** round-trip that preserves fields the client doesn't model
- Markdown rendering with `@mention` highlighting
- Inline image rendering in post detail on graphics-capable terminals (Kitty/iTerm2/Sixel); `[image] url` placeholder elsewhere
- Five built-in themes (`cyber`, `c64`, `vt320`, `dark`, `vapor`), switchable at runtime, plus a `custom` palette defined in `config.toml`
- Per-endpoint rate limiting and one-shot token refresh on 401

## Build

```sh
cargo build --release
./target/release/cs-tui --help
```

Requires Rust 1.81+ (stable channel; see `rust-toolchain.toml`).

## Usage

```sh
# Run against the default API (https://api.cyberspace.online)
cs-tui

# Pick a theme for this run (also remembered between runs)
cs-tui --theme vt320

# Point at a different API base (include the scheme)
cs-tui --api-base https://staging.example.com

# Verbose logging (written to the log file, not the terminal)
cs-tui --debug
```

On first launch you log in with your cyberspace.online email and password. The
session is saved (see [Files](#files)) and reused on the next launch until you
log out.

### Keys

| Key | Action |
|---|---|
| `1`–`8` | Switch section: Feed · Notifications · Bookmarks · Topics · Profile · Journal · Settings · Guilds |
| `j`/`k` or `↑`/`↓` | Move down / up |
| `g`/`G` or `Home`/`End` | Jump to top / bottom |
| `Enter` | Open / select |
| `r` | Refresh |
| `c` | Compose / new |
| `Esc` | Open the menu (Back · Logout · Theme · Quit) |
| `?` | Help overlay |
| `Backspace` | Go back |
| `Ctrl+C` | Quit |

Each screen shows its own context keys in the status bar, and `?` opens a help
overlay anywhere you aren't typing into a field.

### Themes

Cycle palettes at runtime via **Esc → Theme** (or set one at startup with
`--theme` / the `CS_TUI_THEME` environment variable). The selection is
remembered between runs.

## Files

| Path | Purpose |
|---|---|
| `~/.config/cs-tui/session.json` | Saved login session (mode `0600` on Unix) |
| `~/.config/cs-tui/prefs.json` | UI preferences (e.g. selected theme) |
| `~/.local/state/cs-tui/cs-tui.log` | Log output (`--debug` / `RUST_LOG` raise verbosity) |

(Paths follow the XDG base directory spec; locations differ on macOS/Windows.)

## Layout

| Path | Purpose |
|---|---|
| `crates/cs-api/` | HTTP client + types for the Cyberspace REST API |
| `crates/cs-tui/` | Ratatui application (binary) |
| `docs/api-v0.5.0.md` | Authoritative API specification (do not modify) |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
