//! Open a URL with the operating system's default handler (browser).
//!
//! Used for the jukebox "open in browser" action: cyberspace.online streams
//! audio in the browser, and the TUI can't embed that player, so handing the
//! link to the desktop is the faithful equivalent of the web "Open" button.
use std::io;
use std::process::{Command, Stdio};

/// Open `url` with the OS default handler, detached (we don't wait for the
/// browser to exit). Returns the spawn error if the opener binary is missing or
/// fails to launch.
pub fn open_url(url: &str) -> io::Result<()> {
    opener_command(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Build the platform-appropriate opener command: `open` on macOS, `cmd /C
/// start` on Windows, `xdg-open` elsewhere. The URL is always the final
/// argument. `cfg!` (not `#[cfg]`) keeps every branch compiling so the build
/// fails loudly if a target's opener is wrong, and so this stays unit-testable.
fn opener_command(url: &str) -> Command {
    let (program, prefix_args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("open", &[])
    } else if cfg!(target_os = "windows") {
        // The empty "" is `start`'s window-title argument; without it a quoted
        // URL would be mistaken for the title.
        ("cmd", &["/C", "start", ""])
    } else {
        ("xdg-open", &[])
    };
    let mut cmd = Command::new(program);
    cmd.args(prefix_args).arg(url);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opener_command_puts_the_url_last_and_names_a_program() {
        let url = "https://youtu.be/dQw4w9WgXcQ";
        let cmd = opener_command(url);
        assert!(!cmd.get_program().is_empty(), "a program must be chosen");
        let last = cmd.get_args().last().map(|a| a.to_string_lossy().into_owned());
        assert_eq!(last.as_deref(), Some(url), "url must be the final argument");
    }
}
