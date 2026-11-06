use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};

use color_eyre::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream},
    execute,
};
use futures_util::StreamExt;
use ratatui::DefaultTerminal;
use tokio::{
    sync::mpsc,
    time::{Interval, MissedTickBehavior, interval},
};

use crate::{
    app::{App, Envelope, action_for},
    core::ManagedCore,
    ui::{self, shadow::Shadow},
};

/// Render cadence for repaints deferred by background messages.
const TICK: Duration = Duration::from_millis(100);

/// Capacity of the background-task message channel.
pub const MSG_BUFFER: usize = 256;

/// Main loop: turns terminal events into actions, applies background messages,
/// and repaints.
///
/// Terminal events repaint immediately. Background messages only mark the app
/// dirty; the next tick paints once for the whole batch.
///
/// A request of the app for the terminal (`sudo` has to ask for a password
/// on it) is served between two rounds of the loop: the event stream is
/// dropped, as it would read the keys meant for `sudo`, the screen is given
/// back, and tabby takes it again when the command has ended.
pub async fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    mut msg_rx: mpsc::Receiver<Envelope>,
) -> Result<()> {
    let mut tick = interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut shadow = Shadow::default();
    paint(terminal, app, &mut shadow)?;
    while !app.should_quit() {
        if let Some(core) = serve(terminal, app, &mut msg_rx, &mut tick, &mut shadow).await? {
            lend_terminal(terminal, app, &core, &mut shadow).await?;
        }
    }
    Ok(())
}

/// Runs the loop until the app quits, or asks for the terminal (then the
/// core it asks about is returned).
async fn serve(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    msg_rx: &mut mpsc::Receiver<Envelope>,
    tick: &mut Interval,
    shadow: &mut Shadow,
) -> Result<Option<Arc<ManagedCore>>> {
    let mut events = EventStream::new();
    while !app.should_quit() {
        tokio::select! {
            _ = tick.tick() => {
                app.on_tick(Instant::now());
                if app.take_dirty() {
                    paint(terminal, app, shadow)?;
                }
            }
            Some(envelope) = msg_rx.recv() => app.receive(envelope),
            event = events.next() => {
                let Some(event) = event else { return Ok(None) };
                let event = event?;
                let action = match event {
                    Event::Mouse(mouse) => app.mouse_action(mouse, Instant::now()),
                    other => action_for(&other, app.page, app.typing()),
                };
                if let Some(action) = action {
                    app.update(action);
                }
                if app.take_dirty() {
                    paint(terminal, app, shadow)?;
                }
            }
        }
        if let Some(core) = app.take_suspend() {
            return Ok(Some(core));
        }
    }
    Ok(None)
}

/// Gives the terminal to `sudo`, to ask for a password, and takes it back.
async fn lend_terminal(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    core: &Arc<ManagedCore>,
    shadow: &mut Shadow,
) -> Result<()> {
    let mouse = app.settings.mouse;
    if mouse && let Err(err) = execute!(io::stdout(), DisableMouseCapture) {
        tracing::warn!("cannot turn off mouse reporting: {err}");
    }
    ratatui::restore();
    // The screen is the shell's for now, not the TUI's.
    println!("tabby 要用 sudo 给内核加上网络权限（setcap），请输入密码：");
    let result = core.authorize_terminal().await;
    *terminal = ratatui::init();
    if mouse && let Err(err) = execute!(io::stdout(), EnableMouseCapture) {
        tracing::warn!("cannot turn on mouse reporting: {err}");
    }
    terminal.clear()?;
    // What the screen held is not what was last drawn.
    *shadow = Shadow::default();
    app.finish_terminal(result);
    paint(terminal, app, shadow)
}

/// Draws `app`, repainting the cells [`Shadow`] finds cut by the last frame.
fn paint(terminal: &mut DefaultTerminal, app: &App, shadow: &mut Shadow) -> Result<()> {
    let frame = terminal.draw(|frame| {
        ui::draw(frame, app);
        shadow.mark(frame.buffer_mut());
    })?;
    shadow.remember(frame.buffer);
    Ok(())
}
