# cs-tui

A terminal client for [cyberspace.online](https://cyberspace.online), targeting the v0.5.1 API.

![cs-tui screenshot](docs/screenshot.png)

*The feed in the `vapor` theme (one of seven built-in themes; see [Configuration](#configuration)).*

## Status

Early development. Most of the documented v0.5.1 REST surface is implemented; live testing against the API is ongoing. Chat and DMs await the published Firebase RTDB schema.

## Features

- **Feed** with cursor-based infinite scroll and entry titles
- **Post detail** with threaded replies
- **Notifications** with read/unread filtering and an unread badge
- **Thread watching**: `w` in post detail watches/unwatches a thread for `thread_reply` notifications; replying or being `@mentioned` auto-watches (toggle with `autoWatchOnReply` in Settings)
- **Bookmarks**, **Topics**, and per-topic feeds
- **Profiles** (info / posts / replies / followers / following) with follow & unfollow
- **Compose** posts and replies in the built-in editor (soft-wrapping, multi-line paste, no external editor required); delete your own entries
- **Guilds**: browse member groups, view threads/members, join/leave, and post threads
- **Journal** (private notes) with revision history
- **Settings** round-trip that preserves fields the client doesn't model
- Markdown rendering with `@mention` highlighting
- Inline image rendering in post detail on graphics-capable terminals (Kitty/iTerm2/Sixel); `[image] url` placeholder elsewhere
- Seven built-in themes (`cyber`, `c64`, `vt320`, `dark`, `vapor`, `paper` (light), `gruvbox`), switchable at runtime, plus a `custom` palette defined in `config.toml`
- Per-endpoint rate limiting and one-shot token refresh on 401

## Install

Download the archive for your platform from the [latest release](https://github.com/digital-grease/cs-tui/releases/latest), extract it, and run the binary. No Rust toolchain required.

| Platform | Asset | Notes |
|---|---|---|
| Linux (any distro) | `cs-tui-<ver>-x86_64-unknown-linux-musl.tar.gz` | Fully static; the most portable Linux build. |
| Linux (glibc) | `cs-tui-<ver>-x86_64-unknown-linux-gnu.tar.gz` | Needs glibc 2.39 or newer. |
| macOS (Apple Silicon) | `cs-tui-<ver>-aarch64-apple-darwin.tar.gz` | |
| macOS (Intel) | `cs-tui-<ver>-x86_64-apple-darwin.tar.gz` | |
| Windows | `cs-tui-<ver>-x86_64-pc-windows-msvc.zip` | Windows 10 or newer. |

```sh
# Linux / macOS
tar xzf cs-tui-*-x86_64-unknown-linux-musl.tar.gz
./cs-tui
# optionally put it on your PATH
install -m 755 cs-tui ~/.local/bin/
```

On macOS the binaries are not notarized, so the first launch is blocked by Gatekeeper. Right-click the binary and choose Open, or clear the quarantine flag:

```sh
xattr -d com.apple.quarantine cs-tui
```

On Windows, SmartScreen may warn on the unsigned `.exe` (choose More info, then Run anyway). No extra runtime is needed on Windows 10 or newer.

## Build from source

```sh
cargo build --release
./target/release/cs-tui --help
```

Requires Rust 1.81+ (stable channel; see `rust-toolchain.toml`).

## Usage

```sh
# Launch
cs-tui

# Verbose logging (written to the log file, not the terminal)
cs-tui --debug
```

On first launch you log in with your cyberspace.online email and password. The
session is saved (see [Files](#files)) and reused on the next launch until you
log out.

cs-tui is keyboard-driven. Each screen shows its own context keys in the status
bar, and `?` opens a help overlay anywhere you aren't typing into a field.

### Themes

Cycle palettes at runtime via **Esc → Theme**; the selection is remembered
between runs. Set a default (or define a `custom` palette) in `config.toml`.

### Jukebox playback (optional)

Posts can carry a "jukebox" track (a YouTube link). cs-tui shows the track card
and cover art inline, and can stream the audio in the background when
[`mpv`](https://mpv.io) and [`yt-dlp`](https://github.com/yt-dlp/yt-dlp) are
installed and on your `PATH`. Without them the card and link still render, and
`o` opens the link in your browser instead.

| Key | Action |
|---|---|
| `p` | play / pause the focused track (feed, post, topics, bookmarks) |
| `o` | open the jukebox link in your browser |
| `s` | stop playback (also turns shuffle off) |
| `S` | toggle shuffle mode |
| `<` / `>` | previous / next track |
| `[` / `]` | volume down / up |

A now-playing bar shows at the bottom while audio plays. Set the starting volume
with `audio_volume` (0-130, default 50) in `config.toml`.

With **shuffle** on, a track that plays to its end chains into a random jukebox
post instead of stopping, indefinitely. Candidates come from the posts you've
already browsed, topped up now and then from the public feed. Pressing `S` with
nothing playing starts a random track right away; `s` stops the music and the
mode together. Picking a different track by hand keeps shuffle armed, and it
chains onward from whatever ends next. Set `shuffle = true` in `config.toml`
to start every session with the mode armed (playback still begins by hand).

`<` and `>` step through the play history, mpv-style: `<` replays earlier
tracks, `>` moves forward again, and at the newest entry `>` skips to a fresh
random pick from the same pool shuffle uses.

## Files

| Path | Purpose |
|---|---|
| `~/.config/cs-tui/config.toml` | User configuration (see [Configuration](#configuration)) |
| `~/.config/cs-tui/session.json` | Saved login session (mode `0600` on Unix) |
| `~/.config/cs-tui/prefs.json` | UI preferences (e.g. selected theme) |
| `~/.local/state/cs-tui/cs-tui.log` | Log output (`--debug` / `RUST_LOG` raise verbosity) |

(Paths follow the XDG base directory spec; locations differ on macOS/Windows.)

## Configuration

On first run, cs-tui writes a commented `config.toml` to
`~/.config/cs-tui/config.toml`, listing every option at its default. It is never
overwritten, so your edits and comments are safe. Every option is optional;
restart to apply changes. Point at a different file with `--config <path>` or
`$CS_TUI_CONFIG`; an explicit path must already exist (only the default
location is auto-created).

### Appearance

| Option | Default | Notes |
|---|---|---|
| `theme` | `cyber` | One of `cyber`, `c64`, `vt320`, `dark`, `vapor`, `paper` (light), `gruvbox`, `custom`. The in-app Esc → Theme menu overrides this and is remembered separately. |
| `[colors]` | built-in | Custom palette, used when `theme = "custom"`. Keys: `background`, `foreground`, `muted`, `accent`, `heading` (panel titles; defaults to `accent`), `success`, `error`, `warning`, `border`, `selection`. Each is a hex (`"#1e1e2e"`), `"reset"`, or an ANSI index (`"0"` to `"255"`); omitted keys keep the default. |
| `selection` | `fill` | Selected-row emphasis: `fill` (a full-row background fill, the `selection` color) or `bar` (just the `▌` bar + bold-accent text). |
| `background_mode` | `theme` | Screen background / terminal transparency. `theme` uses the palette's own background (`cyber`/`vt320`/`dark` are transparent, `c64`/`vapor`/`paper`/`gruvbox` solid); `transparent` never paints a backdrop so the terminal's transparency shows through on any theme; `opaque` always paints a solid backdrop (black for the transparent themes). |
| `compact` | `false` | Drop the blank-line / rule separators between list items for a denser feed. |

### Time

| Option | Default | Notes |
|---|---|---|
| `time_format` | `relative` | `relative` ("2h ago") or `absolute` ("2026-05-31 14:30"). |
| `timezone` | `utc` | For absolute timestamps: `utc`, or a fixed offset like `-05:00`, `+02:00`, `+0530`. |

### Behavior

| Option | Default | Notes |
|---|---|---|
| `start_section` | `feed` | Section opened on launch: `feed`, `notifications`, `bookmarks`, `topics`, `profile`, `journal`, `guilds`, `settings`. |
| `nsfw` | `false` | Show NSFW posts by default (otherwise hidden until toggled). |
| `confirm_deletes` | `true` | Require the two-step `d` then `y` confirmation before deleting a post or note. |
| `feed_autorefresh` | `true` | Auto-refresh the feed in the background: new entries are prepended at the top without moving your scroll position (only while the feed is on screen). |
| `feed_refresh_secs` | `60` | Seconds between background feed polls. Minimum 10; lower values use more of the read rate limit. |
| `notifications_refresh_secs` | `20` | Seconds between background polls of the unread-notification count (the header badge). Minimum 5; lower values surface new notifications sooner but use more of the read rate limit. |
| `audio_volume` | `50` | Starting jukebox volume for a fresh session (0 to 130; above 100 is soft amplification). Adjust live with `[` / `]`. |
| `shuffle` | `false` | Start each session with shuffle mode armed (playback still begins by hand). See [Jukebox playback](#jukebox-playback-optional). |
| `editor` | _(unset, uses the built-in editor)_ | Set to an external editor command (e.g. `nvim`) to compose in it instead of the built-in editor. GUI editors must block until the file is closed, so use a wait flag: `code --wait`, `subl -w`, `gnome-text-editor --standalone`. Leave unset to use the built-in editor. `$VISUAL`/`$EDITOR` are no longer consulted (an environment editor that forks or is missing was silently aborting composes). |
| `preview_length` | `200` | Characters of post content shown in list previews (clamped 20 to 2000). |
| `image_height` | `20` | Max rows for the inline image strip in post detail (clamped 1 to 60). |

### Input and rendering

| Option | Default | Notes |
|---|---|---|
| `mouse` | `false` | Capture the scroll wheel for in-app scrolling. Off keeps native terminal select/copy. `--mouse` forces it on. |
| `images` | `true` | Render inline images on graphics-capable terminals. `--no-images` forces it off. |
| `hyperlinks` | `true` | Make links clickable via OSC 8 terminal hyperlinks (Ghostty, kitty, WezTerm, iTerm2, foot, recent VTE terminals, Windows Terminal, tmux ≥ 3.4). Off surfaces the bare URL for the terminal's own URL detection instead. |
| `api_base` | `https://api.cyberspace.online` | Override the API base URL. |

## Layout

| Path | Purpose |
|---|---|
| `crates/cs-api/` | HTTP client + types for the Cyberspace REST API |
| `crates/cs-tui/` | Ratatui application (binary) |
| `docs/api-v0.5.1.md` | Authoritative API specification (do not modify) |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
