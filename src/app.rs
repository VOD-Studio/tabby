use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Rect, Size};
use tokio::sync::mpsc;
use tui_input::{Input, backend::crossterm::EventHandler};

use crate::{
    api::{
        backoff,
        client::ApiClient,
        error::ApiError,
        models::{Configs, CoreKind, Delay, GLOBAL, Proxies, Proxy, Version},
    },
    config::Settings,
    ui::{proxies, regions, scroll::Scroll, text::describe, theme::Theme},
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
    /// One card left in the node grid; from the leftmost column, back to the
    /// group list. In the group list, nothing.
    Left,
    /// One card right in the node grid; in the group list, into the grid.
    Right,
    Top,
    Bottom,
    HalfUp,
    HalfDown,
}

impl Move {
    /// The cursor position after this move in a list of `len` items; a half
    /// page is `half` items.
    fn apply(self, cursor: usize, len: usize, half: usize) -> usize {
        let last = len.saturating_sub(1);
        let cursor = cursor.min(last);
        match self {
            Self::Up => cursor.saturating_sub(1),
            Self::Down => (cursor + 1).min(last),
            Self::Left | Self::Right => cursor,
            Self::Top => 0,
            Self::Bottom => last,
            Self::HalfUp => cursor.saturating_sub(half),
            Self::HalfDown => (cursor + half).min(last),
        }
    }

    /// The cursor position after this move in a grid of `len` cards laid out
    /// `columns` per row, filled row by row; a half page is `half_rows` rows.
    /// Left and right stay inside their row. Vertical moves keep the column;
    /// moving down past a short last row lands on the same column of that
    /// row, or on its last card when that column is empty.
    fn apply_grid(self, cursor: usize, len: usize, columns: usize, half_rows: usize) -> usize {
        let last = len.saturating_sub(1);
        let cursor = cursor.min(last);
        let columns = columns.max(1);
        let up = |rows: usize| {
            cursor
                .checked_sub(rows * columns)
                .unwrap_or(cursor % columns)
        };
        let down = |rows: usize| {
            let target = cursor + rows * columns;
            if target <= last {
                target
            } else {
                (last / columns * columns + cursor % columns).min(last)
            }
        };
        match self {
            Self::Up => up(1),
            Self::Down => down(1),
            Self::HalfUp => up(half_rows),
            Self::HalfDown => down(half_rows),
            Self::Left if !cursor.is_multiple_of(columns) => cursor - 1,
            Self::Right if !(cursor + 1).is_multiple_of(columns) && cursor < last => cursor + 1,
            Self::Left | Self::Right => cursor,
            Self::Top => 0,
            Self::Bottom => last,
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
    /// The terminal changed size.
    Resize(Size),
    SelectPage(Page),
    NextPage,
    PrevPage,
    Refresh,
    Cursor(Move),
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
            Event::Resize(width, height) => Some(Action::Resize(Size::new(*width, *height))),
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
                KeyCode::Left => Some(Action::Cursor(Move::Left)),
                KeyCode::Right => Some(Action::Cursor(Move::Right)),
                KeyCode::Char('g') => Some(Action::Cursor(Move::Top)),
                KeyCode::Char('G') => Some(Action::Cursor(Move::Bottom)),
                KeyCode::Char('h') if page == Page::Proxies => Some(Action::Cursor(Move::Left)),
                KeyCode::Char('l') if page == Page::Proxies => Some(Action::Cursor(Move::Right)),
                KeyCode::Char('t') if page == Page::Proxies => Some(Action::TestNode),
                KeyCode::Char('T') if page == Page::Proxies => Some(Action::TestGroup),
                KeyCode::Char('s') if page == Page::Proxies => Some(Action::CycleSort),
                KeyCode::Char('/') if page == Page::Proxies => Some(Action::StartFilter),
                KeyCode::Char(c) => Page::from_digit(c).map(Action::SelectPage),
                _ => None,
            }
        }
        Event::Resize(width, height) => Some(Action::Resize(Size::new(*width, *height))),
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
    /// The terminal size, from the start and every resize event.
    size: Size,
    dirty: bool,
}

impl App {
    pub fn new(
        settings: Settings,
        client: ApiClient,
        msg_tx: mpsc::Sender<Msg>,
        size: Size,
    ) -> Self {
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
            size,
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
            Action::Resize(size) => {
                self.size = size;
                self.dirty = true;
            }
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

    /// Keeps the group cursor sensible when the mode change added or removed
    /// `GLOBAL` at the head of the list: entering global mode lands on it,
    /// leaving stays on the group that was selected.
    fn follow_mode_change(&mut self, was_global: bool, selected: Option<String>) {
        let group = match (was_global, self.is_global_mode()) {
            (false, true) => 0,
            (true, false) => selected
                .and_then(|name| self.visible_groups().position(|group| group.name == name))
                .unwrap_or(0),
            _ => return,
        };
        self.proxies_view.group = group;
        // The node cursor indexed the previous group's members.
        self.proxies_view.node = None;
    }

    pub fn is_global_mode(&self) -> bool {
        matches!(&self.configs, Some(Ok(configs)) if configs.mode == "global")
    }

    /// Groups the proxies page lists. `GLOBAL` only counts in global mode,
    /// where it leads; the kernel puts it last.
    pub fn visible_groups(&self) -> impl Iterator<Item = &Proxy> {
        let proxies = self
            .proxies
            .as_ref()
            .and_then(|proxies| proxies.as_ref().ok());
        let global = proxies
            .filter(|_| self.is_global_mode())
            .and_then(|proxies| proxies.get(GLOBAL))
            .filter(|global| global.all.is_some());
        global.into_iter().chain(
            proxies
                .into_iter()
                .flat_map(Proxies::groups)
                .filter(|group| group.name != GLOBAL),
        )
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
            Pane::Groups if movement == Move::Right => {
                if self.selected_group().is_some() {
                    self.proxies_view.focus = Pane::Nodes;
                }
            }
            Pane::Groups => {
                let len = self.visible_groups().count();
                let half = (self.proxies_fit().groups / 2).max(1);
                let view = &mut self.proxies_view;
                let group = movement.apply(view.group, len, half);
                if group != view.group {
                    view.group = group;
                    view.node = None;
                }
            }
            Pane::Nodes => {
                let fit = self.proxies_fit();
                let current = self.node_cursor();
                if movement == Move::Left && current.is_multiple_of(fit.columns) {
                    self.proxies_view.focus = Pane::Groups;
                } else {
                    let len = self.visible_nodes().len();
                    let half_rows = (fit.rows / 2).max(1);
                    self.proxies_view.node =
                        Some(movement.apply_grid(current, len, fit.columns, half_rows));
                }
            }
        }
        self.dirty = true;
    }

    /// How much of the proxies page fits in the terminal, laid out by the
    /// same functions that draw it.
    fn proxies_fit(&self) -> proxies::Fit {
        let screen = Rect::from(self.size);
        proxies::fit(regions(screen, self.reconnect.is_some()).body)
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
                    Some(Delay::Millis(millis)) => (0, millis),
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
                let was_global = self.is_global_mode();
                let selected = self.selected_group().map(|group| group.name.clone());
                self.configs = Some(result);
                self.follow_mode_change(was_global, selected);
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
                let outcome = result
                    .map(Delay::from_millis)
                    .or_else(|err| failed_test(&err).ok_or(err));
                match outcome {
                    Ok(delay) => self.record_delay(&name, delay),
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
                    // Members that failed or timed out are absent from the reply.
                    Ok(delays) => {
                        for member in &members {
                            let delay = delays
                                .get(member)
                                .map_or(Delay::Failed, |millis| Delay::from_millis(*millis));
                            self.record_delay(member, delay);
                        }
                    }
                    // No member answered: the kernel replies 504 instead of `{}`.
                    Err(err) if failed_test(&err).is_some() => {
                        for member in &members {
                            self.record_delay(member, Delay::Failed);
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

    fn record_delay(&mut self, name: &str, delay: Delay) {
        if let Some(Ok(proxies)) = &mut self.proxies {
            proxies.record_delay(name, delay);
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

/// Reads a delay test that errored as a result, or `None` when the request
/// itself failed.
///
/// Checked against mihomo v1.19.33 with one node per case:
///
/// - `GET /proxies/{name}/delay`: 200 `{"delay": ms}`; **503**
///   `{"message":"An error occurred in the delay test"}` when the node is
///   unreachable (connection refused, DNS failure); **504**
///   `{"message":"Timeout"}` when it did not answer within `timeout`; 404
///   for an unknown node.
/// - `GET /group/{name}/delay`: 200 with only the members that answered. A
///   member that failed and one that timed out are both just missing, so the
///   two cannot be told apart. When no member answers the reply is **504**
///   `{"message":"get delay: all proxies timeout"}`, even if every one
///   failed at once.
///
/// A client-side timeout means the kernel never replied within the request
/// limit, which is longer than any sane delay timeout: the node did not answer.
fn failed_test(err: &ApiError) -> Option<Delay> {
    match err {
        ApiError::Http { status: 503, .. } => Some(Delay::Failed),
        ApiError::Http { status: 504, .. } | ApiError::Timeout => Some(Delay::Timeout),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 cards, 3 per row: the last row holds only card 9.
    const LEN: usize = 10;
    const COLUMNS: usize = 3;

    /// Half a page is two rows.
    fn grid(movement: Move, cursor: usize) -> usize {
        movement.apply_grid(cursor, LEN, COLUMNS, 2)
    }

    #[test]
    fn down_moves_one_row_in_the_same_column() {
        assert_eq!(grid(Move::Down, 1), 4);
        assert_eq!(grid(Move::Up, 4), 1);
    }

    #[test]
    fn down_into_a_short_last_row_lands_on_the_same_column_or_the_last_card() {
        assert_eq!(grid(Move::Down, 6), 9);
        // Column 1 and 2 have no card in the last row.
        assert_eq!(grid(Move::Down, 7), 9);
        assert_eq!(grid(Move::Down, 8), 9);
    }

    #[test]
    fn vertical_moves_stop_at_the_edges() {
        assert_eq!(grid(Move::Up, 2), 2);
        assert_eq!(grid(Move::Down, 9), 9);
    }

    #[test]
    fn horizontal_moves_stay_within_the_row() {
        assert_eq!(grid(Move::Right, 0), 1);
        assert_eq!(grid(Move::Right, 2), 2);
        assert_eq!(grid(Move::Left, 4), 3);
        assert_eq!(grid(Move::Left, 3), 3);
        // The last row is short: nothing to the right of the last card.
        assert_eq!(grid(Move::Right, 9), 9);
    }

    #[test]
    fn right_stops_at_the_last_card_of_a_partial_row() {
        assert_eq!(Move::Right.apply_grid(7, 8, COLUMNS, 1), 7);
    }

    #[test]
    fn half_page_moves_whole_rows_and_keeps_the_column() {
        assert_eq!(grid(Move::HalfDown, 1), 7);
        assert_eq!(grid(Move::HalfUp, 7), 1);
    }

    #[test]
    fn half_page_clamps_to_the_grid() {
        // Two rows down from row 2 overshoots: same rule as `Down`.
        assert_eq!(grid(Move::HalfDown, 5), 9);
        assert_eq!(grid(Move::HalfDown, 9), 9);
        assert_eq!(grid(Move::HalfUp, 4), 1);
        assert_eq!(grid(Move::HalfUp, 2), 2);
    }

    #[test]
    fn list_half_page_is_the_given_height() {
        assert_eq!(Move::HalfDown.apply(2, 30, 4), 6);
        assert_eq!(Move::HalfDown.apply(28, 30, 4), 29);
        assert_eq!(Move::HalfUp.apply(2, 30, 4), 0);
    }

    fn app_with_proxies() -> (App, Vec<String>) {
        let (tx, _rx) = mpsc::channel(1);
        let settings = crate::config::Config::default()
            .resolve(&crate::config::Overrides::default())
            .unwrap();
        let client = ApiClient::new(&settings.url, None).unwrap();
        let mut app = App::new(settings, client, tx, Size::new(120, 40));
        let proxies: Proxies =
            serde_json::from_str(include_str!("../tests/fixtures/mihomo_proxies.json")).unwrap();
        let members = proxies.groups().next().unwrap().all.clone().unwrap();
        app.proxies = Some(Ok(proxies));
        (app, members)
    }

    fn delay_of(app: &App, name: &str) -> Option<Delay> {
        app.proxies.as_ref()?.as_ref().ok()?.get(name)?.delay()
    }

    fn http(status: u16) -> ApiError {
        ApiError::Http {
            status,
            message: String::new(),
        }
    }

    #[test]
    fn node_test_tells_failed_from_timed_out() {
        let (mut app, members) = app_with_proxies();
        let test = |app: &mut App, name: &str, result| {
            app.handle_msg(Msg::Delay {
                name: name.to_owned(),
                result,
            });
        };
        test(&mut app, &members[0], Err(http(503)));
        test(&mut app, &members[1], Err(http(504)));
        test(&mut app, &members[2], Ok(120));
        assert_eq!(delay_of(&app, &members[0]), Some(Delay::Failed));
        assert_eq!(delay_of(&app, &members[1]), Some(Delay::Timeout));
        assert_eq!(delay_of(&app, &members[2]), Some(Delay::Millis(120)));
    }

    #[test]
    fn failed_request_is_not_a_test_result() {
        let (mut app, members) = app_with_proxies();
        let before = delay_of(&app, &members[0]);
        app.handle_msg(Msg::Delay {
            name: members[0].clone(),
            result: Err(http(500)),
        });
        assert_eq!(delay_of(&app, &members[0]), before);
        assert_eq!(app.toasts.len(), 1);
    }

    #[test]
    fn group_test_counts_missing_members_as_failed() {
        let (mut app, members) = app_with_proxies();
        let members = members[..3].to_vec();
        let answered = HashMap::from([(members[0].clone(), 80)]);
        app.handle_msg(Msg::GroupDelay {
            members: members.clone(),
            result: Ok(answered),
        });
        assert_eq!(delay_of(&app, &members[0]), Some(Delay::Millis(80)));
        assert_eq!(delay_of(&app, &members[1]), Some(Delay::Failed));
        assert_eq!(delay_of(&app, &members[2]), Some(Delay::Failed));
    }

    #[test]
    fn group_test_without_any_answer_fails_every_member() {
        let (mut app, members) = app_with_proxies();
        let members = members[..3].to_vec();
        app.handle_msg(Msg::GroupDelay {
            members: members.clone(),
            result: Err(http(504)),
        });
        assert!(
            members
                .iter()
                .all(|m| delay_of(&app, m) == Some(Delay::Failed))
        );
        assert!(app.toasts.is_empty());
    }

    fn set_mode(app: &mut App, mode: &str) {
        app.handle_msg(Msg::Configs(Ok(Configs {
            mode: mode.to_owned(),
            ..Configs::default()
        })));
    }

    fn group_names(app: &App) -> Vec<&str> {
        app.visible_groups()
            .map(|group| group.name.as_str())
            .collect()
    }

    #[test]
    fn global_group_only_shows_in_global_mode_and_leads() {
        let (mut app, _) = app_with_proxies();
        let configured = ["节点选择", "自动选择", "故障转移", "负载均衡"];
        for mode in ["rule", "direct"] {
            set_mode(&mut app, mode);
            assert_eq!(group_names(&app), configured);
        }
        set_mode(&mut app, "global");
        assert_eq!(
            group_names(&app),
            ["GLOBAL", "节点选择", "自动选择", "故障转移", "负载均衡"]
        );
    }

    #[test]
    fn entering_global_mode_moves_the_cursor_to_global() {
        let (mut app, _) = app_with_proxies();
        set_mode(&mut app, "rule");
        app.proxies_view.group = 2;
        app.proxies_view.node = Some(3);
        set_mode(&mut app, "global");
        assert_eq!(app.selected_group().unwrap().name, GLOBAL);
        assert_eq!(app.proxies_view.node, None);
    }

    #[test]
    fn leaving_global_mode_keeps_the_selected_group() {
        let (mut app, _) = app_with_proxies();
        set_mode(&mut app, "global");
        app.proxies_view.group = 3;
        assert_eq!(app.selected_group().unwrap().name, "故障转移");
        set_mode(&mut app, "rule");
        assert_eq!(app.selected_group().unwrap().name, "故障转移");

        // Standing on GLOBAL itself, the cursor falls back to the first group.
        set_mode(&mut app, "global");
        set_mode(&mut app, "rule");
        assert_eq!(app.proxies_view.group, 0);
    }

    #[test]
    fn refreshing_within_global_mode_leaves_the_cursor_alone() {
        let (mut app, _) = app_with_proxies();
        set_mode(&mut app, "global");
        app.proxies_view.group = 3;
        set_mode(&mut app, "global");
        assert_eq!(app.proxies_view.group, 3);
    }
}
