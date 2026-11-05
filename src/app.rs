use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;

use crate::{
    api::{
        client::ApiClient,
        error::ApiError,
        models::{Configs, Version},
    },
    config::Settings,
    ui::theme::Theme,
};

/// The seven top-level pages, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Overview,
    Proxies,
    Providers,
    Rules,
    Connections,
    Logs,
    Settings,
}

impl Page {
    pub const ALL: [Self; 7] = [
        Self::Overview,
        Self::Proxies,
        Self::Providers,
        Self::Rules,
        Self::Connections,
        Self::Logs,
        Self::Settings,
    ];

    /// The page bound to number key `1`–`7`.
    fn from_digit(key: char) -> Option<Self> {
        let number = usize::try_from(key.to_digit(10)?).ok()?;
        Self::ALL.get(number.checked_sub(1)?).copied()
    }

    fn next(self) -> Self {
        Self::ALL[(self as usize + 1) % Self::ALL.len()]
    }

    fn prev(self) -> Self {
        Self::ALL[(self as usize + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// A user intent, derived from a terminal event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Resize,
    SelectPage(Page),
    NextPage,
    PrevPage,
    Refresh,
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
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let alt = key.modifiers.contains(KeyModifiers::ALT);
            match key.code {
                KeyCode::Char('c') if ctrl => Some(Action::Quit),
                KeyCode::Tab => Some(Action::NextPage),
                KeyCode::BackTab => Some(Action::PrevPage),
                KeyCode::Char(_) if ctrl || alt => None,
                KeyCode::Char('q') => Some(Action::Quit),
                KeyCode::Char('r') => Some(Action::Refresh),
                KeyCode::Char(c) => Page::from_digit(c).map(Action::SelectPage),
                _ => None,
            }
        }
        Event::Resize(..) => Some(Action::Resize),
        _ => None,
    }
}

/// How long a toast stays on screen.
const TOAST_TTL: Duration = Duration::from_secs(3);

/// Older toasts are dropped beyond this many.
const MAX_TOASTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Error,
}

/// A short-lived notification, shown top-right.
#[derive(Debug)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    expires_at: Instant,
}

/// Whether the controller has answered the `/version` probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    Connected,
    Disconnected,
}

/// Application state. Only the main loop mutates it.
#[derive(Debug)]
pub struct App {
    pub settings: Settings,
    pub theme: Theme,
    pub page: Page,
    client: ApiClient,
    msg_tx: mpsc::Sender<Msg>,
    pub version: Option<Result<Version, ApiError>>,
    pub configs: Option<Result<Configs, ApiError>>,
    pub toasts: VecDeque<Toast>,
    /// Set by `r` until the next `/version` reply, which then reports success.
    refreshing: bool,
    should_quit: bool,
    dirty: bool,
}

impl App {
    pub fn new(settings: Settings, client: ApiClient, msg_tx: mpsc::Sender<Msg>) -> Self {
        Self {
            theme: settings.theme.into(),
            settings,
            page: Page::Overview,
            client,
            msg_tx,
            version: None,
            configs: None,
            toasts: VecDeque::new(),
            refreshing: false,
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
            Action::SelectPage(page) => self.set_page(page),
            Action::NextPage => self.set_page(self.page.next()),
            Action::PrevPage => self.set_page(self.page.prev()),
            Action::Refresh => {
                self.refreshing = true;
                self.start();
            }
        }
    }

    pub fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Version(result) => {
                let refreshed = std::mem::take(&mut self.refreshing);
                match &result {
                    Ok(_) if refreshed => self.toast(ToastKind::Success, "已刷新".to_owned()),
                    Ok(_) => {}
                    Err(err) => self.toast(ToastKind::Error, describe(err)),
                }
                self.version = Some(result);
            }
            Msg::Configs(result) => {
                if let Err(err) = &result {
                    self.toast(ToastKind::Error, describe(err));
                }
                self.configs = Some(result);
            }
        }
        self.dirty = true;
    }

    /// Drops expired toasts.
    pub fn on_tick(&mut self, now: Instant) {
        let shown = self.toasts.len();
        self.toasts.retain(|toast| toast.expires_at > now);
        if self.toasts.len() != shown {
            self.dirty = true;
        }
    }

    /// Shows a toast. An identical one already on screen is renewed instead of
    /// stacked, so one failure reported by two requests shows once.
    fn toast(&mut self, kind: ToastKind, text: String) {
        let expires_at = Instant::now() + TOAST_TTL;
        if let Some(existing) = self
            .toasts
            .iter_mut()
            .find(|toast| toast.kind == kind && toast.text == text)
        {
            existing.expires_at = expires_at;
            return;
        }
        if self.toasts.len() == MAX_TOASTS {
            self.toasts.pop_front();
        }
        self.toasts.push_back(Toast {
            kind,
            text,
            expires_at,
        });
    }

    pub fn connection(&self) -> Connection {
        match &self.version {
            None => Connection::Connecting,
            Some(Ok(_)) => Connection::Connected,
            Some(Err(_)) => Connection::Disconnected,
        }
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Returns whether a repaint is pending, and clears the flag.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    fn set_page(&mut self, page: Page) {
        self.page = page;
        self.dirty = true;
    }
}

/// One-line description of a failed request for a toast.
fn describe(err: &ApiError) -> String {
    match err {
        ApiError::Unauthorized => "Secret 错误，请用 --secret 或修改配置".to_owned(),
        ApiError::NotFound => "接口不存在（404）".to_owned(),
        ApiError::Http { status, message } => format!("HTTP {status}：{message}"),
        ApiError::Timeout => "请求超时".to_owned(),
        ApiError::Connect(_) => "无法连接控制器".to_owned(),
        ApiError::Decode(_) => "控制器返回了无法解析的数据".to_owned(),
    }
}
