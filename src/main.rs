mod api;
mod app;
mod config;
mod event;
mod format;
mod themes;
mod ui;

use std::{io, panic};

use api::client::ApiClient;
use app::App;
use clap::Parser;
use color_eyre::{
    Result,
    eyre::{WrapErr, eyre},
};
use config::{Config, Overrides, Profiles};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
};
use directories::ProjectDirs;
use tokio::sync::mpsc;
use tracing::level_filters::LevelFilter;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

/// Terminal UI for the Clash / mihomo External Controller.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(flatten)]
    overrides: Overrides,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    color_eyre::install()?;
    init_logging()?;
    let path = config::path();
    let file = Config::load(path.as_deref())?;
    let settings = file.resolve(&cli.overrides)?;
    let picker = themes::Picker {
        dir: config::themes_dir(),
        config: path.clone(),
        ..themes::Picker::default()
    };
    let theme = themes::load(&settings.theme, picker.dir.as_deref())?;
    let profiles = Profiles {
        path,
        ..file.profiles(&cli.overrides, &settings)
    };
    tracing::info!(
        profile = %settings.profile,
        url = %settings.url,
        theme = %settings.theme,
        "starting tabby {}",
        env!("CARGO_PKG_VERSION"),
    );
    let client = ApiClient::new(&settings.url, settings.secret.as_deref())?;
    let (msg_tx, msg_rx) = mpsc::channel(event::MSG_BUFFER);

    let mouse = settings.mouse;
    let mut terminal = ratatui::init();
    if mouse {
        if let Err(err) = execute!(io::stdout(), EnableMouseCapture) {
            ratatui::restore();
            return Err(err).wrap_err("cannot turn on mouse reporting");
        }
        release_mouse_on_panic();
    }
    let result = match terminal.size() {
        Ok(size) => {
            let mut app = App::new(settings, theme, client, msg_tx, size, profiles, picker);
            app.start();
            event::run(&mut terminal, &mut app, msg_rx).await
        }
        Err(err) => Err(err.into()),
    };
    if mouse && let Err(err) = execute!(io::stdout(), DisableMouseCapture) {
        tracing::warn!("cannot turn off mouse reporting: {err}");
    }
    ratatui::restore();
    result
}

/// Makes a panic turn mouse reporting off before anything else: the hook
/// `ratatui::init` installed restores the screen but leaves it on, and the
/// shell would then receive the pointer's escape sequences as input.
fn release_mouse_on_panic() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        // Nothing can be done about a failure while panicking.
        let _ = execute!(io::stdout(), DisableMouseCapture);
        previous(info);
    }));
}

/// Sends tracing output to `tabby.log` in the state directory (stdout belongs
/// to the TUI), and records panics there too.
///
/// Filtering follows `RUST_LOG`, defaulting to `info`.
fn init_logging() -> Result<()> {
    let dirs = ProjectDirs::from("", "", "tabby")
        .ok_or_else(|| eyre!("cannot determine the home directory"))?;
    let dir = dirs.state_dir().unwrap_or_else(|| dirs.data_local_dir());
    let file = RollingFileAppender::builder()
        .rotation(Rotation::NEVER)
        .filename_prefix("tabby.log")
        .build(dir)
        .wrap_err_with(|| format!("cannot open log file in {}", dir.display()))?;
    tracing_subscriber::fmt()
        .with_writer(file)
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    // Installed before `ratatui::init`, which wraps it to restore the terminal first.
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        previous(info);
    }));
    Ok(())
}
