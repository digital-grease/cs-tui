//! Scroll-wheel mouse reporting (`--mouse` / `mouse = true`), and whether it is
//! on.
//!
//! Recorded so an external editor can be handed a terminal without it: with
//! reporting left on, every wheel notch or click arrives in an editor that does
//! not read the mouse (nano) as escape-sequence text typed into the draft. And
//! an editor that turns reporting off on exit (vim with `mouse=a`) would leave
//! the wheel dead for the rest of the session unless it is put back.
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the reader asked for mouse capture this session.
static CAPTURED: AtomicBool = AtomicBool::new(false);

/// Turn reporting on or off for the session, and remember which.
pub fn set_captured(on: bool) {
    CAPTURED.store(on, Ordering::Relaxed);
    write_reporting(on);
}

/// Whether the session captures the mouse.
pub fn is_captured() -> bool {
    CAPTURED.load(Ordering::Relaxed)
}

/// Toggle xterm button + SGR mouse reporting (modes 1000/1006). Crossterm's
/// `EnableMouseCapture` also enables motion tracking, which we don't want.
/// Does not change what the session asked for; see [`set_captured`].
pub fn write_reporting(on: bool) {
    let seq: &[u8] = if on {
        b"\x1b[?1000h\x1b[?1006h"
    } else {
        b"\x1b[?1006l\x1b[?1000l"
    };
    let mut out = std::io::stdout();
    let _ = out.write_all(seq);
    let _ = out.flush();
}
