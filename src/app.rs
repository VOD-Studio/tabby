use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{Frame, text::Text, widgets::Paragraph};
use tokio::sync::mpsc;

use crate::{
    api::{
        client::ApiClient,
        error::ApiError,
        models::{Configs, Version},
    },
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
    Version(Result<Version, ApiError>),
    Configs(Result<Configs, ApiError>),
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
    version: Option<Result<Version, ApiError>>,
    configs: Option<Result<Configs, ApiError>>,
    should_quit: bool,
    dirty: bool,
}

impl App {
    pub fn new(settings: Settings, client: ApiClient, msg_tx: mpsc::Sender<Msg>) -> Self {
        Self {
            settings,
            client,
            msg_tx,
            version: None,
            configs: None,
            should_quit: false,
            dirty: false,
        }
    }

    /// Spawns the startup requests.
    pub fn start(&self) {
        let client = self.client.clone();
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(Msg::Version(client.version().await)).await;
        });
        let client = self.client.clone();
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(Msg::Configs(client.configs().await)).await;
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
            Msg::Version(result) => self.version = Some(result),
            Msg::Configs(result) => self.configs = Some(result),
        }
        self.dirty = true;
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
    let core = match &app.version {
        None => "connecting...".to_owned(),
        Some(Ok(version)) => format!("{:?} {}", version.kind(), version.version),
        Some(Err(err)) => format!("error: {err}"),
    };
    let configs = match &app.configs {
        None => "connecting...".to_owned(),
        Some(Ok(c)) => format!(
            "mode {} | log {} | ports http {} socks {} mixed {} | lan {} | ipv6 {} | tun {}",
            c.mode,
            c.log_level,
            c.port,
            c.socks_port,
            c.mixed_port,
            c.allow_lan,
            c.ipv6,
            c.tun
                .as_ref()
                .map_or("needs mihomo", |tun| if tun.enable { "on" } else { "off" }),
        ),
        Some(Err(err)) => format!("error: {err}"),
    };
    let text = Text::from(format!(
        "tabby - press q to quit\n\nprofile  {}\nurl      {}\nsecret   {secret}\ntheme    {:?}\ncore     {core}\nconfigs  {configs}",
        settings.profile, settings.url, settings.theme,
    ));
    frame.render_widget(Paragraph::new(text), frame.area());
}
