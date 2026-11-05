mod app;
mod config;
mod event;

use app::App;
use color_eyre::Result;
use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let settings = Config::load()?.resolve()?;
    let mut terminal = ratatui::init();
    let result = event::run(&mut terminal, &mut App::new(settings)).await;
    ratatui::restore();
    result
}
