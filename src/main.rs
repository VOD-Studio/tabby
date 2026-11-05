mod app;
mod config;
mod event;

use app::App;
use clap::Parser;
use color_eyre::Result;
use config::{Config, Overrides};

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
    let settings = Config::load()?.resolve(&cli.overrides)?;
    let mut terminal = ratatui::init();
    let result = event::run(&mut terminal, &mut App::new(settings)).await;
    ratatui::restore();
    result
}
