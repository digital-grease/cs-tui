use anyhow::{Context, Result};
use clap::Parser;
use cs_api::Client;

mod prefs;
mod session;
mod ui;

use prefs::Prefs;
use session::Session;
use ui::{theme::ThemeKind, App};

#[derive(Debug, Parser)]
#[command(version, about = "TUI client for cyberspace.online")]
struct Cli {
    /// Override the API base URL.
    #[arg(long, env = "CS_TUI_API_BASE")]
    api_base: Option<String>,

    /// Color theme: cyber (default), c64, vt320, or dark. Overrides the saved
    /// preference for this run; the theme is also remembered between runs.
    #[arg(long, env = "CS_TUI_THEME")]
    theme: Option<String>,

    /// Run against the in-memory mock client (no network). Not yet implemented.
    #[arg(long)]
    mock: bool,

    /// Verbose tracing to stderr (RUST_LOG-compatible).
    #[arg(long)]
    debug: bool,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    init_tracing(cli.debug);

    if cli.mock {
        anyhow::bail!("--mock flag is not yet implemented (lands in phase 7)");
    }

    let client = build_client(cli.api_base.as_deref()).context("build api client")?;

    let saved = match Session::load() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "session load failed; starting fresh");
            None
        }
    };

    let prefill_email = saved.as_ref().map(|s| s.email.clone()).unwrap_or_default();
    let mut has_session = false;
    if let Some(s) = saved {
        if s.tokens.is_authenticated() {
            client.set_tokens(s.tokens).await;
            has_session = true;
        }
    }

    // Precedence: explicit --theme/$CS_TUI_THEME > saved prefs > default.
    let prefs = Prefs::load();
    let theme_kind = cli
        .theme
        .as_deref()
        .or(prefs.theme.as_deref())
        .map(ThemeKind::from_name)
        .unwrap_or(ThemeKind::Cyber);
    // Detect terminal image-graphics support before entering the alternate
    // screen (the query reads/writes stdio). `None` → images aren't rendered.
    let picker = match ratatui_image::picker::Picker::from_query_stdio() {
        Ok(p) => {
            tracing::info!(protocol = ?p.protocol_type(), "terminal image graphics detected");
            Some(p)
        }
        Err(e) => {
            tracing::info!(error = %e, "no terminal image graphics; using [image] placeholders");
            None
        }
    };

    let terminal = ratatui::init();
    let mut app = App::with_theme(client, prefill_email, theme_kind);
    app.set_image_picker(picker);
    if has_session {
        app.enter_feed_initial();
    }
    let run_result = app.run(terminal).await;
    ratatui::restore();

    run_result
}

fn init_tracing(verbose: bool) {
    use tracing_subscriber::{fmt, EnvFilter};
    let default_level = if verbose { "debug" } else { "warn" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(format!("cs_tui={default_level},cs_api={default_level}"))
    });

    // Logs go to a file under the XDG state dir (e.g. ~/.local/state/cs-tui/
    // cs-tui.log). Writing to stderr would corrupt the alternate-screen TUI, so
    // the file is the default sink; we only fall back to stderr if the log file
    // can't be opened (e.g. no home directory).
    match log_appender() {
        Some(appender) => {
            let _ = fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_ansi(false)
                .with_writer(appender)
                .try_init();
        }
        None => {
            let _ = fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_writer(std::io::stderr)
                .try_init();
        }
    }
}

/// Resolve the XDG state directory, create it, and return a blocking file
/// appender that writes `cs-tui.log`. `None` if no directory can be resolved.
fn log_appender() -> Option<tracing_appender::rolling::RollingFileAppender> {
    let dir = log_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(tracing_appender::rolling::never(dir, "cs-tui.log"))
}

/// The directory log files live in: the XDG state dir on Linux
/// (`~/.local/state/cs-tui`), falling back to the local data dir elsewhere.
fn log_dir() -> Option<std::path::PathBuf> {
    let dirs = directories::ProjectDirs::from("online", "cyberspace", "cs-tui")?;
    Some(
        dirs.state_dir()
            .unwrap_or_else(|| dirs.data_local_dir())
            .to_path_buf(),
    )
}

fn build_client(api_base: Option<&str>) -> Result<Client> {
    let mut b = Client::builder();
    if let Some(s) = api_base {
        b = b.base_url_str(s).context("invalid --api-base")?;
    }
    b.build().context("build cs_api client")
}
