//! Open a URL with the operating system's default handler, or with the
//! `browser` command the reader configured.
//!
//! Used for the jukebox "open in browser" action: cyberspace.online streams
//! audio in the browser, and the TUI can't embed that player, so handing the
//! link to the desktop is the faithful equivalent of the web "Open" button.
//!
//! **The scheme check runs before the handler is chosen**, so configuring a
//! `browser` cannot widen what is openable. A reader who points this at
//! something that is not a browser has pointed it there deliberately; a URL
//! arriving in someone else's message still cannot reach it unless it is http
//! or https.
use std::io;
use std::process::{Command, Stdio};

/// Open `url` with the configured `browser`, or the OS default handler when
/// there is none, detached (we don't wait for the browser to exit).
///
/// # Errors
///
/// If the URL is not http(s), or the chosen program is missing or fails to
/// launch.
pub fn open_url(url: &str) -> io::Result<()> {
    open_url_with(url, crate::config::get().browser.as_deref())
}

/// Open `url`, preferring `browser` over the OS default handler.
///
/// Split from [`open_url`] so it can be tested: [`open_url`] reads the
/// process-wide runtime config, which is a `OnceLock` a test cannot set.
///
/// # Errors
///
/// If the URL is not http(s), or the chosen program fails to launch.
pub fn open_url_with(url: &str, browser: Option<&str>) -> io::Result<()> {
    // Attachment URLs arrive in other people's messages, so the scheme is not
    // ours to trust. Without this the raw string went to `xdg-open` (or
    // `cmd /C start`), which will happily act on `file://`, `smb://` or any
    // custom handler the desktop has registered. Only the two schemes a link in
    // a message is ever legitimately going to use are handed over.
    if !is_openable(url) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to open a non-http(s) url",
        ));
    }
    command_for(url, browser)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// The command that will open `url`: the configured browser if there is a
/// usable one, otherwise the OS default handler.
fn command_for(url: &str, browser: Option<&str>) -> Command {
    browser
        .and_then(|b| browser_command(b, url))
        .unwrap_or_else(|| opener_command(url))
}

/// Build a command from a configured `browser` string.
///
/// Split on whitespace and run directly. **There is no shell**, so nothing here
/// is quoted, globbed, or word-split by anything but us, and a URL cannot inject
/// a second command however it is spelled: it is one argument, always.
///
/// `%s` in any argument is replaced by the URL, for the browsers that want the
/// link somewhere other than last (`qutebrowser --target tab %s`). Without a
/// `%s` the URL is appended, which is what most of them want.
///
/// `None` when the setting has no program in it at all, so a whitespace-only
/// value falls back to the OS handler rather than failing to spawn on every
/// link.
fn browser_command(browser: &str, url: &str) -> Option<Command> {
    let mut parts = browser.split_whitespace();
    let program = parts.next()?;
    let mut cmd = Command::new(program);
    let mut substituted = false;
    for arg in parts {
        if arg.contains("%s") {
            cmd.arg(arg.replace("%s", url));
            substituted = true;
        } else {
            cmd.arg(arg);
        }
    }
    if !substituted {
        cmd.arg(url);
    }
    Some(cmd)
}

/// Whether `url` is something we will hand to a handler at all.
///
/// http and https only. Deliberately narrower than the set
/// [`super::hyperlink`] will *linkify*, because making text clickable is the
/// terminal's business and launching a handler is the operating system's.
#[must_use]
pub fn is_openable(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
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
    fn only_http_urls_are_handed_to_the_desktop() {
        // These arrive in other people's messages; `xdg-open` acts on whatever
        // handler the scheme names.
        for ok in [
            "https://example.com/a.png",
            "http://example.com/a.png",
            "HTTPS://EXAMPLE.COM/A.PNG",
        ] {
            assert!(is_openable(ok), "{ok}");
        }
        for bad in [
            "file:///etc/passwd",
            "smb://host/share",
            "javascript:alert(1)",
            "mailto:someone@example.com",
            "",
            "  ",
            "/etc/passwd",
        ] {
            assert!(!is_openable(bad), "{bad}");
            assert!(open_url(bad).is_err(), "{bad} must be refused");
        }
    }

    /// The program plus its arguments, for asserting on a built command.
    fn parts(cmd: &Command) -> (String, Vec<String>) {
        (
            cmd.get_program().to_string_lossy().into_owned(),
            cmd.get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        )
    }

    #[test]
    fn a_configured_browser_replaces_the_os_handler() {
        let url = "https://example.com/a.png";
        let (prog, args) = parts(&command_for(url, Some("firefox --new-tab")));
        assert_eq!(prog, "firefox");
        assert_eq!(args, vec!["--new-tab", url]);
    }

    #[test]
    fn a_percent_s_places_the_url_instead_of_appending_it() {
        // Some browsers want the link before their own trailing arguments.
        let url = "https://example.com/a.png";
        let (prog, args) = parts(&command_for(url, Some("qutebrowser %s --target tab")));
        assert_eq!(prog, "qutebrowser");
        assert_eq!(args, vec![url, "--target", "tab"]);
        // and it must not ALSO be appended
        assert_eq!(args.iter().filter(|a| *a == url).count(), 1);
    }

    #[test]
    fn a_blank_browser_falls_back_to_the_os_handler() {
        // "" and "   " mean unset, not "spawn nothing". Failing to spawn on
        // every link, with no message saying why, is the worse reading.
        let url = "https://example.com/a.png";
        for blank in [Some(""), Some("   "), Some("\t"), None] {
            let (prog, _) = parts(&command_for(url, blank));
            let (want, _) = parts(&opener_command(url));
            assert_eq!(prog, want, "{blank:?} should fall back");
        }
    }

    #[test]
    fn a_configured_browser_cannot_widen_what_is_openable() {
        // The scheme check runs before the handler is chosen, so pointing
        // `browser` at something else does not make `file://` reachable from a
        // link in someone else's message.
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "smb://h/s"] {
            assert!(open_url_with(bad, Some("firefox")).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_url_stays_one_argument_however_it_is_spelled() {
        // There is no shell, so this is structural rather than a matter of
        // escaping: whatever the URL contains, it arrives as a single argv
        // entry. A URL with a space in it cannot become two arguments, and one
        // with a semicolon cannot become a second command.
        let url = "https://example.com/a b;rm -rf /";
        let (_, args) = parts(&command_for(url, Some("firefox")));
        assert_eq!(args, vec![url]);
        let (_, args) = parts(&command_for(url, Some("qutebrowser %s")));
        assert_eq!(args, vec![url]);
    }

    #[test]
    fn opener_command_puts_the_url_last_and_names_a_program() {
        let url = "https://youtu.be/dQw4w9WgXcQ";
        let cmd = opener_command(url);
        assert!(!cmd.get_program().is_empty(), "a program must be chosen");
        let last = cmd
            .get_args()
            .last()
            .map(|a| a.to_string_lossy().into_owned());
        assert_eq!(last.as_deref(), Some(url), "url must be the final argument");
    }
}
