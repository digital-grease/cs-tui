# Configuration

On first run, cs-tui writes a commented `config.toml` to
`~/.config/cs-tui/config.toml`, listing every option at its default. It is never
overwritten, so your edits and comments are safe. Every option is optional, and
changes apply on restart.

Point at a different file with `--config <path>` or `$CS_TUI_CONFIG`. An
explicit path must already exist; only the default location is auto-created.

## Appearance

| Option | Default | Notes |
|---|---|---|
| `theme` | `cyber` | One of `cyber`, `c64`, `vt320`, `dark`, `vapor`, `paper` (light), `gruvbox`, `custom`. The in-app Esc → Theme menu overrides this and is remembered separately. |
| `[colors]` | built-in | Custom palette, used when `theme = "custom"`. Keys: `background`, `foreground`, `muted`, `accent`, `heading` (panel titles; defaults to `accent`), `success`, `error`, `warning`, `border`, `selection`. Each is a hex (`"#1e1e2e"`), `"reset"`, or an ANSI index (`"0"` to `"255"`); omitted keys keep the default. |
| `selection` | `fill` | Selected-row emphasis: `fill` (a full-row background fill, the `selection` color) or `bar` (just the `▌` bar + bold-accent text). |
| `background_mode` | `theme` | Screen background / terminal transparency. `theme` uses the palette's own background (`cyber`/`vt320`/`dark` are transparent, `c64`/`vapor`/`paper`/`gruvbox` solid); `transparent` never paints a backdrop so the terminal's transparency shows through on any theme; `opaque` always paints a solid backdrop (black for the transparent themes). |
| `compact` | `false` | Drop the blank-line / rule separators between list items for a denser feed. |

## Time

| Option | Default | Notes |
|---|---|---|
| `time_format` | `relative` | `relative` ("2h ago") or `absolute` ("2026-05-31 14:30"). |
| `timezone` | `utc` | For absolute timestamps: `utc`, or a fixed offset like `-05:00`, `+02:00`, `+0530`. |

## Behavior

| Option | Default | Notes |
|---|---|---|
| `start_section` | `feed` | Section opened on launch: `feed`, `notifications`, `c-mail`, `circ`, `bookmarks`, `topics`, `profile`, `journal`, `guilds`, `programs`, `settings`. |
| `nsfw` | `false` | Show NSFW posts by default (otherwise hidden until toggled). |
| `confirm_deletes` | `true` | Require the two-step `d` then `y` confirmation before deleting a post or note. |
| `feed_autorefresh` | `true` | Auto-refresh the feed in the background: new entries are prepended at the top without moving your scroll position (only while the feed is on screen). |
| `feed_refresh_secs` | `30` | Seconds between background feed polls. Minimum 10; lower values use more of the read rate limit. |
| `notifications_refresh_secs` | `20` | Seconds between background polls of the unread-notification count (the header badge). Minimum 5; lower values surface new notifications sooner but use more of the read rate limit. |
| `cmail_refresh_secs` | `20` | Seconds between background polls of the unread C-Mail count (the header badge). Minimum 5; lower values surface new mail sooner but use more of the read rate limit. |
| `cmail_bell` | `false` | Ring the terminal bell when new C-Mail arrives while you are on another screen. |
| `audio_volume` | `50` | Starting jukebox volume for a fresh session (0 to 130; above 100 is soft amplification). Adjust live with `[` / `]`. |
| `shuffle` | `false` | Start each session with shuffle mode armed (playback still begins by hand). See [Jukebox](KEYS.md#jukebox). |
| `editor` | _(unset, uses the built-in editor)_ | Set to an external editor command (e.g. `nvim`) to compose in it instead of the built-in editor. GUI editors must block until the file is closed, so use a wait flag: `code --wait`, `subl -w`, `gnome-text-editor --standalone`. Leave unset to use the built-in editor. `$VISUAL`/`$EDITOR` are no longer consulted (an environment editor that forks or is missing was silently aborting composes). |
| `browser` | _(unset, uses the OS default handler)_ | Command used to open a link, instead of `xdg-open` / `open` / `cmd /C start`. Split on spaces and run directly, with no shell, so quotes and globs are not interpreted. `%s` in an argument is replaced by the URL; without one it is appended (`firefox --new-tab`, `qutebrowser %s --target tab`). Only http and https links are ever opened, whatever this is set to. |
| `preview_length` | `200` | Characters of post content shown in list previews (clamped 20 to 2000). |
| `image_height` | `20` | Max rows for the inline image strip in post detail (clamped 1 to 60). |
| `image_sharpness` | `crisp` | How an image is resampled when it is scaled to fit: `crisp` (nearest neighbour, cheapest, keeps hard edges on pixel art), `smooth`, `medium`, or `sharp` (best on downscaled photographs, slowest). The cost is paid once per picture per size, when it is first drawn, and not again while you scroll. |
| `graphics_protocol` | _(unset, probes)_ | Force a terminal graphics protocol instead of probing for one: `kitty`, `iterm2`, `sixel`, or `halfblocks`. Leave unset unless the probe gets it wrong, which shows up as no images in a terminal that supports them, or a screenful of escape bytes in one that does not. |

## What other people can see

Two settings publish your activity to other people on Cyberspace. Both are on by
default, because that is what the website does and what people in a room expect.
Set either to `false` and cs-tui never makes the call at all.

| Option | Default | Notes |
|---|---|---|
| `circ_presence` | `true` | Announce your presence while a cIRC room is open, so your name appears in that room's user list for everyone in the room, including people reading on the website. cs-tui refreshes it on the cadence the server asks for, and removes you when you leave the room or quit. Set to `false` to stay out of the user list entirely; you can still read the room and post in it. |
| `cmail_typing` | `true` | Publish a typing indicator while a C-Mail conversation is open, so the other participant sees the same "…is typing" the website shows. It is refreshed while you type and cleared when you stop, close the conversation, or quit. Set to `false` to publish nothing; you still see their indicator either way. |

One further setting makes a request on your behalf, but to GitHub rather than to
Cyberspace, and publishes nothing to anyone you talk to.

| Option | Default | Notes |
|---|---|---|
| `update_check` | `true` | Ask GitHub, at most once a day and in the background, whether a newer cs-tui release exists, and mention it once if so. The release then stays listed in the Esc menu, which opens its page. The request tells GitHub your address and that you run cs-tui; it never touches the Cyberspace API, carries no session token, and nothing is ever downloaded or installed. Set to `false` to make no such request. |

## Input and rendering

| Option | Default | Notes |
|---|---|---|
| `mouse` | `false` | Capture the scroll wheel for in-app scrolling. Off keeps native terminal select/copy. `--mouse` forces it on. |
| `images` | `true` | Render inline images on graphics-capable terminals. `--no-images` forces it off. |
| `animate_styles` | `false` | Animate the `blink`, `wave` and `glitch` text styles instead of drawing static approximations of them. This redraws the chat pane several times a second for as long as an animated message is on screen, whether or not you are looking at it, which is real battery on a laptop. Off by default for that reason. |
| `hyperlinks` | `true` | Make links clickable via OSC 8 terminal hyperlinks (Ghostty, kitty, WezTerm, iTerm2, foot, recent VTE terminals, Windows Terminal, tmux ≥ 3.4). Off surfaces the bare URL for the terminal's own URL detection instead. |
| `api_base` | `https://api.cyberspace.online` | Override the API base URL. |
