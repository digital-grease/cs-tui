# Keys

cs-tui is keyboard-driven. `?` opens a help overlay anywhere you are not typing
into a field, listing every key grouped by screen. The overlay is longer than a
terminal, so it scrolls with `j`/`k`, the arrows and PgUp/PgDn (`g`/`G` jump to
the ends); any other key closes it.

## Everywhere

| Key | Action |
|---|---|
| `←` / `→` | Move between sections |
| `j` / `k` | Move through a list |
| `Enter` | Open the selected item |
| `r` | Refresh |
| `c` | Compose |
| `Tab` / `Shift+Tab` | Switch sub-tabs (profile, guild) |
| `?` | Help overlay |
| `Esc` | Back, or the menu on a top-level section |
| `Backspace` | Back |

The sections, in order: Feed, Notifications, C-Mail, cIRC, Bookmarks, Topics,
Profile, Journal, Guilds, Programs, Settings. There are no number-key jumps, so
digits reach the screen you are on.

## By screen

| Screen | Key | Action |
|---|---|---|
| Feed, topic feed, post detail | `e` | Edit your own entry (on post detail, the selected reply) |
| Feed, topic feed, post detail | `F` | Flag an entry or reply for review, with an optional reason |
| Post detail | `w` | Watch / unwatch the thread for reply notifications |
| Post detail | `v` | Show / hide a post behind its content warning |
| Someone else's profile | `P` | Poke them |
| Your own profile, Posts tab | `E` | Edit the selected post (`e` edits your profile, `P` pins a post) |
| Guild | `J` then `y` | Join (member if you are not in a guild yet, apprentice otherwise) |
| Guild | `P` then `y` | Make this guild your profile badge |
| Guild | `L` then `y` | Leave, an apprenticeship included (founders leave on the web) |
| cIRC room, C-Mail composer | `Alt+Enter` or `Ctrl+J` | Start a new line in the message ([up to 10](#line-breaks-in-chat)) |
| cIRC room | `Ctrl+U` | Show / hide the room roster |
| cIRC room | `Ctrl+A` | Open the action menu for the message under the cursor |
| cIRC room | `↑` / `↓` | Move through the room history (`PgUp` / `PgDn` too) |
| cIRC action menu | `j` / `k` | Pick a message |
| cIRC action menu | `Home` / `End` | Jump to the oldest / newest message held |
| cIRC action menu | `d` then `y` | Delete your own message |
| cIRC action menu | `F` | Flag the message |
| cIRC action menu | `o` | Open the image or GIF, or play the track |
| cIRC action menu | `v` | Reveal a spoiler, or the original of a rewritten message |
| cIRC action menu | `m` | Mute the author in this room |
| cIRC action menu | `Esc` | Close the menu, back to the composer |
| C-Mail conversation | `o` | Open the attachment, or play the track |
| C-Mail conversation | `v` | Reveal a spoiler, or the original of a rewritten message |
| Program gallery | `m` / `t` | Switch between the gallery and your own programs / cycle the runtime filter |
| Program gallery | `P` | Publish a program from a local file |
| Program gallery | `d` then `y` | Recall your own program (in the `m` listing; reversible, the release history stays) |
| Program gallery | `D` then `y` | Delete the record, which frees the slot it holds against your program count |
| Program publish form | `Tab` / `Space` / `Ctrl+D` | Move between fields / cycle the runtime / publish |
| Program source | `,` / `.` | Walk the release history (`[` / `]` are the jukebox volume) |
| Program source | `j`/`k`, `PgUp`/`PgDn`, `g`/`G` | Scroll the source |
| Program source | `w` | Write the source to a file (it will not overwrite one) |

### Why cIRC uses chords

In a cIRC room the composer always has focus, so every letter you type goes into
the message. That is why the room's own actions are chords, and why deleting or
flagging a message goes through the `Ctrl+A` menu: while the menu is up it owns
the keyboard, and `Ctrl+A` is the only key in a room that is not typed.

### Line breaks in chat

A cIRC or C-Mail message can run to 10 lines. `Enter` sends, and `Alt+Enter`
or `Ctrl+J` starts a new line. Which of those reaches cs-tui depends on the
terminal:

- Windows Terminal, WezTerm and mintty keep `Alt+Enter` for full screen, so use
  `Ctrl+J` there. On Windows, `Ctrl+Enter` and `Shift+Enter` start a new line
  too.
- On macOS, Option is only Alt when the terminal is set to use it as Meta.
  Otherwise Option+Return is a plain Return and sends, so use `Ctrl+J`.
- Elsewhere, `Shift+Enter` and `Ctrl+Enter` usually arrive as a plain `Enter`
  and send.

Pasting several lines keeps them as separate lines. The exception is Windows,
where a paste reaches cs-tui as typed keys and each line break sends what came
before it. To paste several lines there, open the editor with `Ctrl+E`, paste
into it, and close it to bring the text back.

A draft longer than 10 lines is not sent. The server would not refuse it, it
would run the extra lines into the tenth, so cs-tui says so and keeps the draft
for you to trim. A run of blank lines counts as one line, since the server
collapses those runs. In C-Mail that is the whole rule. In a cIRC room two
things are exempt:

- `/art`, which has its own canvas of 80 columns by 25 rows. cs-tui checks a
  picture against it before sending and names the limit it breaks, rather
  than leaving the server to refuse it.
- A text style like `/rainbow`, whose line breaks the server turns into spaces.
  A styled draft written on several lines gets a warning first; press `Enter`
  again to send it as one line.

A message also holds at most 2,048 characters, with no exemptions. A longer
draft is not sent either: a warning says how many characters to cut, and the
draft stays where it is. The count is cautious: it uses the units a JavaScript
server counts, in which most emoji count as two, so a message full of them
holds about half as many.

### Masked and rewritten messages

A `/spoiler` message is drawn as blocks with a `[spoiler]` label where they end.
`v` unmasks it and hides it again. The same key shows the original text of a
`/l33t`, `/flip` or `/cursive` message, which cs-tui rewrites itself: the server
sends those raw.

## Jukebox

Posts and chat messages can carry a "jukebox" track: a YouTube link, or an MP3
uploaded on the website. cs-tui shows the track card and cover art inline, and
can stream the audio in the background when [`mpv`](https://mpv.io) is
installed and on your `PATH`. YouTube tracks also need
[`yt-dlp`](https://github.com/yt-dlp/yt-dlp); without it, shuffle plays only the
uploaded tracks. Without mpv the card and link still render, and `o` opens the
link in your browser instead.

| Key | Action |
|---|---|
| `p` | Play / pause the focused track (feed, post, topics, bookmarks) |
| `o` | Open the jukebox link in your browser |
| `s` | Stop playback (also turns shuffle off) |
| `S` | Toggle shuffle mode |
| `<` / `>` | Previous / next track |
| `[` / `]` | Volume down / up |

A now-playing bar shows at the bottom while audio plays. Set the starting volume
with `audio_volume` in [`config.toml`](CONFIG.md#behavior).

With **shuffle** on, a track that plays to its end chains into a random jukebox
post instead of stopping, indefinitely. Candidates come from the posts you have
already browsed, topped up now and then from the public feed. Pressing `S` with
nothing playing starts a random track right away; `s` stops the music and the
mode together. Picking a different track by hand keeps shuffle armed, and it
chains onward from whatever ends next.

`<` and `>` step through the play history, mpv-style: `<` replays earlier
tracks, `>` moves forward again, and at the newest entry `>` skips to a fresh
random pick from the same pool shuffle uses.
