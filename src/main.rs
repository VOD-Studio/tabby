mod api;
mod app;
mod config;
mod event;
mod format;
mod themes;
mod ui;

use std::panic;

use api::client::ApiClient;
use app::App;
use clap::Parser;
use color_eyre::{
    Result,
    eyre::{WrapErr, eyre},
};
use config::{Config, Overrides, Profiles};
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
    let theme = themes::load(&settings.theme, config::themes_dir().as_deref())?;
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

    let mut terminal = ratatui::init();
    let result = match terminal.size() {
        Ok(size) => {
            let mut app = App::new(settings, theme, client, msg_tx, size, profiles);
            app.start();
            event::run(&mut terminal, &mut app, msg_rx).await
        }
        Err(err) => Err(err.into()),
    };
    ratatui::restore();
    result
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
