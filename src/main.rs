use color_eyre::Result;
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::{DefaultTerminal, widgets::Paragraph};

fn main() -> Result<()> {
    color_eyre::install()?;
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal) -> Result<()> {
    terminal.draw(|frame| {
        frame.render_widget(Paragraph::new("tabby - press any key to exit"), frame.area());
    })?;
    loop {
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            return Ok(());
        }
    }
}
