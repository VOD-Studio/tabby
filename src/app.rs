use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{Frame, text::Text, widgets::Paragraph};
use tokio::sync::mpsc;

use crate::{
    api::{client::ApiClient, error::ApiError},
    config::Settings,
};

/// A user intent, derived from a terminal event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Resize,
}

/// A message from a background task.
#[derive(Debug)]
pub enum Msg {
    /// Raw `GET /version` reply from the startup probe.
    Version(Result<serde_json::Value, ApiError>),
}

/// Maps a terminal event to an [`Action`]; `None` means the event is ignored.
pub fn action_for(event: &Event) -> Option<Action> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            let ctrl_c =
                key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
            (ctrl_c || key.code == KeyCode::Char('q')).then_some(Action::Quit)
        }
        Event::Resize(..) => Some(Action::Resize),
        _ => None,
    }
}

/// Application state. Only the main loop mutates it.
#[derive(Debug)]
pub struct App {
    settings: Settings,
    client: ApiClient,
    msg_tx: mpsc::Sender<Msg>,
    core: Option<String>,
    should_quit: bool,
    dirty: bool,
}

impl App {
    pub fn new(settings: Settings, client: ApiClient, msg_tx: mpsc::Sender<Msg>) -> Self {
        Self {
            settings,
            client,
            msg_tx,
            core: None,
            should_quit: false,
            dirty: false,
        }
    }

    /// Spawns the startup requests.
    pub fn start(&self) {
        let client = self.client.clone();
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let version = client.get_json(&["version"]).await;
            let _ = tx.send(Msg::Version(version)).await;
        });
    }

    pub fn update(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::Resize => self.dirty = true,
        }
    }

    pub fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Version(result) => {
                self.core = Some(match result {
                    Ok(version) => version.to_string(),
                    Err(err) => format!("error: {err}"),
                });
                self.dirty = true;
            }
        }
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Returns whether a repaint is pending, and clears the flag.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
}

/// Placeholder screen until the `ui` module lands.
pub fn draw(frame: &mut Frame, app: &App) {
    let settings = &app.settings;
    let secret = if settings.secret.is_some() { "set" } else { "none" };
    let text = Text::from(format!(
        "tabby - press q to quit\n\nprofile  {}\nurl      {}\nsecret   {secret}\ntheme    {:?}\ncore     {}",
        settings.profile,
        settings.url,
        settings.theme,
        app.core.as_deref().unwrap_or("connecting..."),
    ));
    frame.render_widget(Paragraph::new(text), frame.area());
}
