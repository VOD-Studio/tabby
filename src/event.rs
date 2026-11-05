use std::time::Duration;

use color_eyre::Result;
use crossterm::event::EventStream;
use futures_util::StreamExt;
use ratatui::DefaultTerminal;
use tokio::{
    sync::mpsc,
    time::{MissedTickBehavior, interval},
};

use crate::app::{App, Msg, action_for, draw};

/// Render cadence for repaints deferred by background messages.
const TICK: Duration = Duration::from_millis(100);

/// Capacity of the background-task message channel.
const MSG_BUFFER: usize = 256;

/// Main loop: turns terminal events into actions, applies background messages,
/// and repaints.
///
/// Terminal events repaint immediately. Background messages only mark the app
/// dirty; the next tick paints once for the whole batch.
pub async fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let (_msg_tx, mut msg_rx) = mpsc::channel::<Msg>(MSG_BUFFER);

    terminal.draw(|frame| draw(frame, app))?;
    while !app.should_quit() {
        tokio::select! {
            _ = tick.tick() => {
                if app.take_dirty() {
                    terminal.draw(|frame| draw(frame, app))?;
                }
            }
            Some(msg) = msg_rx.recv() => match msg {},
            event = events.next() => {
                let Some(event) = event else { break };
                if let Some(action) = action_for(&event?) {
                    app.update(action);
                }
                if app.take_dirty() {
                    terminal.draw(|frame| draw(frame, app))?;
                }
            }
        }
    }
    Ok(())
}
