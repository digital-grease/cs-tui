//! Background music playback via an external `mpv` process.
//!
//! cyberspace.online "jukebox" tracks are YouTube links. A terminal can't embed
//! a video player, but `mpv` (with `yt-dlp` on PATH, which it invokes to resolve
//! YouTube URLs) plays the audio cleanly in the background. We spawn it with an
//! IPC socket and drive pause/stop/volume over that socket; the UI shows a
//! now-playing bar. This is local-only by nature — cs-tui isn't hosted over SSH,
//! so the audio always plays on the user's own machine.
//!
//! Design: [`play`] spawns mpv and returns a [`Handle`] immediately. A detached
//! task connects to the IPC socket (it appears a moment after launch), then
//! loops handling control commands and polling for mpv's exit. When mpv exits —
//! track finished, stopped, or crashed — it emits [`BgEvent::PlaybackEnded`] so
//! the UI can clear the bar.
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

use super::app::BgEvent;
use super::theme::Theme;

/// Boxed read/write halves of the IPC connection (unix socket or Windows pipe).
type IpcWriter = Box<dyn tokio::io::AsyncWrite + Unpin + Send>;
type IpcReader = tokio::io::BufReader<Box<dyn tokio::io::AsyncRead + Unpin + Send>>;

/// Volume bounds. mpv accepts 0..=130 (above 100 is soft amplification); we keep
/// the same range so the displayed percentage matches what mpv applies.
const VOLUME_MIN: i64 = 0;
const VOLUME_MAX: i64 = 130;

/// Starting volume for a fresh session. Deliberately conservative: mpv at 100%
/// of a YouTube source is loud, so we open at half and let `[`/`]` adjust.
pub const DEFAULT_VOLUME: i64 = 50;

/// Control messages sent from the UI to the per-playback task.
enum Cmd {
    TogglePause,
    Volume(i64),
    Stop,
}

/// What the UI holds while a track is loaded. Dropping it stops playback (the
/// command channel closes and the task kills mpv).
pub struct Handle {
    /// The track being played, for the now-playing bar.
    pub artist: String,
    pub title: String,
    /// The source URL, so the UI can tell "play this" from "toggle the current".
    pub url: String,
    /// Generation id; [`BgEvent::PlaybackEnded`] carries it so a stale exit
    /// notification can't clear a newer track.
    pub token: u64,
    /// UI-side mirror of mpv's pause state (we drive it, so we track it).
    pub paused: bool,
    /// UI-side mirror of mpv's volume (0..=130).
    pub volume: i64,
    /// Latest playback position in seconds, polled from mpv (0 until known).
    pub position_secs: f64,
    /// Track length in seconds, polled from mpv (0 until known, or for a stream
    /// with no duration). Drives the progress gauge's denominator.
    pub duration_secs: f64,
    tx: mpsc::UnboundedSender<Cmd>,
}

impl Handle {
    /// Toggle pause and reflect it locally for the bar.
    pub fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        let _ = self.tx.send(Cmd::TogglePause);
    }

    /// Step the volume by `delta`, clamped to mpv's range.
    pub fn step_volume(&mut self, delta: i64) {
        let next = (self.volume + delta).clamp(VOLUME_MIN, VOLUME_MAX);
        let applied = next - self.volume;
        self.volume = next;
        if applied != 0 {
            let _ = self.tx.send(Cmd::Volume(applied));
        }
    }

    /// Stop playback (kills mpv). The task will still emit `PlaybackEnded`.
    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }
}

/// Whether `mpv` is on PATH and runnable. Cheap-ish (spawns `mpv --version`);
/// callers should memoize. `yt-dlp` is mpv's concern at playback time — we let a
/// resolution failure surface as a track that simply ends quickly rather than
/// probing for it here.
#[must_use]
pub fn mpv_available() -> bool {
    binary_runs("mpv")
}

/// Whether a YouTube resolver (`yt-dlp`, or legacy `youtube-dl`) is on PATH. mpv
/// shells out to one to play YouTube URLs; without it, YouTube playback fails the
/// instant it starts, so the UI checks this to warn instead of flashing the bar.
#[must_use]
pub fn ytdlp_available() -> bool {
    binary_runs("yt-dlp") || binary_runs("youtube-dl")
}

/// Whether `bin --version` runs and exits successfully (a PATH/runnability probe).
fn binary_runs(bin: &str) -> bool {
    std::process::Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Per-playback IPC socket path. Includes the process id so two cs-tui instances
/// (or a stale file from a prior run) never collide. On Windows this is a named
/// pipe path; elsewhere a unix-socket file in the temp dir.
fn socket_path(token: u64) -> PathBuf {
    let pid = std::process::id();
    if cfg!(windows) {
        PathBuf::from(format!(r"\\.\pipe\cs-tui-mpv-{pid}-{token}"))
    } else {
        std::env::temp_dir().join(format!("cs-tui-mpv-{pid}-{token}.sock"))
    }
}

/// Start playing `url` (artist/title are for the bar). Spawns mpv and returns a
/// handle immediately; a background task owns the process and IPC. `bg_tx`
/// receives [`BgEvent::PlaybackEnded`] when playback ends. `volume` seeds mpv's
/// starting volume so it carries across tracks.
pub fn play(
    url: &str,
    artist: String,
    title: String,
    token: u64,
    volume: i64,
    bg_tx: mpsc::UnboundedSender<BgEvent>,
) -> std::io::Result<Handle> {
    let sock = socket_path(token);
    // A stale socket file would make mpv refuse to bind; clear it first.
    #[cfg(unix)]
    let _ = std::fs::remove_file(&sock);

    let child = Command::new("mpv")
        .arg("--no-video")
        .arg("--no-terminal")
        .arg("--really-quiet")
        .arg("--idle=no")
        .arg(format!("--volume={volume}"))
        .arg(format!("--input-ipc-server={}", sock.display()))
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;

    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(child, rx, sock, token, bg_tx));

    Ok(Handle {
        artist,
        title,
        url: url.to_string(),
        token,
        paused: false,
        volume,
        position_secs: 0.0,
        duration_secs: 0.0,
        tx,
    })
}

/// The per-playback task: connect IPC, then handle commands and watch for exit.
async fn run(
    mut child: Child,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    sock: PathBuf,
    token: u64,
    bg_tx: mpsc::UnboundedSender<BgEvent>,
) {
    let (mut reader, mut writer) = match connect_ipc(&sock).await {
        Some((r, w)) => (Some(r), Some(w)),
        None => (None, None),
    };
    let mut reader_done = reader.is_none();
    // Last known track length, carried so a `time-pos` reply can be paired with
    // the gauge denominator from the preceding `duration` reply.
    let mut duration = 0.0f64;
    // A 1s tick drives both progress polling (one bar redraw per second is plenty
    // for a gauge) and exit-detection (try_wait), so a finished track clears the
    // bar within ~1s; an explicit stop is instant via the command channel. The
    // select futures borrow `rx`, the timer, and `reader` — never `child` or
    // `writer` — so the arm bodies use those freely without a borrow conflict.
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(Cmd::TogglePause) => {
                    write_cmd(&mut writer, r#"{"command":["cycle","pause"]}"#).await;
                }
                Some(Cmd::Volume(delta)) => {
                    write_cmd(&mut writer, &format!(r#"{{"command":["add","volume",{delta}]}}"#)).await;
                }
                // Explicit stop, or the handle was dropped: ask mpv to quit, then
                // make sure it's gone.
                Some(Cmd::Stop) | None => {
                    write_cmd(&mut writer, r#"{"command":["quit"]}"#).await;
                    let _ = child.start_kill();
                    break;
                }
            },
            _ = tick.tick() => {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                // Ask for the track length first, then the position, so a position
                // reply lands with the gauge denominator already updated.
                write_cmd(&mut writer, r#"{"command":["get_property","duration"],"request_id":2}"#).await;
                write_cmd(&mut writer, r#"{"command":["get_property","time-pos"],"request_id":1}"#).await;
            }
            line = next_line(&mut reader), if !reader_done => match line {
                Some(l) => match parse_progress(&l) {
                    Some(Progress::Duration(d)) => duration = d,
                    Some(Progress::Position(p)) => {
                        let _ = bg_tx.send(BgEvent::PlaybackProgress {
                            token,
                            position_secs: p,
                            duration_secs: duration,
                        });
                    }
                    None => {}
                },
                // EOF (mpv closed the socket): stop reading so the branch goes idle
                // instead of busy-looping. `try_wait` will catch the exit.
                None => reader_done = true,
            }
        }
    }
    // Reap and clean up, then tell the UI this generation is done.
    let _ = child.wait().await;
    #[cfg(unix)]
    let _ = std::fs::remove_file(&sock);
    let _ = bg_tx.send(BgEvent::PlaybackEnded { token });
}

/// Read one newline-delimited reply from mpv. Resolves to `None` on EOF/error;
/// stays pending forever when there's no reader, so the select branch (guarded by
/// `!reader_done`) simply never fires in that case.
async fn next_line(reader: &mut Option<IpcReader>) -> Option<String> {
    match reader {
        Some(r) => {
            let mut line = String::new();
            match r.read_line(&mut line).await {
                Ok(0) | Err(_) => None,
                Ok(_) => Some(line),
            }
        }
        None => std::future::pending::<Option<String>>().await,
    }
}

/// A parsed progress reply from mpv (`get_property` for `duration` / `time-pos`).
enum Progress {
    Duration(f64),
    Position(f64),
}

/// Parse one mpv IPC reply line. Returns `None` for async events, replies with no
/// numeric `data` (e.g. "property unavailable" while yt-dlp is still resolving),
/// and anything unrecognized.
fn parse_progress(line: &str) -> Option<Progress> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let data = v.get("data")?.as_f64()?;
    if !data.is_finite() {
        return None;
    }
    match v.get("request_id")?.as_i64()? {
        2 => Some(Progress::Duration(data)),
        1 => Some(Progress::Position(data)),
        _ => None,
    }
}

/// Send a single newline-terminated JSON command to mpv. Fire-and-forget for
/// control commands; the polling commands' replies are read separately.
async fn write_cmd(writer: &mut Option<IpcWriter>, json: &str) {
    if let Some(w) = writer.as_mut() {
        let _ = w.write_all(json.as_bytes()).await;
        let _ = w.write_all(b"\n").await;
        let _ = w.flush().await;
    }
}

/// Connect to mpv's IPC endpoint, retrying briefly since the socket/pipe appears
/// a moment after mpv launches. Returns the read+write halves, or `None` if it
/// never shows (controls/progress then no-op, but playback + end-detection still
/// work).
async fn connect_ipc(sock: &Path) -> Option<(IpcReader, IpcWriter)> {
    for _ in 0..50 {
        #[cfg(unix)]
        {
            if let Ok(stream) = tokio::net::UnixStream::connect(sock).await {
                let (r, w) = stream.into_split();
                let r: Box<dyn tokio::io::AsyncRead + Unpin + Send> = Box::new(r);
                let w: IpcWriter = Box::new(w);
                return Some((tokio::io::BufReader::new(r), w));
            }
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ClientOptions;
            if let Ok(pipe) = ClientOptions::new().open(sock) {
                let (r, w) = tokio::io::split(pipe);
                let r: Box<dyn tokio::io::AsyncRead + Unpin + Send> = Box::new(r);
                let w: IpcWriter = Box::new(w);
                return Some((tokio::io::BufReader::new(r), w));
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

/// Format a second count as `m:ss` (e.g. 67.0 → "1:07"). Tracks over an hour
/// just keep counting minutes, which is fine for songs.
fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Render the now-playing bar (a single row): track, play/pause state, elapsed /
/// total time, a progress gauge (when there's room and a known duration), and the
/// control keys. The gauge is dropped on narrow terminals so the line never wraps.
pub fn render_bar(frame: &mut Frame<'_>, area: Rect, handle: &Handle, theme: &Theme) {
    let state = if handle.paused { "⏸" } else { "▶" };
    let track = match (handle.title.is_empty(), handle.artist.is_empty()) {
        (false, false) => format!("{} · {}", handle.title, handle.artist),
        (false, true) => handle.title.clone(),
        (true, false) => handle.artist.clone(),
        (true, true) => "jukebox".to_string(),
    };
    let resume_or_pause = if handle.paused { "p resume" } else { "p pause" };

    let left = format!("♪ {state} ");
    let controls = format!("  ·  {resume_or_pause} · s stop · [ ] vol {}%", handle.volume);

    let dur = handle.duration_secs;
    let pos = if dur > 0.0 {
        handle.position_secs.clamp(0.0, dur)
    } else {
        handle.position_secs.max(0.0)
    };
    // Time readout: "elapsed / total" when the duration is known, just the elapsed
    // for a stream with none, and nothing until mpv reports a position.
    let time = if dur > 0.0 {
        format!("  {} / {}", fmt_time(pos), fmt_time(dur))
    } else if handle.position_secs > 0.0 {
        format!("  {}", fmt_time(pos))
    } else {
        String::new()
    };

    // Fit a gauge into whatever width is left, but only when a duration is known
    // and the rest of the bar already fits with room to spare.
    let fixed = left.width() + track.width() + time.width() + controls.width();
    let total = area.width as usize;
    let gauge = if dur > 0.0 && total > fixed + 8 {
        let cells = (total - fixed - 4).min(20); // interior, minus "  ▕" and "▏"
        let filled = (((pos / dur).clamp(0.0, 1.0) * cells as f64).round() as usize).min(cells);
        let bar = "█".repeat(filled) + &"░".repeat(cells - filled);
        format!("  ▕{bar}▏")
    } else {
        String::new()
    };

    let spans = vec![
        Span::styled(left, theme.accent_style().add_modifier(Modifier::BOLD)),
        Span::styled(track, theme.base()),
        Span::styled(time, theme.accent_style()),
        Span::styled(gauge, theme.muted_style()),
        Span::styled(controls, theme.muted_style()),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(volume: i64) -> Handle {
        let (tx, _rx) = mpsc::unbounded_channel();
        Handle {
            artist: "Art of Noise".into(),
            title: "Paranoimia".into(),
            url: "https://youtu.be/x".into(),
            token: 1,
            paused: false,
            volume,
            position_secs: 0.0,
            duration_secs: 0.0,
            tx,
        }
    }

    fn bar_text(h: &Handle, width: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let theme = Theme::cyber();
        let mut term = Terminal::new(TestBackend::new(width, 1)).unwrap();
        term.draw(|f| render_bar(f, f.area(), h, &theme)).unwrap();
        term.backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn fmt_time_formats_minutes_and_seconds() {
        assert_eq!(fmt_time(0.0), "0:00");
        assert_eq!(fmt_time(7.0), "0:07");
        assert_eq!(fmt_time(67.4), "1:07");
        assert_eq!(fmt_time(253.0), "4:13");
    }

    #[test]
    fn parse_progress_reads_duration_and_position_replies() {
        match parse_progress(r#"{"data":253.0,"request_id":2,"error":"success"}"#) {
            Some(Progress::Duration(d)) => assert!((d - 253.0).abs() < 1e-9),
            other => panic!("expected Duration, got {}", matches!(other, Some(Progress::Position(_)))),
        }
        match parse_progress(r#"{"data":67.4,"request_id":1,"error":"success"}"#) {
            Some(Progress::Position(p)) => assert!((p - 67.4).abs() < 1e-9),
            _ => panic!("expected Position"),
        }
        // Unavailable property (no data), an async event, and garbage → ignored.
        assert!(parse_progress(r#"{"request_id":1,"error":"property unavailable"}"#).is_none());
        assert!(parse_progress(r#"{"event":"playback-restart"}"#).is_none());
        assert!(parse_progress("not json").is_none());
    }

    #[test]
    fn bar_shows_time_and_gauge_when_duration_known() {
        let mut h = handle(50);
        h.position_secs = 67.0;
        h.duration_secs = 253.0;
        let text = bar_text(&h, 90);
        assert!(text.contains("1:07 / 4:13"), "time readout: {text:?}");
        assert!(text.contains('█'), "gauge filled cells: {text:?}");
        assert!(text.contains('░'), "gauge empty cells: {text:?}");
    }

    #[test]
    fn bar_hides_gauge_on_narrow_terminals() {
        let mut h = handle(50);
        h.position_secs = 67.0;
        h.duration_secs = 253.0;
        let text = bar_text(&h, 40); // too narrow for a gauge
        assert!(!text.contains('█'), "no gauge when cramped: {text:?}");
    }

    #[test]
    fn bar_omits_time_until_a_position_is_known() {
        let h = handle(50); // position/duration both 0
        let text = bar_text(&h, 90);
        assert!(!text.contains('/'), "no time readout yet: {text:?}");
        assert!(!text.contains('█'), "no gauge yet: {text:?}");
    }

    #[test]
    fn toggle_pause_flips_local_state() {
        let mut h = handle(100);
        assert!(!h.paused);
        h.toggle_pause();
        assert!(h.paused);
        h.toggle_pause();
        assert!(!h.paused);
    }

    #[test]
    fn step_volume_clamps_to_mpv_range() {
        let mut h = handle(100);
        h.step_volume(50); // 150 -> clamped to 130
        assert_eq!(h.volume, VOLUME_MAX);
        h.step_volume(-1000); // -> clamped to 0
        assert_eq!(h.volume, VOLUME_MIN);
    }

    #[test]
    fn socket_path_is_unique_per_token_and_process() {
        let a = socket_path(1);
        let b = socket_path(2);
        assert_ne!(a, b, "different tokens must not collide");
        assert!(
            a.to_string_lossy().contains(&std::process::id().to_string()),
            "path is namespaced by pid: {a:?}"
        );
    }
}
