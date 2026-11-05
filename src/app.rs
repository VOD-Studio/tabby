use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;
use tui_input::{Input, backend::crossterm::EventHandler};

use crate::{
    api::{
        backoff,
        client::ApiClient,
        error::ApiError,
        models::{Configs, CoreKind, GLOBAL, Proxies, Proxy, Version},
    },
    config::Settings,
    ui::{scroll::Scroll, text::describe, theme::Theme},
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

/// Cursor movement inside the focused list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Up,
    Down,
    Top,
    Bottom,
    HalfUp,
    HalfDown,
}

/// Rows covered by half a page (`Ctrl+u` / `Ctrl+d`).
const HALF_PAGE: usize = 10;

impl Move {
    /// The cursor position after this move in a list of `len` items.
    fn apply(self, cursor: usize, len: usize) -> usize {
        let last = len.saturating_sub(1);
        let cursor = cursor.min(last);
        match self {
            Self::Up => cursor.saturating_sub(1),
            Self::Down => (cursor + 1).min(last),
            Self::Top => 0,
            Self::Bottom => last,
            Self::HalfUp => cursor.saturating_sub(HALF_PAGE),
            Self::HalfDown => (cursor + HALF_PAGE).min(last),
        }
    }
}

/// The two panels of the proxies page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Pane {
    #[default]
    Groups,
    Nodes,
}

/// How the nodes of a group are ordered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    /// The order of the configuration.
    #[default]
    Default,
    /// Fastest first; nodes without a result go last.
    Delay,
    Name,
}

impl Sort {
    fn next(self) -> Self {
        match self {
            Self::Default => Self::Delay,
            Self::Delay => Self::Name,
            Self::Name => Self::Default,
        }
    }
}

/// Cursor, focus, ordering and filter of the proxies page.
#[derive(Debug, Default)]
pub struct ProxiesView {
    pub focus: Pane,
    /// Index into [`App::visible_groups`].
    pub group: usize,
    pub group_scroll: Scroll,
    /// Index into [`App::visible_nodes`]; `None` rests on the group's
    /// current node.
    pub node: Option<usize>,
    pub node_scroll: Scroll,
    pub sort: Sort,
    /// Case-insensitive substring the node names must contain.
    pub filter: Input,
    /// The filter box has the keyboard.
    pub typing: bool,
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
    Cursor(Move),
    Focus(Pane),
    /// `Enter`: acts on the item under the cursor.
    Confirm,
    /// `t`: delay-test the node under the cursor.
    TestNode,
    /// `T`: delay-test every member of the selected group.
    TestGroup,
    /// `s`: next node ordering.
    CycleSort,
    /// `/`: start typing a node filter.
    StartFilter,
    /// A key typed into the filter box.
    FilterInput(KeyEvent),
    /// `Enter` in the filter box: keep the filter, stop typing.
    FilterAccept,
    /// `Esc` in the filter box: drop the filter.
    FilterClear,
    ToggleHelp,
    /// `Esc`: closes the topmost overlay, or clears an applied filter.
    Dismiss,
}

/// A message from a background task.
#[derive(Debug)]
pub enum Msg {
    Version(Result<Version, ApiError>),
    Configs(Result<Configs, ApiError>),
    Proxies(Result<Proxies, ApiError>),
    /// Reply to switching a selector group.
    Selected {
        group: String,
        node: String,
        result: Result<(), ApiError>,
    },
    /// Reply to a single-node delay test.
    Delay {
        name: String,
        result: Result<u32, ApiError>,
    },
    /// Reply to a whole-group delay test.
    GroupDelay {
        members: Vec<String>,
        result: Result<HashMap<String, u32>, ApiError>,
    },
}

/// Maps a terminal event to an [`Action`]; `None` means the event is ignored.
pub fn action_for(event: &Event, page: Page, typing: bool) -> Option<Action> {
    if typing {
        return match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let ctrl_c =
                    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
                Some(match key.code {
                    _ if ctrl_c => Action::Quit,
                    KeyCode::Esc => Action::FilterClear,
                    KeyCode::Enter => Action::FilterAccept,
                    _ => Action::FilterInput(*key),
                })
            }
            Event::Resize(..) => Some(Action::Resize),
            _ => None,
        };
    }
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let alt = key.modifiers.contains(KeyModifiers::ALT);
            match key.code {
                KeyCode::Char('c') if ctrl => Some(Action::Quit),
                KeyCode::Char('d') if ctrl => Some(Action::Cursor(Move::HalfDown)),
                KeyCode::Char('u') if ctrl => Some(Action::Cursor(Move::HalfUp)),
                KeyCode::Tab => Some(Action::NextPage),
                KeyCode::BackTab => Some(Action::PrevPage),
                KeyCode::Esc => Some(Action::Dismiss),
                KeyCode::Enter => Some(Action::Confirm),
                KeyCode::Char(_) if ctrl || alt => None,
                KeyCode::Char('q') => Some(Action::Quit),
                KeyCode::Char('r') => Some(Action::Refresh),
                KeyCode::Char('?') => Some(Action::ToggleHelp),
                KeyCode::Down | KeyCode::Char('j') => Some(Action::Cursor(Move::Down)),
                KeyCode::Up | KeyCode::Char('k') => Some(Action::Cursor(Move::Up)),
                KeyCode::Char('g') => Some(Action::Cursor(Move::Top)),
                KeyCode::Char('G') => Some(Action::Cursor(Move::Bottom)),
                KeyCode::Char('h') if page == Page::Proxies => Some(Action::Focus(Pane::Groups)),
                KeyCode::Char('l') if page == Page::Proxies => Some(Action::Focus(Pane::Nodes)),
                KeyCode::Char('t') if page == Page::Proxies => Some(Action::TestNode),
                KeyCode::Char('T') if page == Page::Proxies => Some(Action::TestGroup),
                KeyCode::Char('s') if page == Page::Proxies => Some(Action::CycleSort),
                KeyCode::Char('/') if page == Page::Proxies => Some(Action::StartFilter),
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
    pub proxies: Option<Result<Proxies, ApiError>>,
    pub proxies_view: ProxiesView,
    /// Nodes with a delay test in flight.
    pub testing: HashSet<String>,
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
            proxies: None,
            proxies_view: ProxiesView::default(),
            testing: HashSet::new(),
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
        self.spawn(|client| async move { Msg::Version(client.version().await) });
        self.spawn(|client| async move { Msg::Configs(client.configs().await) });
        self.spawn(|client| async move { Msg::Proxies(client.proxies().await) });
    }

    /// Runs one request in the background; its message arrives on the channel.
    fn spawn<F, Fut>(&self, request: F)
    where
        F: FnOnce(ApiClient) -> Fut,
        Fut: Future<Output = Msg> + Send + 'static,
    {
        let tx = self.msg_tx.clone();
        let msg = request(self.client.clone());
        tokio::spawn(async move {
            let _ = tx.send(msg.await).await;
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
                if self.overlay.take().is_none() && self.page == Page::Proxies {
                    self.clear_filter();
                }
                self.dirty = true;
            }
            // Overlays are modal: everything else waits until they close.
            _ if self.overlay.is_some() => {}
            Action::SelectPage(page) => self.set_page(page),
            Action::NextPage => self.set_page(self.page.next()),
            Action::PrevPage => self.set_page(self.page.prev()),
            Action::Cursor(movement) => self.move_cursor(movement),
            Action::Confirm => self.confirm(),
            Action::TestNode => self.test_node(),
            Action::TestGroup => self.test_group(),
            Action::CycleSort => {
                self.proxies_view.sort = self.proxies_view.sort.next();
                self.proxies_view.node = None;
                self.dirty = true;
            }
            Action::StartFilter => {
                self.proxies_view.typing = true;
                self.dirty = true;
            }
            Action::FilterInput(key) => {
                self.proxies_view.filter.handle_event(&Event::Key(key));
                self.proxies_view.node = None;
                self.dirty = true;
            }
            Action::FilterAccept => {
                self.proxies_view.typing = false;
                self.dirty = true;
            }
            Action::FilterClear => self.clear_filter(),
            Action::Focus(pane) => {
                if self.page == Page::Proxies && self.visible_groups().next().is_some() {
                    self.proxies_view.focus = pane;
                    self.dirty = true;
                }
            }
            Action::Refresh => {
                // A failed request shows as loading again while it is retried.
                if matches!(self.version, Some(Err(_))) {
                    self.version = None;
                }
                if matches!(self.configs, Some(Err(_))) {
                    self.configs = None;
                }
                if matches!(self.proxies, Some(Err(_))) {
                    self.proxies = None;
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

    /// Groups the proxies page lists: `GLOBAL` only counts in global mode.
    pub fn visible_groups(&self) -> impl Iterator<Item = &Proxy> {
        let global_mode = matches!(&self.configs, Some(Ok(configs)) if configs.mode == "global");
        self.proxies
            .iter()
            .filter_map(|proxies| proxies.as_ref().ok())
            .flat_map(Proxies::groups)
            .filter(move |group| global_mode || group.name != GLOBAL)
    }

    /// The group the cursor is on.
    pub fn selected_group(&self) -> Option<&Proxy> {
        let len = self.visible_groups().count();
        self.visible_groups()
            .nth(self.proxies_view.group.min(len.saturating_sub(1)))
    }

    fn move_cursor(&mut self, movement: Move) {
        if self.page != Page::Proxies {
            return;
        }
        match self.proxies_view.focus {
            Pane::Groups => {
                let len = self.visible_groups().count();
                let view = &mut self.proxies_view;
                let group = movement.apply(view.group, len);
                if group != view.group {
                    view.group = group;
                    view.node = None;
                }
            }
            Pane::Nodes => {
                let len = self.visible_nodes().len();
                let current = self.node_cursor();
                self.proxies_view.node = Some(movement.apply(current, len));
            }
        }
        self.dirty = true;
    }

    /// Drops the node filter and gives the keyboard back.
    fn clear_filter(&mut self) {
        self.proxies_view.filter.reset();
        self.proxies_view.typing = false;
        self.proxies_view.node = None;
    }

    /// The selected group's members that pass the filter, in display order.
    pub fn visible_nodes(&self) -> Vec<&str> {
        let Some(group) = self.selected_group() else {
            return vec![];
        };
        let view = &self.proxies_view;
        let filter = view.filter.value().to_lowercase();
        let mut nodes: Vec<&str> = group
            .all
            .iter()
            .flatten()
            .map(String::as_str)
            .filter(|name| filter.is_empty() || name.to_lowercase().contains(&filter))
            .collect();
        let proxies = self
            .proxies
            .as_ref()
            .and_then(|proxies| proxies.as_ref().ok());
        match view.sort {
            Sort::Default => {}
            Sort::Name => nodes.sort_by_cached_key(|name| name.to_lowercase()),
            Sort::Delay => nodes.sort_by_key(|name| {
                match proxies
                    .and_then(|proxies| proxies.get(name))
                    .and_then(Proxy::delay)
                {
                    Some(millis) if millis > 0 => (0, millis),
                    _ => (1, 0),
                }
            }),
        }
        nodes
    }

    /// Where the node cursor is in [`App::visible_nodes`]: wherever it was
    /// moved to, else on the group's current node.
    pub fn node_cursor(&self) -> usize {
        let nodes = self.visible_nodes();
        let resting = self
            .selected_group()
            .and_then(|group| group.now.as_deref())
            .and_then(|now| nodes.iter().position(|name| *name == now))
            .unwrap_or(0);
        self.proxies_view
            .node
            .unwrap_or(resting)
            .min(nodes.len().saturating_sub(1))
    }

    /// `Enter` on the proxies page: from the group list, moves into the nodes;
    /// on a node, switches the group to it.
    fn confirm(&mut self) {
        if self.page != Page::Proxies || self.selected_group().is_none() {
            return;
        }
        match self.proxies_view.focus {
            Pane::Groups => {
                self.proxies_view.focus = Pane::Nodes;
                self.dirty = true;
            }
            Pane::Nodes => self.select_node(),
        }
    }

    fn select_node(&mut self) {
        let Some(group) = self.selected_group() else {
            return;
        };
        if !group.is_selector() {
            self.toast(ToastKind::Error, "该组为自动选择".to_owned());
            return;
        }
        let nodes = self.visible_nodes();
        let Some(node) = nodes.get(self.node_cursor()) else {
            return;
        };
        let (group, node) = (group.name.clone(), (*node).to_owned());
        self.spawn(move |client| async move {
            let result = client.select_proxy(&group, &node).await;
            Msg::Selected {
                group,
                node,
                result,
            }
        });
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
                self.note_failure(&result);
                self.configs = Some(result);
            }
            Msg::Proxies(result) => {
                self.note_failure(&result);
                self.proxies = Some(result);
            }
            Msg::Selected {
                group,
                node,
                result,
            } => match result {
                Ok(()) => {
                    if let Some(Ok(proxies)) = &mut self.proxies {
                        proxies.set_now(&group, &node);
                    }
                    self.toast(ToastKind::Success, format!("已切换到 {node}"));
                }
                Err(err) => self.toast(ToastKind::Error, format!("切换失败：{}", describe(&err))),
            },
            Msg::Delay { name, result } => {
                self.testing.remove(&name);
                match result {
                    Ok(millis) => self.record_delay(&name, millis),
                    Err(err) if is_failed_test(&err) => self.record_delay(&name, 0),
                    Err(err) => {
                        self.toast(ToastKind::Error, format!("测速失败：{}", describe(&err)));
                    }
                }
            }
            Msg::GroupDelay { members, result } => {
                for member in &members {
                    self.testing.remove(member);
                }
                match result {
                    // Members that failed are simply absent from the reply.
                    Ok(delays) => {
                        for member in &members {
                            self.record_delay(member, delays.get(member).copied().unwrap_or(0));
                        }
                    }
                    Err(err) => {
                        self.toast(ToastKind::Error, format!("测速失败：{}", describe(&err)));
                    }
                }
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
        if self.is_loading() || (self.page == Page::Proxies && !self.testing.is_empty()) {
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
    pub fn is_loading(&self) -> bool {
        match self.page {
            Page::Overview => self.configs.is_none(),
            Page::Proxies => self.proxies.is_none(),
            _ => false,
        }
    }

    /// Whether the kernel is mihomo (group delay tests and the like need it).
    pub fn is_mihomo(&self) -> bool {
        matches!(&self.version, Some(Ok(version)) if version.kind() == CoreKind::Mihomo)
    }

    fn record_delay(&mut self, name: &str, millis: u32) {
        if let Some(Ok(proxies)) = &mut self.proxies {
            proxies.record_delay(name, millis);
        }
    }

    /// `t`: delay-test the node under the cursor, or the group's current node
    /// while the group list has the focus.
    fn test_node(&mut self) {
        let Some(group) = self.selected_group() else {
            return;
        };
        let target = match self.proxies_view.focus {
            Pane::Groups => group.now.clone(),
            Pane::Nodes => self
                .visible_nodes()
                .get(self.node_cursor())
                .map(|name| (*name).to_owned()),
        };
        let Some(name) = target else {
            return;
        };
        if !self.testing.insert(name.clone()) {
            return;
        }
        let url = self.settings.delay_url.clone();
        let timeout = self.settings.delay_timeout_ms;
        self.dirty = true;
        self.spawn(move |client| async move {
            let result = client.proxy_delay(&name, &url, timeout).await;
            Msg::Delay { name, result }
        });
    }

    /// `T`: delay-test all members of the selected group at once.
    fn test_group(&mut self) {
        if !self.is_mihomo() {
            self.toast(ToastKind::Error, "整组测速需要 mihomo".to_owned());
            return;
        }
        let Some(group) = self.selected_group() else {
            return;
        };
        let name = group.name.clone();
        let members: Vec<String> = group
            .all
            .iter()
            .flatten()
            .filter(|member| !self.testing.contains(*member))
            .cloned()
            .collect();
        if members.is_empty() {
            return;
        }
        self.testing.extend(members.iter().cloned());
        let url = self.settings.delay_url.clone();
        let timeout = self.settings.delay_timeout_ms;
        self.dirty = true;
        self.spawn(move |client| async move {
            let result = client.group_delay(&name, &url, timeout).await;
            Msg::GroupDelay { members, result }
        });
    }

    /// Toasts a failed data request, unless the connection is already known
    /// to be down (the banner covers that).
    fn note_failure<T>(&mut self, result: &Result<T, ApiError>) {
        if let Err(err) = result
            && !matches!(self.version, Some(Err(_)))
        {
            self.toast(ToastKind::Error, describe(err));
        }
    }

    /// Shows a toast. An identical one already on screen is renewed instead of
    /// stacked, so one failure reported by two requests shows once.
    fn toast(&mut self, kind: ToastKind, text: String) {
        self.dirty = true;
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

/// The kernel answers a failed or timed-out delay test with 503 / 504, which
/// is a result (the node is unreachable), not a failure of the request.
fn is_failed_test(err: &ApiError) -> bool {
    matches!(
        err,
        ApiError::Timeout
            | ApiError::Http {
                status: 503 | 504,
                ..
            }
    )
}
