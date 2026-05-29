# cs-tui

A terminal client for [cyberspace.online](https://cyberspace.online), targeting the v0.4 API.

Personal, human-driven client. Per the [Cyberspace API terms](docs/api-v0.4.md): no scraping, no bots, no LLM-driven agents. This is software you drive with your own keystrokes.

## Status

Early development. Most of the documented v0.3.7 REST surface is implemented; live testing against the API is still pending. Chat and DMs await the published Firebase RTDB schema.

## Features

- **Feed** with cursor-based infinite scroll and entry titles
- **Post detail** with threaded replies
- **Notifications** with read/unread filtering and an unread badge
- **Bookmarks**, **Topics**, and per-topic feeds
- **Profiles** (info / posts / replies / followers / following) with follow & unfollow
- **Compose** posts and replies via your `$EDITOR`; delete your own entries
- **Journal** (private notes) with revision history
- **Settings** round-trip that preserves fields the client doesn't model
- Markdown rendering with `@mention` highlighting
- Four themes (`cyber`, `c64`, `vt320`, `dark`), switchable at runtime
- Per-endpoint rate limiting and one-shot token refresh on 401

## Build

```sh
cargo build --release
./target/release/cs-tui --help
```

Requires Rust 1.80+ (stable channel; see `rust-toolchain.toml`).

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
| `1`–`7` | Switch section: Feed · Notifications · Bookmarks · Topics · Profile · Journal · Settings |
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
| `docs/api-v0.4.md` | Authoritative API specification (do not modify) |

## License

MIT OR Apache-2.0 (dual-licensed).
