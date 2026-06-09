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
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use super::app::BgEvent;
use super::theme::Theme;

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
    let mut ipc = connect_ipc(&sock).await;
    // Poll for mpv's own exit (track ended / external quit). The futures in the
    // select borrow `rx` and the timer, never `child`, so the arm bodies are
    // free to call `child` without a borrow conflict.
    let mut tick = tokio::time::interval(Duration::from_millis(300));
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(Cmd::TogglePause) => {
                    write_cmd(&mut ipc, r#"{"command":["cycle","pause"]}"#).await;
                }
                Some(Cmd::Volume(delta)) => {
                    write_cmd(&mut ipc, &format!(r#"{{"command":["add","volume",{delta}]}}"#)).await;
                }
                // Explicit stop, or the handle was dropped: ask mpv to quit, then
                // make sure it's gone.
                Some(Cmd::Stop) | None => {
                    write_cmd(&mut ipc, r#"{"command":["quit"]}"#).await;
                    let _ = child.start_kill();
                    break;
                }
            },
            _ = tick.tick() => {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
            }
        }
    }
    // Reap and clean up, then tell the UI this generation is done.
    let _ = child.wait().await;
    #[cfg(unix)]
    let _ = std::fs::remove_file(&sock);
    let _ = bg_tx.send(BgEvent::PlaybackEnded { token });
}

/// Send a single newline-terminated JSON command to mpv. Fire-and-forget: we
/// never read replies (the commands we use don't need them).
async fn write_cmd(ipc: &mut Option<Box<dyn tokio::io::AsyncWrite + Unpin + Send>>, json: &str) {
    if let Some(conn) = ipc.as_mut() {
        let _ = conn.write_all(json.as_bytes()).await;
        let _ = conn.write_all(b"\n").await;
        let _ = conn.flush().await;
    }
}

/// Connect to mpv's IPC endpoint, retrying briefly since the socket/pipe appears
/// a moment after mpv launches. Returns `None` if it never shows (controls then
/// no-op, but playback and end-detection still work).
async fn connect_ipc(sock: &Path) -> Option<Box<dyn tokio::io::AsyncWrite + Unpin + Send>> {
    for _ in 0..50 {
        #[cfg(unix)]
        {
            if let Ok(stream) = tokio::net::UnixStream::connect(sock).await {
                return Some(Box::new(stream));
            }
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ClientOptions;
            if let Ok(pipe) = ClientOptions::new().open(sock) {
                return Some(Box::new(pipe));
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

/// Render the now-playing bar (a single row). Shows the track, play/pause state,
/// volume, and the control keys.
pub fn render_bar(frame: &mut Frame<'_>, area: Rect, handle: &Handle, theme: &Theme) {
    let state = if handle.paused { "⏸" } else { "▶" };
    let track = match (handle.title.is_empty(), handle.artist.is_empty()) {
        (false, false) => format!("{} · {}", handle.title, handle.artist),
        (false, true) => handle.title.clone(),
        (true, false) => handle.artist.clone(),
        (true, true) => "jukebox".to_string(),
    };
    let resume_or_pause = if handle.paused { "p resume" } else { "p pause" };
    let spans = vec![
        Span::styled(
            format!("♪ {state} "),
            theme.accent_style().add_modifier(Modifier::BOLD),
        ),
        Span::styled(track, theme.base()),
        Span::styled(
            format!(
                "  ·  {resume_or_pause} · s stop · [ ] vol {}%",
                handle.volume
            ),
            theme.muted_style(),
        ),
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
            tx,
        }
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
