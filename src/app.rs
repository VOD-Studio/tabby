use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;

use crate::{
    api::{
        backoff,
        client::ApiClient,
        error::ApiError,
        models::{Configs, Version},
    },
    config::Settings,
    ui::{text::describe, theme::Theme},
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
    ToggleHelp,
    /// `Esc`: closes the topmost overlay.
    Dismiss,
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
                KeyCode::Esc => Some(Action::Dismiss),
                KeyCode::Char(_) if ctrl || alt => None,
                KeyCode::Char('q') => Some(Action::Quit),
                KeyCode::Char('r') => Some(Action::Refresh),
                KeyCode::Char('?') => Some(Action::ToggleHelp),
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

/// A modal panel drawn over the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    Help,
}

/// Bookkeeping while the controller is unreachable.
#[derive(Debug, Clone, Copy)]
pub struct Reconnect {
    /// The retry being waited for, or in flight (1 after the first failure).
    pub attempt: u32,
    /// When the next retry fires; `None` while one is in flight or not coming.
    pub retry_at: Option<Instant>,
    /// The secret was rejected: retrying cannot help.
    pub unauthorized: bool,
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
    pub overlay: Option<Overlay>,
    /// Frame counter of the loading spinner; advances while one is visible.
    pub spinner: usize,
    /// `Some` from the first failed `/version` probe until one succeeds.
    pub reconnect: Option<Reconnect>,
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
            overlay: None,
            spinner: 0,
            reconnect: None,
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
            Action::ToggleHelp => {
                self.overlay = match self.overlay {
                    Some(Overlay::Help) => None,
                    None => Some(Overlay::Help),
                };
                self.dirty = true;
            }
            Action::Dismiss => {
                self.overlay = None;
                self.dirty = true;
            }
            // Overlays are modal: everything else waits until they close.
            _ if self.overlay.is_some() => {}
            Action::SelectPage(page) => self.set_page(page),
            Action::NextPage => self.set_page(self.page.next()),
            Action::PrevPage => self.set_page(self.page.prev()),
            Action::Refresh => {
                // A failed request shows as loading again while it is retried.
                if matches!(self.version, Some(Err(_))) {
                    self.version = None;
                }
                if matches!(self.configs, Some(Err(_))) {
                    self.configs = None;
                }
                // Retry now instead of waiting out the backoff.
                if let Some(reconnect) = &mut self.reconnect {
                    reconnect.retry_at = None;
                }
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
                    Ok(_) => {
                        if self.reconnect.take().is_some() {
                            self.toast(ToastKind::Success, "已重新连接".to_owned());
                        } else if refreshed {
                            self.toast(ToastKind::Success, "已刷新".to_owned());
                        }
                    }
                    Err(err) => {
                        // Automatic retries stay quiet; the banner covers them.
                        let first_failure = self.reconnect.is_none();
                        self.schedule_reconnect(err);
                        if first_failure || refreshed {
                            self.toast(ToastKind::Error, describe(err));
                        }
                    }
                }
                self.version = Some(result);
            }
            Msg::Configs(result) => {
                if let Err(err) = &result
                    && !matches!(self.version, Some(Err(_)))
                {
                    self.toast(ToastKind::Error, describe(err));
                }
                self.configs = Some(result);
            }
        }
        self.dirty = true;
    }

    /// Drops expired toasts, fires due reconnects and steps the animations.
    pub fn on_tick(&mut self, now: Instant) {
        let shown = self.toasts.len();
        self.toasts.retain(|toast| toast.expires_at > now);
        if self.toasts.len() != shown {
            self.dirty = true;
        }
        if self.is_loading() {
            self.spinner = self.spinner.wrapping_add(1);
            self.dirty = true;
        }
        let mut retry_due = false;
        if let Some(reconnect) = &mut self.reconnect
            && let Some(at) = reconnect.retry_at
        {
            if now >= at {
                reconnect.retry_at = None;
                retry_due = true;
            }
            // Either way the banner's countdown changed.
            self.dirty = true;
        }
        if retry_due {
            self.start();
        }
    }

    /// Plans the next retry after a failed `/version` probe: 1 s, 2 s, 4 s …
    /// up to 30 s. A rejected secret is not retried.
    fn schedule_reconnect(&mut self, err: &ApiError) {
        let attempt = self
            .reconnect
            .map_or(1, |reconnect| reconnect.attempt.saturating_add(1));
        let unauthorized = matches!(err, ApiError::Unauthorized);
        self.reconnect = Some(Reconnect {
            attempt,
            retry_at: (!unauthorized).then(|| Instant::now() + backoff(attempt - 1)),
            unauthorized,
        });
    }

    /// Whether the current page is waiting for data (and so shows a spinner).
    /// The overview is the only page with data to wait for so far.
    pub fn is_loading(&self) -> bool {
        self.page == Page::Overview && self.configs.is_none()
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
