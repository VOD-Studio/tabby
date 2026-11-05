use std::time::Duration;

use color_eyre::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::{DefaultTerminal, Frame, widgets::Paragraph};
use tokio::time::{MissedTickBehavior, interval};

/// Redraw cadence for deferred (coalesced) repaints.
const TICK: Duration = Duration::from_millis(100);

/// Main loop: multiplexes terminal events and the render tick.
///
/// Keys repaint nothing yet; resize only marks the screen dirty so bursts of
/// resize events collapse into one repaint on the next tick.
pub async fn run(terminal: &mut DefaultTerminal) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut dirty = false;

    terminal.draw(draw)?;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if dirty {
                    terminal.draw(draw)?;
                    dirty = false;
                }
            }
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                    let ctrl_c = key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL);
                    if ctrl_c || key.code == KeyCode::Char('q') {
                        return Ok(());
                    }
                }
                Some(Ok(Event::Resize(..))) => dirty = true,
                Some(Ok(_)) => {}
                Some(Err(err)) => return Err(err.into()),
                None => return Ok(()),
            },
        }
    }
}

fn draw(frame: &mut Frame) {
    frame.render_widget(Paragraph::new("tabby - press q to quit"), frame.area());
}
