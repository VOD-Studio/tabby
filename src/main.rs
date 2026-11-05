mod app;
mod event;

use app::App;
use color_eyre::Result;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let mut terminal = ratatui::init();
    let result = event::run(&mut terminal, &mut App::default()).await;
    ratatui::restore();
    result
}
