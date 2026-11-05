mod event;

use color_eyre::Result;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let mut terminal = ratatui::init();
    let result = event::run(&mut terminal).await;
    ratatui::restore();
    result
}
