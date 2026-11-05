use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::future::join_all;
use jiff::Timestamp;
use ratatui::layout::{Rect, Size};
use tokio::sync::mpsc;
use tui_input::{Input, backend::crossterm::EventHandler};

use crate::{
    api::{
        backoff,
        client::ApiClient,
        error::ApiError,
        models::{
            self, Baseline, Configs, ConnectionSnapshot, ConnectionStats, CoreKind, Delay, GLOBAL,
            Mode, Proxies, Proxy, Rate, Traffic, Version,
        },
        stream::{Event as StreamEvent, Stream, Streams},
    },
    config::Settings,
    format::natural_cmp,
    ui::{connections, proxies, regions, scroll::Scroll, text::describe, theme::Theme},
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
    /// Fastest first, then timed out, failed and untested nodes.
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

/// Frames a new connection stays highlighted: about three seconds at the
/// kernel's default of one frame a second.
const FRESH_FRAMES: u8 = 3;

/// One row of the connections table.
#[derive(Debug, Clone)]
pub struct ConnectionRow {
    pub connection: models::Connection,
    /// What it moved since the frame before; `None` when it was not in that
    /// frame, so there is nothing to measure against.
    pub rate: Option<Rate>,
    /// It appeared within the last [`FRESH_FRAMES`] frames.
    pub fresh: bool,
}

impl ConnectionRow {
    /// The name the table shows: the host, else the address it went to;
    /// empty when the kernel gave neither.
    pub fn host(&self) -> &str {
        let metadata = &self.connection.metadata;
        if metadata.host.is_empty() {
            &metadata.destination_ip
        } else {
            &metadata.host
        }
    }

    fn process(&self) -> &str {
        &self.connection.metadata.process
    }

    /// Whether any field the filter looks at contains `needle`, which is
    /// lowercase already.
    fn matches(&self, needle: &str) -> bool {
        let connection = &self.connection;
        let metadata = &connection.metadata;
        [
            &metadata.host,
            &metadata.destination_ip,
            &metadata.process,
            &connection.rule,
            &connection.rule_payload,
        ]
        .into_iter()
        .chain(&connection.chains)
        .any(|field| field.to_lowercase().contains(needle))
    }
}

/// The columns the connections table can be ordered by, left to right as
/// the table lays them out. The route is not one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Host,
    Process,
    Up,
    Down,
    Total,
    Duration,
}

impl SortColumn {
    const ALL: [Self; 6] = [
        Self::Host,
        Self::Process,
        Self::Up,
        Self::Down,
        Self::Total,
        Self::Duration,
    ];

    /// The direction `s` lands on: names A to Z and the newest connection
    /// first, but the most traffic first.
    fn ascending_first(self) -> bool {
        !matches!(self, Self::Up | Self::Down | Self::Total)
    }
}

/// The order of the connections table. Ascending runs names A to Z, traffic
/// from least to most and durations from shortest (newest) to longest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionSort {
    pub column: SortColumn,
    pub ascending: bool,
}

impl Default for ConnectionSort {
    /// Newest first.
    fn default() -> Self {
        Self {
            column: SortColumn::Duration,
            ascending: true,
        }
    }
}

impl ConnectionSort {
    /// `s`: the next of the `shown` columns to the right, wrapping around,
    /// in its first direction. Columns a narrow terminal hides are skipped,
    /// even when the table is sorted by one of them.
    fn next(self, shown: &[SortColumn]) -> Self {
        let all = SortColumn::ALL;
        let at = all.iter().position(|&c| c == self.column).unwrap_or(0);
        let column = (1..=all.len())
            .map(|step| all[(at + step) % all.len()])
            .find(|column| shown.contains(column))
            .unwrap_or(self.column);
        Self {
            column,
            ascending: column.ascending_first(),
        }
    }

    /// `S`: same column, the other way round.
    fn reversed(self) -> Self {
        Self {
            ascending: !self.ascending,
            ..self
        }
    }

    /// Orders two values in this direction. A missing value goes last
    /// either way: reversing the whole comparison would bring them to the
    /// top.
    fn order<T>(self, a: Option<T>, b: Option<T>, cmp: impl FnOnce(T, T) -> Ordering) -> Ordering {
        match (a, b) {
            (Some(a), Some(b)) if self.ascending => cmp(a, b),
            (Some(a), Some(b)) => cmp(b, a),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }

    /// Sorts `rows` in this order. Ties go by id: the kernel sends the
    /// connections in a different order every frame, and the table must not
    /// shuffle.
    fn sort(self, rows: &mut Vec<ConnectionRow>) {
        let number: fn(&ConnectionRow) -> Option<i128> = match self.column {
            SortColumn::Host => return self.sort_by_text(rows, ConnectionRow::host),
            SortColumn::Process => return self.sort_by_text(rows, ConnectionRow::process),
            SortColumn::Up => |row| row.rate.map(|rate| i128::from(rate.up)),
            SortColumn::Down => |row| row.rate.map(|rate| i128::from(rate.down)),
            SortColumn::Total => {
                |row| Some(i128::from(row.connection.upload) + i128::from(row.connection.download))
            }
            // Started later is shorter so far.
            SortColumn::Duration => |row| row.connection.start.map(|start| -start.as_nanosecond()),
        };
        rows.sort_by(|a, b| {
            self.order(number(a), number(b), |a, b| a.cmp(&b))
                .then_with(|| a.connection.id.cmp(&b.connection.id))
        });
    }

    /// Sorts by a name, compared as the proxies page compares node names:
    /// lowercased, digit runs by value. An empty name counts as missing.
    fn sort_by_text(self, rows: &mut Vec<ConnectionRow>, text: fn(&ConnectionRow) -> &str) {
        // Lowercased once per row, not once per comparison.
        let mut keyed: Vec<(Option<String>, ConnectionRow)> = rows
            .drain(..)
            .map(|row| {
                let key = text(&row).to_lowercase();
                ((!key.is_empty()).then_some(key), row)
            })
            .collect();
        keyed.sort_by(|(a_key, a), (b_key, b)| {
            self.order(a_key.as_deref(), b_key.as_deref(), natural_cmp)
                .then_with(|| a.connection.id.cmp(&b.connection.id))
        });
        rows.extend(keyed.into_iter().map(|(_, row)| row));
    }
}

/// What the connection detail popup shows, while it is open.
#[derive(Debug)]
pub struct Detail {
    /// The connection as of the last table update that had it.
    pub row: ConnectionRow,
    /// When that update was taken; its duration counts up to this.
    pub as_of: Option<Timestamp>,
    /// The table updated without it: the connection has ended.
    pub closed: bool,
}

/// Table, cursor, order, filter and pause state of the connections page.
#[derive(Debug, Default)]
pub struct ConnectionsView {
    /// The table, in `sort` order and unfiltered, as of the last frame it
    /// was updated for. `None` until the first frame, and again while the
    /// stream is down.
    pub rows: Option<Vec<ConnectionRow>>,
    /// When `rows` was taken. Durations count up to it rather than to the
    /// clock, so a paused table keeps still.
    pub as_of: Option<Timestamp>,
    /// Index into [`ConnectionsView::visible`].
    pub cursor: usize,
    pub scroll: Scroll,
    /// The table stays as it is; frames only keep the baseline up to date.
    pub paused: bool,
    pub sort: ConnectionSort,
    /// Case-insensitive substring one of a row's fields must contain.
    pub filter: Input,
    /// The filter box has the keyboard.
    pub typing: bool,
    /// The connection in the detail popup, while it is open.
    pub detail: Option<Detail>,
    /// Totals of the last frame, whether or not it reached the table: the
    /// frame after is measured against it, so resuming shows one frame's
    /// traffic rather than everything moved while paused.
    baseline: Baseline,
    /// Frames of highlight left, for the connections that are still new.
    fresh: HashMap<String, u8>,
}

impl ConnectionsView {
    /// Takes in a frame: rates, highlights and baseline always, the table
    /// (and the popup's connection with it) unless it is paused. The first
    /// frame after (re)connecting is let through even then: a table that was
    /// never filled has nothing to freeze.
    fn apply(&mut self, frame: ConnectionSnapshot, now: Timestamp) {
        // Everything is new then; lighting up the whole table says nothing.
        let first = self.rows.is_none();
        let (rates, baseline) = frame.rates(&self.baseline);
        let mut fresh = HashMap::new();
        for connection in &frame.connections {
            let frames = match self.fresh.get(&connection.id) {
                Some(left) => left.saturating_sub(1),
                None if !first && !self.baseline.contains_key(&connection.id) => FRESH_FRAMES,
                None => 0,
            };
            if frames > 0 {
                fresh.insert(connection.id.clone(), frames);
            }
        }
        self.baseline = baseline;
        self.fresh = fresh;
        if self.paused && !first {
            return;
        }

        let anchor = self.anchor();
        let mut rows: Vec<ConnectionRow> = frame
            .connections
            .into_iter()
            .zip(rates)
            .map(|(connection, rate)| ConnectionRow {
                fresh: self.fresh.contains_key(&connection.id),
                connection,
                rate,
            })
            .collect();
        self.sort.sort(&mut rows);
        if let Some(detail) = &mut self.detail {
            match rows
                .iter()
                .find(|row| row.connection.id == detail.row.connection.id)
            {
                Some(row) => {
                    detail.row = row.clone();
                    detail.as_of = Some(now);
                }
                // What it last had stays up, its clock stopped there.
                None => detail.closed = true,
            }
        }
        self.rows = Some(rows);
        self.as_of = Some(now);
        self.restore(anchor);
    }

    /// The rows that pass the filter, in table order.
    pub fn visible(&self) -> Vec<&ConnectionRow> {
        let Some(rows) = &self.rows else {
            return vec![];
        };
        let needle = self.filter.value().to_lowercase();
        rows.iter()
            .filter(|row| needle.is_empty() || row.matches(&needle))
            .collect()
    }

    /// The connection under the cursor.
    pub fn selected_connection(&self) -> Option<&ConnectionRow> {
        let visible = self.visible();
        let last = visible.len().checked_sub(1)?;
        visible.get(self.cursor.min(last)).copied()
    }

    /// The id and position of the connection under the cursor.
    fn anchor(&self) -> Option<(String, usize)> {
        let visible = self.visible();
        let index = self.cursor.min(visible.len().checked_sub(1)?);
        Some((visible[index].connection.id.clone(), index))
    }

    /// Puts the cursor back on the connection of `anchor`. When that one is
    /// gone or filtered out, it stays at the same position, within the
    /// list.
    fn restore(&mut self, anchor: Option<(String, usize)>) {
        let visible = self.visible();
        self.cursor = anchor
            .map_or(0, |(id, index)| {
                visible
                    .iter()
                    .position(|row| row.connection.id == id)
                    .unwrap_or(index)
            })
            .min(visible.len().saturating_sub(1));
    }

    /// Reorders the table, paused or not, keeping the cursor on its row.
    fn set_sort(&mut self, sort: ConnectionSort) {
        let anchor = self.anchor();
        self.sort = sort;
        if let Some(rows) = &mut self.rows {
            sort.sort(rows);
        }
        self.restore(anchor);
    }

    /// Changes the filter, keeping the cursor on its row.
    fn edit_filter(&mut self, edit: impl FnOnce(&mut Input)) {
        let anchor = self.anchor();
        edit(&mut self.filter);
        self.restore(anchor);
    }

    /// Drops the filter and gives the keyboard back.
    fn clear_filter(&mut self) {
        self.edit_filter(Input::reset);
        self.typing = false;
    }

    /// Takes the connections `closed` picks out of the table at once, paused
    /// or not: a frozen table must not keep what is gone. The cursor keeps
    /// its row, or its position when that row went. The baseline is left
    /// alone; the next frame lines up with it by itself.
    fn remove(&mut self, closed: impl Fn(&str) -> bool) {
        let anchor = self.anchor();
        if let Some(rows) = &mut self.rows {
            rows.retain(|row| !closed(&row.connection.id));
        }
        self.restore(anchor);
    }

    /// The stream is gone: the table, what it was measured against and the
    /// popup's connection go with it. Pausing, the order and the filter are
    /// the user's settings and stay.
    fn reset(&mut self) {
        *self = Self {
            paused: self.paused,
            sort: self.sort,
            filter: std::mem::take(&mut self.filter),
            typing: self.typing,
            ..Self::default()
        };
    }
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
    /// `m`: switch to the next proxy mode.
    CycleMode,
    Cursor(Move),
    /// `Enter`: acts on the item under the cursor: picks a node, opens a
    /// connection's details.
    Confirm,
    /// `x`: close the connection under the cursor.
    CloseConnection,
    /// `X`: ask to close the connections the table shows.
    CloseConnections,
    /// `y`: go ahead with what the confirm dialog asks about.
    Accept,
    /// `n`: drop what the confirm dialog asks about.
    Cancel,
    /// `t`: delay-test the node under the cursor.
    TestNode,
    /// `T`: delay-test every member of the selected group.
    TestGroup,
    /// `s`: next node ordering, or next column the connections are sorted
    /// by.
    CycleSort,
    /// `S`: sort the connections the other way round.
    ReverseSort,
    /// `p`: freeze or resume the connections table.
    TogglePause,
    /// `/`: start typing the current page's filter.
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
    /// Reply to a `/configs` fetch. `refetch` marks the one that follows a
    /// mode switch, whose failure must not take the data on screen away.
    Configs {
        seq: u64,
        refetch: bool,
        result: Result<Configs, ApiError>,
    },
    /// Reply to `PATCH /configs` switching the mode.
    ModeSet(Result<(), ApiError>),
    /// Reply to a `/proxies` fetch. `refetch` marks one made while data is
    /// already on screen, whose failure must not take that data away.
    Proxies {
        seq: u64,
        refetch: bool,
        result: Result<Proxies, ApiError>,
    },
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
    /// Replies to closing connections one request each: `x`, or `X` under a
    /// filter. `report` asks for a toast on success, too.
    ConnectionsClosed {
        results: Vec<(String, Result<(), ApiError>)>,
        report: bool,
    },
    /// Reply to closing every connection; the table held `count` when it was
    /// sent.
    AllConnectionsClosed {
        count: usize,
        result: Result<(), ApiError>,
    },
    /// A WebSocket stream connected, dropped, or delivered a frame.
    Stream {
        stream: Stream,
        event: StreamEvent,
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
            // The pages with a sortable, filterable list.
            let filtered = matches!(page, Page::Proxies | Page::Connections);
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
                KeyCode::Char('s') if filtered => Some(Action::CycleSort),
                KeyCode::Char('S') if page == Page::Connections => Some(Action::ReverseSort),
                KeyCode::Char('/') if filtered => Some(Action::StartFilter),
                KeyCode::Char('p') if page == Page::Connections => Some(Action::TogglePause),
                KeyCode::Char('x') if page == Page::Connections => Some(Action::CloseConnection),
                KeyCode::Char('X') if page == Page::Connections => Some(Action::CloseConnections),
                // Only lowercase: the dialog says `y`, and a dangerous action
                // should not go ahead on a Shift slip.
                KeyCode::Char('y') => Some(Action::Accept),
                KeyCode::Char('n') => Some(Action::Cancel),
                KeyCode::Char('m') => Some(Action::CycleMode),
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
    /// Details of one connection, kept in [`ConnectionsView::detail`].
    Connection,
    /// The question in [`App::pending`].
    Confirm,
}

/// Which connections `X` closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Closing {
    /// Every connection, with one `DELETE /connections`; the table held
    /// `count`.
    All { count: usize },
    /// The ones that passed `filter`, one `DELETE /connections/{id}` each:
    /// closing what the user cannot see would come as a surprise.
    Matching { filter: String, ids: Vec<String> },
}

/// A dangerous action waiting for `y`, with what it is to act on. It says
/// what the dialog shows; the dialog only draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    CloseConnections(Closing),
}

impl Confirm {
    pub fn title(&self) -> &'static str {
        match self {
            Self::CloseConnections(_) => "关闭连接",
        }
    }

    pub fn body(&self) -> String {
        match self {
            Self::CloseConnections(Closing::All { count }) => format!("关闭全部 {count} 条连接？"),
            Self::CloseConnections(Closing::Matching { filter, ids }) => {
                format!("关闭匹配「{filter}」的 {} 条连接？", ids.len())
            }
        }
    }

    /// What `y` does, for the dialog and the key bar.
    pub fn verb(&self) -> &'static str {
        match self {
            Self::CloseConnections(_) => "确认",
        }
    }
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

/// Which WebSocket streams are connected.
#[derive(Debug, Default)]
struct Links {
    up: HashSet<Stream>,
    /// Every stream went down after one had been up, and none has come back
    /// yet.
    outage: bool,
}

/// Progress of a mode switch. Pressing `m` again before it ends would race
/// two requests, so it waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeSwitch {
    Idle,
    /// `PATCH /configs` is in flight.
    Patching,
    /// Patched; waiting for the `/configs` fetch with this number (or a
    /// later one), which says what the kernel now runs.
    Refetching(u64),
}

/// Samples kept per chart or sparkline: one per second of the last minute.
pub const TRAFFIC_HISTORY: usize = 60;

/// Appends `sample`, dropping the oldest once [`TRAFFIC_HISTORY`] are kept.
fn push_sample<T>(history: &mut VecDeque<T>, sample: T) {
    if history.len() == TRAFFIC_HISTORY {
        history.pop_front();
    }
    history.push_back(sample);
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
    /// `/configs` fetches issued so far; numbers them.
    config_fetches: u64,
    /// The newest `/configs` fetch whose reply has been applied.
    applied_config: u64,
    mode_switch: ModeSwitch,
    /// Nodes with a delay test in flight.
    pub testing: HashSet<String>,
    /// `/proxies` fetches issued so far; numbers them.
    fetches: u64,
    /// The newest fetch whose reply has been applied; older ones are dropped.
    applied_fetch: u64,
    /// Delay results measured here, with the value of `fetches` when each was
    /// recorded. A reply replaces one only if its fetch came after it.
    local_delays: HashMap<String, u64>,
    pub toasts: VecDeque<Toast>,
    pub overlay: Option<Overlay>,
    /// What the confirm dialog asks about, while it is open.
    pub pending: Option<Confirm>,
    /// Frame counter of the loading spinner; advances while one is visible.
    pub spinner: usize,
    streams: Streams,
    links: Links,
    /// The latest `/traffic` frame; `None` while that stream is down.
    pub traffic: Option<Traffic>,
    /// The latest [`TRAFFIC_HISTORY`] `/traffic` frames, oldest first; empty
    /// while that stream is down.
    pub traffic_history: VecDeque<Traffic>,
    /// Counts of the latest `/connections` frame; `None` while that stream is
    /// down.
    pub connections: Option<ConnectionStats>,
    /// The connections table, from the same stream.
    pub connections_view: ConnectionsView,
    /// Open connections over the last [`TRAFFIC_HISTORY`] frames, oldest
    /// first; empty while that stream is down.
    pub connection_history: VecDeque<u64>,
    /// Bytes in use by the kernel, from the latest `/memory` frame; `None`
    /// while that stream is down or has not reported yet. mihomo only.
    pub memory: Option<u64>,
    /// The last [`TRAFFIC_HISTORY`] `/memory` frames, oldest first; empty
    /// while that stream is down.
    pub memory_history: VecDeque<u64>,
    /// `Some` from the first failed `/version` probe until one succeeds.
    pub reconnect: Option<Reconnect>,
    /// Set by `r` until the next `/version` reply, which then reports success.
    refreshing: bool,
    should_quit: bool,
    /// The terminal size, from the start and every resize event.
    pub size: Size,
    dirty: bool,
}

impl App {
    pub fn new(
        settings: Settings,
        client: ApiClient,
        msg_tx: mpsc::Sender<Msg>,
        size: Size,
    ) -> Self {
        let streams = Streams::new(&settings.url, settings.secret.as_deref(), msg_tx.clone());
        Self {
            theme: settings.theme.into(),
            settings,
            page: Page::Overview,
            client,
            msg_tx,
            version: None,
            configs: None,
            proxies: None,
            config_fetches: 0,
            applied_config: 0,
            mode_switch: ModeSwitch::Idle,
            proxies_view: ProxiesView::default(),
            testing: HashSet::new(),
            fetches: 0,
            applied_fetch: 0,
            local_delays: HashMap::new(),
            toasts: VecDeque::new(),
            overlay: None,
            pending: None,
            spinner: 0,
            streams,
            links: Links::default(),
            traffic: None,
            traffic_history: VecDeque::with_capacity(TRAFFIC_HISTORY),
            connections: None,
            connections_view: ConnectionsView::default(),
            connection_history: VecDeque::with_capacity(TRAFFIC_HISTORY),
            memory: None,
            memory_history: VecDeque::with_capacity(TRAFFIC_HISTORY),
            reconnect: None,
            refreshing: false,
            should_quit: false,
            size,
            dirty: false,
        }
    }

    /// Spawns the startup requests and the WebSocket streams. Calling it
    /// again repeats the requests; running streams are left alone.
    pub fn start(&mut self) {
        self.fetch_version();
        self.fetch_configs(false);
        self.fetch_proxies(false);
        for stream in [Stream::Traffic, Stream::Logs, Stream::Connections] {
            self.streams.start(stream);
        }
    }

    fn fetch_version(&self) {
        self.spawn(|client| async move { Msg::Version(client.version().await) });
    }

    /// Fetches `/proxies` in the background.
    fn fetch_proxies(&mut self, refetch: bool) {
        self.fetches += 1;
        let seq = self.fetches;
        self.spawn(move |client| async move {
            Msg::Proxies {
                seq,
                refetch,
                result: client.proxies().await,
            }
        });
    }

    /// Fetches `/configs` in the background and returns the fetch's number.
    fn fetch_configs(&mut self, refetch: bool) -> u64 {
        self.config_fetches += 1;
        let seq = self.config_fetches;
        self.spawn(move |client| async move {
            Msg::Configs {
                seq,
                refetch,
                result: client.configs().await,
            }
        });
        seq
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
            _ if self.overlay == Some(Overlay::Confirm) => self.answer(action),
            Action::ToggleHelp => {
                // The connection popup is not swapped for the help.
                self.overlay = match self.overlay {
                    Some(Overlay::Help) => None,
                    None => Some(Overlay::Help),
                    other => other,
                };
                self.dirty = true;
            }
            Action::Dismiss => {
                if self.overlay.is_some() {
                    self.close_overlay();
                } else {
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
            Action::CycleMode => self.cycle_mode(),
            Action::Confirm => self.confirm(),
            Action::CloseConnection => self.close_connection(),
            Action::CloseConnections => self.ask_close_connections(),
            // Only the confirm dialog takes these.
            Action::Accept | Action::Cancel => {}
            Action::TestNode => self.test_node(),
            Action::TestGroup => self.test_group(),
            Action::CycleSort => {
                match self.page {
                    Page::Proxies => {
                        self.proxies_view.sort = self.proxies_view.sort.next();
                        self.proxies_view.node = None;
                    }
                    Page::Connections => {
                        let shown: Vec<SortColumn> = connections::sort_columns(self.size).collect();
                        let view = &mut self.connections_view;
                        view.set_sort(view.sort.next(&shown));
                    }
                    _ => {}
                }
                self.dirty = true;
            }
            Action::ReverseSort => {
                let view = &mut self.connections_view;
                view.set_sort(view.sort.reversed());
                self.dirty = true;
            }
            Action::TogglePause => {
                self.connections_view.paused = !self.connections_view.paused;
                self.dirty = true;
            }
            Action::StartFilter => self.set_typing(true),
            Action::FilterInput(key) => {
                let event = Event::Key(key);
                match self.page {
                    Page::Proxies => {
                        self.proxies_view.filter.handle_event(&event);
                        self.proxies_view.node = None;
                    }
                    Page::Connections => self.connections_view.edit_filter(|filter| {
                        filter.handle_event(&event);
                    }),
                    _ => {}
                }
                self.dirty = true;
            }
            Action::FilterAccept => self.set_typing(false),
            Action::FilterClear => {
                self.clear_filter();
                self.dirty = true;
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
                self.streams.retry_now();
                self.start();
            }
        }
    }

    /// `m`: asks the kernel for the next mode and, once it agrees, fetches
    /// `/configs` to show what it really runs. An unknown mode goes to rule.
    fn cycle_mode(&mut self) {
        if self.mode_switch != ModeSwitch::Idle {
            return;
        }
        let Some(Ok(configs)) = &self.configs else {
            self.toast(ToastKind::Error, "尚未获取到当前模式".to_owned());
            return;
        };
        let target = configs.mode().map_or(Mode::Rule, Mode::next);
        self.mode_switch = ModeSwitch::Patching;
        self.spawn(move |client| async move { Msg::ModeSet(client.set_mode(target).await) });
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
        matches!(&self.configs, Some(Ok(configs)) if configs.mode() == Some(Mode::Global))
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
        if self.page == Page::Connections {
            self.move_in_connections(movement);
            return;
        }
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

    /// Table rows that fit the terminal, laid out by the function that
    /// draws them.
    fn connections_capacity(&self) -> usize {
        let screen = Rect::from(self.size);
        connections::capacity(regions(screen, self.reconnect.is_some()).body)
    }

    fn move_in_connections(&mut self, movement: Move) {
        let half = (self.connections_capacity() / 2).max(1);
        let view = &mut self.connections_view;
        let len = view.visible().len();
        view.cursor = movement.apply(view.cursor, len, half);
        self.dirty = true;
    }

    /// Whether the current page's filter box has the keyboard.
    pub fn typing(&self) -> bool {
        match self.page {
            Page::Proxies => self.proxies_view.typing,
            Page::Connections => self.connections_view.typing,
            _ => false,
        }
    }

    /// The current page's filter, on pages that have one.
    pub fn filter(&self) -> Option<&Input> {
        match self.page {
            Page::Proxies => Some(&self.proxies_view.filter),
            Page::Connections => Some(&self.connections_view.filter),
            _ => None,
        }
    }

    /// Gives the keyboard to the current page's filter box, or back.
    fn set_typing(&mut self, typing: bool) {
        match self.page {
            Page::Proxies => self.proxies_view.typing = typing,
            Page::Connections => self.connections_view.typing = typing,
            _ => {}
        }
        self.dirty = true;
    }

    /// Drops the current page's filter and gives the keyboard back.
    fn clear_filter(&mut self) {
        match self.page {
            Page::Proxies => {
                self.proxies_view.filter.reset();
                self.proxies_view.typing = false;
                self.proxies_view.node = None;
            }
            Page::Connections => self.connections_view.clear_filter(),
            _ => {}
        }
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
            Sort::Name => {
                let mut keyed: Vec<(String, &str)> = nodes
                    .into_iter()
                    .map(|name| (name.to_lowercase(), name))
                    .collect();
                keyed.sort_by(|(a, _), (b, _)| natural_cmp(a, b));
                nodes = keyed.into_iter().map(|(_, name)| name).collect();
            }
            Sort::Delay => nodes.sort_by_key(|name| {
                match proxies
                    .and_then(|proxies| proxies.get(name))
                    .and_then(Proxy::delay)
                {
                    Some(Delay::Millis(millis)) => (0, millis),
                    Some(Delay::Timeout) => (1, 0),
                    Some(Delay::Failed) => (2, 0),
                    None => (3, 0),
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

    /// `Enter`: on the proxies page, from the group list, moves into the
    /// nodes; on a node, switches the group to it. On the connections page,
    /// opens the details of the connection under the cursor.
    fn confirm(&mut self) {
        match self.page {
            Page::Proxies if self.selected_group().is_some() => match self.proxies_view.focus {
                Pane::Groups => {
                    self.proxies_view.focus = Pane::Nodes;
                    self.dirty = true;
                }
                Pane::Nodes => self.select_node(),
            },
            Page::Connections => {
                let view = &mut self.connections_view;
                let Some(row) = view.selected_connection() else {
                    return;
                };
                view.detail = Some(Detail {
                    row: row.clone(),
                    as_of: view.as_of,
                    closed: false,
                });
                self.overlay = Some(Overlay::Connection);
                self.dirty = true;
            }
            _ => {}
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

    /// The confirm dialog answers to `y`, `n` and `Esc` only; quitting and
    /// resizing are handled before it is asked.
    fn answer(&mut self, action: Action) {
        match action {
            Action::Accept => self.accept(),
            Action::Cancel | Action::Dismiss => self.close_overlay(),
            _ => {}
        }
    }

    /// Closes the overlay along with what it showed.
    fn close_overlay(&mut self) {
        match self.overlay.take() {
            Some(Overlay::Connection) => self.connections_view.detail = None,
            Some(Overlay::Confirm) => self.pending = None,
            Some(Overlay::Help) | None => {}
        }
        self.dirty = true;
    }

    /// `y`: closes the confirm dialog and does what it asked about.
    fn accept(&mut self) {
        self.overlay = None;
        self.dirty = true;
        let Some(pending) = self.pending.take() else {
            return;
        };
        match pending {
            Confirm::CloseConnections(closing) => self.close_connections(closing),
        }
    }

    /// `x`: closes the connection under the cursor. No question asked: it
    /// is one connection, the one in plain sight.
    fn close_connection(&self) {
        if let Some(row) = self.connections_view.selected_connection() {
            self.close_each(vec![row.connection.id.clone()], false);
        }
    }

    /// `X`: asks before closing what the table shows: every connection, or
    /// with a filter only the ones that pass it. Nothing shown, nothing
    /// asked.
    fn ask_close_connections(&mut self) {
        let view = &self.connections_view;
        let visible = view.visible();
        if visible.is_empty() {
            return;
        }
        let filter = view.filter.value();
        let closing = if filter.is_empty() {
            Closing::All {
                count: visible.len(),
            }
        } else {
            Closing::Matching {
                filter: filter.to_owned(),
                ids: visible
                    .iter()
                    .map(|row| row.connection.id.clone())
                    .collect(),
            }
        };
        self.pending = Some(Confirm::CloseConnections(closing));
        self.overlay = Some(Overlay::Confirm);
        self.dirty = true;
    }

    /// Closes what `X` was confirmed for, as far as the table still has it:
    /// connections may have ended meanwhile, and without a table there is
    /// nothing to go by.
    fn close_connections(&self, closing: Closing) {
        let Some(rows) = &self.connections_view.rows else {
            return;
        };
        match closing {
            Closing::All { .. } => {
                let count = rows.len();
                if count > 0 {
                    self.spawn(move |client| async move {
                        Msg::AllConnectionsClosed {
                            count,
                            result: client.close_all_connections().await,
                        }
                    });
                }
            }
            Closing::Matching { mut ids, .. } => {
                let present: HashSet<&str> =
                    rows.iter().map(|row| row.connection.id.as_str()).collect();
                ids.retain(|id| present.contains(id.as_str()));
                if !ids.is_empty() {
                    self.close_each(ids, true);
                }
            }
        }
    }

    /// Closes `ids`, one request each, all sent at once; the replies come
    /// back together in one message.
    fn close_each(&self, ids: Vec<String>, report: bool) {
        self.spawn(move |client| async move {
            let results = join_all(ids.iter().map(|id| client.close_connection(id))).await;
            Msg::ConnectionsClosed {
                results: ids.into_iter().zip(results).collect(),
                report,
            }
        });
    }

    /// Takes the connections that closed out of the table and reports the
    /// ones that did not in a single toast. A 404 means the connection had
    /// ended by itself: it is gone, as asked, and counts as closed (mihomo
    /// replies 204 to an id it does not know anyway). `report` adds a toast
    /// for the ones closed.
    fn apply_closed(&mut self, results: Vec<(String, Result<(), ApiError>)>, report: bool) {
        let mut closed = HashSet::new();
        let mut failed = vec![];
        for (id, result) in results {
            match result {
                Ok(()) | Err(ApiError::NotFound) => {
                    closed.insert(id);
                }
                Err(err) => failed.push(err),
            }
        }
        self.connections_view.remove(|id| closed.contains(id));
        if report && !closed.is_empty() {
            self.toast(
                ToastKind::Success,
                format!("已关闭 {} 条连接", closed.len()),
            );
        }
        if let Some(first) = failed.first() {
            let text = if report {
                format!("{} 条关闭失败：{}", failed.len(), describe(first))
            } else {
                format!("关闭连接失败：{}", describe(first))
            };
            self.toast(ToastKind::Error, text);
        }
    }

    /// Applies a `/version` reply: the connection state, the banner and the
    /// toast that goes with it.
    fn apply_version(&mut self, result: Result<Version, ApiError>) {
        let refreshed = std::mem::take(&mut self.refreshing);
        match &result {
            Ok(version) => {
                // Memory is a mihomo endpoint; Clash has no such route.
                if version.kind() == CoreKind::Mihomo {
                    self.streams.start(Stream::Memory);
                }
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

    /// Swaps in a `/configs` reply from fetch `seq`, and ends the mode
    /// switch if this is the refetch it waited for (or a later one).
    fn apply_configs(&mut self, seq: u64, refetch: bool, result: Result<Configs, ApiError>) {
        self.applied_config = seq;
        if matches!(self.mode_switch, ModeSwitch::Refetching(awaited) if seq >= awaited) {
            self.mode_switch = ModeSwitch::Idle;
        }
        self.note_failure(&result);
        // A failed refetch keeps the mode on screen; the toast says why.
        if refetch && result.is_err() && matches!(self.configs, Some(Ok(_))) {
            return;
        }
        let was_global = self.is_global_mode();
        let selected = self.selected_group().map(|group| group.name.clone());
        self.configs = Some(result);
        self.follow_mode_change(was_global, selected);
    }

    /// The kernel's answer to `PATCH /configs`: on success, asks what it now
    /// runs rather than assuming.
    fn apply_mode_set(&mut self, result: Result<(), ApiError>) {
        match result {
            Ok(()) => {
                let seq = self.fetch_configs(true);
                self.mode_switch = ModeSwitch::Refetching(seq);
            }
            Err(err) => {
                self.mode_switch = ModeSwitch::Idle;
                self.toast(
                    ToastKind::Error,
                    format!("切换模式失败：{}", describe(&err)),
                );
            }
        }
    }

    pub fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Version(result) => self.apply_version(result),
            // A reply older than one already applied would roll the mode back.
            Msg::Configs { seq, .. } if seq < self.applied_config => {}
            Msg::Configs {
                seq,
                refetch,
                result,
            } => self.apply_configs(seq, refetch, result),
            Msg::ModeSet(result) => self.apply_mode_set(result),
            // A reply older than one already applied would roll the page back.
            Msg::Proxies { seq, .. } if seq < self.applied_fetch => {}
            Msg::Proxies {
                seq,
                refetch,
                result,
            } => {
                self.applied_fetch = seq;
                self.note_failure(&result);
                match result {
                    Ok(fresh) => self.apply_proxies(fresh, seq),
                    Err(_) if refetch && matches!(self.proxies, Some(Ok(_))) => {}
                    Err(err) => self.proxies = Some(Err(err)),
                }
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
                    self.fetch_proxies(true);
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
                let tested = match result {
                    // Members that failed or timed out are absent from the reply.
                    Ok(delays) => {
                        for member in &members {
                            let delay = delays
                                .get(member)
                                .map_or(Delay::Failed, |millis| Delay::from_millis(*millis));
                            self.record_delay(member, delay);
                        }
                        true
                    }
                    // No member answered: the kernel replies 504 instead of `{}`.
                    Err(err) if failed_test(&err).is_some() => {
                        for member in &members {
                            self.record_delay(member, Delay::Failed);
                        }
                        true
                    }
                    Err(err) => {
                        self.toast(ToastKind::Error, format!("测速失败：{}", describe(&err)));
                        false
                    }
                };
                if tested {
                    self.fetch_proxies(true);
                }
            }
            Msg::ConnectionsClosed { results, report } => self.apply_closed(results, report),
            Msg::AllConnectionsClosed { count, result } => match result {
                Ok(()) => {
                    self.connections_view.remove(|_| true);
                    self.toast(ToastKind::Success, format!("已关闭 {count} 条连接"));
                }
                Err(err) => self.toast(
                    ToastKind::Error,
                    format!("关闭连接失败：{}", describe(&err)),
                ),
            },
            Msg::Stream { stream, event } => self.on_stream(stream, event),
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

    /// Applies a stream event. The banner stays driven by the `/version`
    /// probe alone; the streams only say when to probe or restart.
    ///
    /// - Every stream down at once, with no banner yet: probe now, instead of
    ///   waiting for the next request to fail.
    /// - First stream back after that: the kernel may have been replaced, so
    ///   repeat the startup requests, as a REST retry does. Streams that are
    ///   already running are left alone.
    fn on_stream(&mut self, stream: Stream, event: StreamEvent) {
        match event {
            StreamEvent::Connected => {
                self.links.up.insert(stream);
                if std::mem::take(&mut self.links.outage) {
                    self.start();
                }
            }
            StreamEvent::Disconnected => {
                // Only a stream that was up can take the last one down; the
                // first connection attempts at start-up are not an outage.
                let was_up = self.links.up.remove(&stream);
                match stream {
                    Stream::Traffic => {
                        self.traffic = None;
                        self.traffic_history.clear();
                    }
                    Stream::Connections => {
                        self.connections = None;
                        self.connection_history.clear();
                        self.connections_view.reset();
                        // What the popup or the dialog was about went with
                        // the table.
                        if self.overlay == Some(Overlay::Connection)
                            || matches!(self.pending, Some(Confirm::CloseConnections(_)))
                        {
                            self.close_overlay();
                        }
                    }
                    Stream::Memory => {
                        self.memory = None;
                        self.memory_history.clear();
                    }
                    Stream::Logs => {}
                }
                if was_up && self.links.up.is_empty() {
                    self.links.outage = true;
                    if self.reconnect.is_none() {
                        self.fetch_version();
                    }
                }
            }
            StreamEvent::Traffic(traffic) => {
                self.traffic = Some(traffic);
                push_sample(&mut self.traffic_history, traffic);
            }
            StreamEvent::Connections(frame) => {
                let stats = frame.stats();
                self.connections = Some(stats);
                push_sample(&mut self.connection_history, stats.active);
                self.connections_view.apply(frame, Timestamp::now());
            }
            // The first frame after connecting reports 0: no process uses
            // none, it is the kernel having sampled nothing yet.
            StreamEvent::Memory(memory) if memory.inuse > 0 => {
                self.memory = Some(memory.inuse);
                push_sample(&mut self.memory_history, memory.inuse);
            }
            StreamEvent::Memory(_) => {}
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
            Page::Connections => self.connections_view.rows.is_none(),
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
            self.local_delays.insert(name.to_owned(), self.fetches);
        }
    }

    /// Swaps in a fresh `/proxies` reply from fetch `seq`. Delay results
    /// measured after that fetch was issued survive it, and the node cursor
    /// stays on its node.
    fn apply_proxies(&mut self, mut fresh: Proxies, seq: u64) {
        let anchor = self.node_anchor();
        if let Some(Ok(earlier)) = &self.proxies {
            self.local_delays
                .retain(|name, recorded| *recorded >= seq && fresh.carry_delay(earlier, name));
        } else {
            self.local_delays.clear();
        }
        self.proxies = Some(Ok(fresh));
        self.restore_node_cursor(anchor);
    }

    /// The group, position and name the node cursor is on.
    fn node_anchor(&self) -> Option<(String, usize, String)> {
        let group = self.selected_group()?.name.clone();
        let index = self.node_cursor();
        let name = (*self.visible_nodes().get(index)?).to_owned();
        Some((group, index, name))
    }

    /// Puts the node cursor back on the node it was on. A node that is gone
    /// leaves it at the same position, which is the nearest card left.
    fn restore_node_cursor(&mut self, anchor: Option<(String, usize, String)>) {
        let Some((group, index, name)) = anchor else {
            return;
        };
        if self.selected_group().map(|g| g.name.as_str()) != Some(group.as_str()) {
            self.proxies_view.node = None;
            return;
        }
        let position = self
            .visible_nodes()
            .iter()
            .position(|node| *node == name)
            .unwrap_or(index);
        self.proxies_view.node = Some(position);
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
        let entering_proxies = page == Page::Proxies && self.page != Page::Proxies;
        self.page = page;
        self.dirty = true;
        // Shows what is there and refreshes behind it, without a spinner.
        if entering_proxies && self.proxies.is_some() {
            self.fetch_proxies(true);
        }
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
    use crate::api::models::{Connection, ConnectionSnapshot, Memory};

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

    /// An app on a controller nobody listens on, so requests it spawns fail
    /// fast instead of reaching a real kernel.
    fn app_without_controller() -> (App, mpsc::Receiver<Msg>) {
        app_at("http://127.0.0.1:1")
    }

    fn app_at(url: &str) -> (App, mpsc::Receiver<Msg>) {
        let (tx, rx) = mpsc::channel(16);
        let overrides = crate::config::Overrides {
            url: Some(url.to_owned()),
            ..Default::default()
        };
        let settings = crate::config::Config::default()
            .resolve(&overrides)
            .unwrap();
        let client = ApiClient::new(&settings.url, None).unwrap();
        (App::new(settings, client, tx, Size::new(120, 40)), rx)
    }

    fn app_with(proxies: Proxies) -> App {
        let (mut app, _rx) = app_without_controller();
        app.proxies = Some(Ok(proxies));
        app
    }

    fn app_with_proxies() -> (App, Vec<String>) {
        let proxies: Proxies =
            serde_json::from_str(include_str!("../tests/fixtures/mihomo_proxies.json")).unwrap();
        let members = proxies.groups().next().unwrap().all.clone().unwrap();
        (app_with(proxies), members)
    }

    /// A reply with one selector group `G` over `nodes`, each given with the
    /// delays of its history.
    fn reply(nodes: &[(&str, &[u32])]) -> Proxies {
        let mut proxies = serde_json::Map::new();
        let names: Vec<&str> = nodes.iter().map(|(name, _)| *name).collect();
        proxies.insert(
            "G".to_owned(),
            serde_json::json!({"name": "G", "type": "Selector", "all": names, "now": names[0]}),
        );
        for (name, delays) in nodes {
            let history: Vec<_> = delays
                .iter()
                .map(|delay| serde_json::json!({"delay": delay}))
                .collect();
            proxies.insert(
                (*name).to_owned(),
                serde_json::json!({"name": name, "type": "Direct", "history": history}),
            );
        }
        serde_json::from_value(serde_json::json!({ "proxies": proxies })).unwrap()
    }

    fn fetched(seq: u64, refetch: bool, proxies: Proxies) -> Msg {
        Msg::Proxies {
            seq,
            refetch,
            result: Ok(proxies),
        }
    }

    fn cursor_name(app: &App) -> String {
        app.visible_nodes()[app.node_cursor()].to_owned()
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
    fn request_errors_are_not_test_results() {
        // 香港 01 has a result already: it must survive every one of these.
        let node = "香港 01";
        for err in [
            http(500),
            ApiError::NotFound,
            ApiError::Unauthorized,
            ApiError::Connect("refused".to_owned()),
        ] {
            let (mut app, _) = app_with_proxies();
            let before = delay_of(&app, node);
            assert!(matches!(before, Some(Delay::Millis(_))));
            app.testing.insert(node.to_owned());
            app.handle_msg(Msg::Delay {
                name: node.to_owned(),
                result: Err(err),
            });
            assert_eq!(delay_of(&app, node), before);
            assert!(!app.testing.contains(node));
            assert_eq!(app.toasts.len(), 1);
        }
    }

    #[tokio::test]
    async fn group_test_counts_missing_members_as_failed() {
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

    #[tokio::test]
    async fn group_test_without_any_answer_fails_every_member() {
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

    /// Delivers a `/configs` reply with the next fetch number.
    fn set_mode(app: &mut App, mode: &str) {
        app.config_fetches += 1;
        configs_reply(app, app.config_fetches, mode);
    }

    fn configs_reply(app: &mut App, seq: u64, mode: &str) {
        app.handle_msg(Msg::Configs {
            seq,
            refetch: false,
            result: Ok(Configs {
                mode: mode.to_owned(),
                ..Configs::default()
            }),
        });
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

    #[tokio::test]
    async fn refetch_follows_a_switch_and_a_finished_group_test() {
        let (mut app, members) = app_with_proxies();
        let group = app.selected_group().unwrap().name.clone();
        let node = members[0].clone();
        let switched = |result| Msg::Selected {
            group: group.clone(),
            node: node.clone(),
            result,
        };

        app.handle_msg(switched(Err(http(500))));
        assert_eq!(app.fetches, 0);
        app.handle_msg(switched(Ok(())));
        assert_eq!(app.fetches, 1);

        let group_test = |result| Msg::GroupDelay {
            members: members[..2].to_vec(),
            result,
        };
        app.handle_msg(group_test(Err(http(500))));
        assert_eq!(app.fetches, 1);
        app.handle_msg(group_test(Ok(HashMap::new())));
        assert_eq!(app.fetches, 2);
        app.handle_msg(group_test(Err(http(504))));
        assert_eq!(app.fetches, 3);
    }

    #[tokio::test]
    async fn entering_the_proxies_page_refetches_once_data_is_there() {
        let (mut app, _) = app_with_proxies();
        app.update(Action::SelectPage(Page::Proxies));
        assert_eq!(app.fetches, 1);
        // Already there: nothing to refresh.
        app.update(Action::SelectPage(Page::Proxies));
        assert_eq!(app.fetches, 1);

        // The first load is still in flight: it is not asked twice.
        let (mut app, _) = app_with_proxies();
        app.proxies = None;
        app.update(Action::SelectPage(Page::Proxies));
        assert_eq!(app.fetches, 0);
    }

    #[tokio::test]
    async fn refetch_does_not_erase_a_result_measured_after_it_was_issued() {
        let mut app = app_with(reply(&[("a", &[]), ("b", &[])]));
        app.fetch_proxies(true);
        app.testing.insert("b".to_owned());
        app.record_delay("a", Delay::Millis(50));

        // The reply to a fetch issued before the test knows nothing of it.
        app.handle_msg(fetched(1, true, reply(&[("a", &[]), ("b", &[])])));
        assert_eq!(delay_of(&app, "a"), Some(Delay::Millis(50)));
        assert!(app.testing.contains("b"));

        // A fetch issued afterwards does: the server's data takes over.
        app.fetch_proxies(true);
        app.handle_msg(fetched(2, true, reply(&[("a", &[70]), ("b", &[])])));
        assert_eq!(delay_of(&app, "a"), Some(Delay::Millis(70)));
    }

    #[tokio::test]
    async fn reply_older_than_the_applied_one_is_dropped() {
        let mut app = app_with(reply(&[("a", &[])]));
        app.handle_msg(fetched(2, true, reply(&[("a", &[70])])));
        app.handle_msg(fetched(1, true, reply(&[("a", &[90])])));
        assert_eq!(delay_of(&app, "a"), Some(Delay::Millis(70)));
    }

    #[tokio::test]
    async fn failed_refetch_keeps_the_data_but_failed_load_does_not() {
        let mut app = app_with(reply(&[("a", &[])]));
        let failed = |seq, refetch| Msg::Proxies {
            seq,
            refetch,
            result: Err(http(500)),
        };
        app.handle_msg(failed(1, true));
        assert!(matches!(app.proxies, Some(Ok(_))));
        assert_eq!(app.toasts.len(), 1);
        app.handle_msg(failed(2, false));
        assert!(matches!(app.proxies, Some(Err(_))));
    }

    #[tokio::test]
    async fn refetch_keeps_the_cursor_on_its_node() {
        let mut app = app_with(reply(&[("a", &[]), ("b", &[]), ("c", &[])]));
        app.proxies_view.node = Some(1);
        app.handle_msg(fetched(
            1,
            true,
            reply(&[("x", &[]), ("y", &[]), ("a", &[]), ("b", &[]), ("c", &[])]),
        ));
        assert_eq!(cursor_name(&app), "b");
        assert_eq!(app.node_cursor(), 3);
    }

    #[tokio::test]
    async fn refetch_moves_the_cursor_to_the_nearest_card_when_its_node_is_gone() {
        let nodes = [("a", &[][..]), ("b", &[]), ("c", &[]), ("d", &[])];
        let mut app = app_with(reply(&nodes));
        app.proxies_view.node = Some(2);
        app.handle_msg(fetched(1, true, reply(&[nodes[0], nodes[1], nodes[3]])));
        assert_eq!(cursor_name(&app), "d");

        // Gone from the end of the grid: the last card is the nearest.
        app.handle_msg(fetched(2, true, reply(&nodes[..2])));
        assert_eq!(cursor_name(&app), "b");
    }

    #[test]
    fn delay_sort_runs_fastest_then_timed_out_failed_untested() {
        let mut app = app_with(reply(&[
            ("untested-1", &[]),
            ("failed-1", &[0]),
            ("slow-a", &[300]),
            ("timeout-1", &[]),
            ("fast", &[20]),
            ("failed-2", &[0]),
            ("slow-b", &[300]),
            ("timeout-2", &[]),
            ("untested-2", &[]),
        ]));
        app.record_delay("timeout-1", Delay::Timeout);
        app.record_delay("timeout-2", Delay::Timeout);
        app.proxies_view.sort = Sort::Delay;
        // Ties inside a class keep the configured order.
        assert_eq!(
            app.visible_nodes(),
            [
                "fast",
                "slow-a",
                "slow-b",
                "timeout-1",
                "timeout-2",
                "failed-1",
                "failed-2",
                "untested-1",
                "untested-2",
            ]
        );
    }

    #[test]
    fn name_sort_compares_digit_runs_as_numbers() {
        let mut app = app_with(reply(&[
            ("香港 10", &[]),
            ("香港 2", &[]),
            ("香港 1", &[]),
            ("香港 100", &[]),
        ]));
        app.proxies_view.sort = Sort::Name;
        assert_eq!(
            app.visible_nodes(),
            ["香港 1", "香港 2", "香港 10", "香港 100"]
        );
    }

    #[test]
    fn name_sort_mixes_digits_and_text_by_lowercase_unicode() {
        let mut app = app_with(reply(&[
            ("node10", &[]),
            ("Node2", &[]),
            ("node", &[]),
            ("10node", &[]),
            ("2node", &[]),
            ("a-1", &[]),
            ("ab", &[]),
            ("a1", &[]),
        ]));
        app.proxies_view.sort = Sort::Name;
        // A digit run is only a number against another digit run; against
        // anything else it is a plain character: `-` < digits < letters.
        assert_eq!(
            app.visible_nodes(),
            [
                "2node", "10node", "a-1", "a1", "ab", "node", "Node2", "node10"
            ]
        );
    }

    #[test]
    fn name_sort_keeps_config_order_for_equal_numbers() {
        let mut app = app_with(reply(&[
            ("a1", &[]),
            ("a01", &[]),
            ("a001", &[]),
            ("a0", &[]),
        ]));
        app.proxies_view.sort = Sort::Name;
        assert_eq!(app.visible_nodes(), ["a0", "a1", "a01", "a001"]);
    }

    fn stream_event(app: &mut App, stream: Stream, event: StreamEvent) {
        app.handle_msg(Msg::Stream { stream, event });
    }

    fn connect(app: &mut App, stream: Stream) {
        stream_event(app, stream, StreamEvent::Connected);
    }

    fn disconnect(app: &mut App, stream: Stream) {
        stream_event(app, stream, StreamEvent::Disconnected);
    }

    /// How many `/version`, `/configs`, `/proxies` and mode `PATCH` requests
    /// the app made, counted from the replies they leave on the channel.
    /// Dropping the app first stops its streams, so the channel closes once
    /// the requests are answered and nothing can still be on its way.
    async fn requests_made(app: App, mut rx: mpsc::Receiver<Msg>) -> (usize, usize, usize, usize) {
        drop(app);
        let (mut versions, mut configs, mut proxies, mut modes) = (0, 0, 0, 0);
        while let Some(msg) = rx.recv().await {
            match msg {
                Msg::Version(_) => versions += 1,
                Msg::Configs { .. } => configs += 1,
                Msg::Proxies { .. } => proxies += 1,
                Msg::ModeSet(_) => modes += 1,
                _ => {}
            }
        }
        (versions, configs, proxies, modes)
    }

    #[tokio::test]
    async fn losing_every_stream_probes_the_controller_at_once() {
        let (mut app, rx) = app_without_controller();
        connect(&mut app, Stream::Traffic);
        connect(&mut app, Stream::Logs);
        // One stream still up: the controller is evidently there, no probe.
        disconnect(&mut app, Stream::Traffic);
        disconnect(&mut app, Stream::Logs);
        assert_eq!(requests_made(app, rx).await, (1, 0, 0, 0));
    }

    #[tokio::test]
    async fn streams_that_never_connected_are_no_outage() {
        let (mut app, rx) = app_without_controller();
        app.start();
        // A stream reporting a hang-up it never connected for.
        disconnect(&mut app, Stream::Traffic);
        // Only what `start` asked for: no extra probe on top.
        assert_eq!(requests_made(app, rx).await, (1, 1, 1, 0));
    }

    #[tokio::test]
    async fn first_connections_do_not_refetch() {
        let (mut app, rx) = app_without_controller();
        for stream in [Stream::Traffic, Stream::Logs, Stream::Connections] {
            connect(&mut app, stream);
        }
        assert_eq!(requests_made(app, rx).await, (0, 0, 0, 0));
    }

    #[tokio::test]
    async fn first_stream_back_after_an_outage_repeats_the_startup_requests() {
        let (mut app, rx) = app_without_controller();
        connect(&mut app, Stream::Traffic);
        disconnect(&mut app, Stream::Traffic);
        connect(&mut app, Stream::Logs);
        // The others following do not repeat it.
        connect(&mut app, Stream::Traffic);
        // One probe for the outage, then version, configs and proxies, as a
        // REST retry would ask for them.
        assert_eq!(requests_made(app, rx).await, (2, 1, 1, 0));
    }

    #[tokio::test]
    async fn an_outage_behind_the_banner_is_not_probed_again() {
        let (mut app, rx) = app_without_controller();
        app.handle_msg(Msg::Version(Err(ApiError::Timeout)));
        assert!(app.reconnect.is_some());
        connect(&mut app, Stream::Traffic);
        disconnect(&mut app, Stream::Traffic);
        // The banner already retries on its own schedule. Recovery still
        // refetches, which is what clears it.
        connect(&mut app, Stream::Traffic);
        assert_eq!(requests_made(app, rx).await, (1, 1, 1, 0));
    }

    #[tokio::test]
    async fn traffic_is_dropped_with_its_own_stream_only() {
        let (mut app, _rx) = app_without_controller();
        connect(&mut app, Stream::Traffic);
        connect(&mut app, Stream::Logs);
        let frame = Traffic { up: 1, down: 2 };
        stream_event(&mut app, Stream::Traffic, StreamEvent::Traffic(frame));
        assert_eq!(app.traffic, Some(frame));
        disconnect(&mut app, Stream::Logs);
        assert_eq!(app.traffic, Some(frame));
        disconnect(&mut app, Stream::Traffic);
        assert_eq!(app.traffic, None);
    }

    #[tokio::test]
    async fn memory_stream_starts_for_mihomo_only() {
        let version = |meta| Version {
            version: "v1".to_owned(),
            meta,
        };
        let (mut clash, _rx) = app_without_controller();
        clash.handle_msg(Msg::Version(Ok(version(false))));
        assert!(!clash.streams.started(Stream::Memory));

        let (mut mihomo, _rx) = app_without_controller();
        mihomo.handle_msg(Msg::Version(Ok(version(true))));
        assert!(mihomo.streams.started(Stream::Memory));
    }

    #[tokio::test]
    async fn stream_histories_keep_the_newest_minute_and_clear_with_their_stream() {
        let (mut app, _rx) = app_without_controller();
        for stream in [Stream::Traffic, Stream::Connections, Stream::Memory] {
            connect(&mut app, stream);
        }
        for n in 0..70 {
            let frame = Traffic { up: n, down: 0 };
            stream_event(&mut app, Stream::Traffic, StreamEvent::Traffic(frame));
            let open = ConnectionSnapshot {
                upload_total: 5,
                download_total: 6,
                connections: vec![Connection::default(); usize::try_from(n).unwrap()],
            };
            stream_event(
                &mut app,
                Stream::Connections,
                StreamEvent::Connections(open),
            );
            // The first frame after connecting (n = 0) reports 0 before
            // anything is sampled; it is not a reading.
            let memory = Memory { inuse: n * 1000 };
            stream_event(&mut app, Stream::Memory, StreamEvent::Memory(memory));
        }
        assert_eq!(app.traffic_history.len(), TRAFFIC_HISTORY);
        assert_eq!(app.traffic_history.front().unwrap().up, 10);
        assert_eq!(app.traffic_history.back().unwrap().up, 69);
        assert_eq!(app.connection_history.len(), TRAFFIC_HISTORY);
        assert_eq!(app.connection_history.front(), Some(&10));
        assert_eq!(app.connection_history.back(), Some(&69));
        assert_eq!(app.connections.unwrap().active, 69);
        assert_eq!(app.connections.unwrap().upload_total, 5);
        assert_eq!(app.memory_history.len(), TRAFFIC_HISTORY);
        assert_eq!(app.memory_history.front(), Some(&10_000));
        assert_eq!(app.memory, Some(69_000));

        // Another stream going down leaves the others alone.
        connect(&mut app, Stream::Logs);
        disconnect(&mut app, Stream::Logs);
        assert_eq!(app.traffic_history.len(), TRAFFIC_HISTORY);
        assert_eq!(app.connection_history.len(), TRAFFIC_HISTORY);
        assert_eq!(app.memory_history.len(), TRAFFIC_HISTORY);

        disconnect(&mut app, Stream::Traffic);
        assert!(app.traffic.is_none() && app.traffic_history.is_empty());
        assert!(app.connections.is_some() && app.memory.is_some());
        disconnect(&mut app, Stream::Memory);
        assert!(app.memory.is_none() && app.memory_history.is_empty());
        assert!(app.connections.is_some());
        // The table and what it is measured against live and die with the
        // stream; the user's pause outlasts it.
        app.connections_view.paused = true;
        assert!(app.connections_view.rows.is_some());
        assert!(!app.connections_view.baseline.is_empty());
        disconnect(&mut app, Stream::Connections);
        assert!(app.connections.is_none() && app.connection_history.is_empty());
        let view = &app.connections_view;
        assert!(view.rows.is_none() && view.baseline.is_empty() && view.fresh.is_empty());
        assert!(view.paused);
    }

    fn press(key: char, page: Page, typing: bool) -> Option<Action> {
        let event = Event::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
        action_for(&event, page, typing)
    }

    #[test]
    fn m_switches_mode_on_every_page_except_while_typing_a_filter() {
        for page in Page::ALL {
            assert_eq!(press('m', page, false), Some(Action::CycleMode), "{page:?}");
        }
        // Page keys, the mode key and page numbers all go into the box of
        // the page being filtered, with nothing else happening.
        for page in [Page::Proxies, Page::Connections] {
            let (mut app, _rx) = app_without_controller();
            app.page = page;
            app.update(press('/', page, app.typing()).unwrap());
            for key in ['p', 's', 'S', 'm', '5'] {
                let action = press(key, page, app.typing());
                assert_eq!(
                    action,
                    Some(Action::FilterInput(KeyEvent::new(
                        KeyCode::Char(key),
                        KeyModifiers::NONE
                    ))),
                    "{page:?} {key}"
                );
                app.update(action.unwrap());
            }
            assert_eq!(app.filter().unwrap().value(), "psSm5", "{page:?}");
            assert_eq!(app.page, page);
            assert!(!app.connections_view.paused);
            assert_eq!(app.connections_view.sort, ConnectionSort::default());
            assert_eq!(app.proxies_view.sort, Sort::Default);
            // `m` would have complained that there is no mode yet.
            assert!(app.toasts.is_empty());
        }
    }

    #[tokio::test]
    async fn m_sends_one_patch_until_the_refetch_answers() {
        let (mut app, rx) = app_without_controller();
        set_mode(&mut app, "rule");
        app.update(Action::CycleMode);
        // The patch is in flight.
        app.update(Action::CycleMode);
        app.update(Action::CycleMode);

        // Patched: the refetch (number 2) is in flight.
        app.handle_msg(Msg::ModeSet(Ok(())));
        app.update(Action::CycleMode);

        // An older fetch answering first says nothing about the new mode.
        configs_reply(&mut app, 1, "rule");
        app.update(Action::CycleMode);

        configs_reply(&mut app, 2, "global");
        assert!(app.is_global_mode());
        // Now free: the next press patches again.
        app.update(Action::CycleMode);
        assert_eq!(requests_made(app, rx).await, (0, 1, 0, 2));
    }

    #[tokio::test]
    async fn a_failed_patch_or_refetch_frees_the_key_and_keeps_the_mode() {
        let (mut app, rx) = app_without_controller();
        set_mode(&mut app, "rule");
        app.update(Action::CycleMode);
        app.handle_msg(Msg::ModeSet(Err(http(500))));
        assert_eq!(app.toasts.len(), 1);
        assert!(matches!(app.configs, Some(Ok(_))));

        app.update(Action::CycleMode);
        app.handle_msg(Msg::ModeSet(Ok(())));
        app.handle_msg(Msg::Configs {
            seq: 2,
            refetch: true,
            result: Err(http(500)),
        });
        // The old mode stays up rather than an error page over the overview.
        assert!(matches!(&app.configs, Some(Ok(configs)) if configs.mode == "rule"));
        app.update(Action::CycleMode);
        assert_eq!(requests_made(app, rx).await, (0, 1, 0, 3));
    }

    #[tokio::test]
    async fn a_late_reply_cannot_roll_the_mode_back() {
        let (mut app, _rx) = app_without_controller();
        // Fetch 1 was asked for at start-up and is slow; fetch 2 follows the switch.
        configs_reply(&mut app, 2, "global");
        configs_reply(&mut app, 1, "rule");
        assert!(app.is_global_mode());
    }

    #[tokio::test]
    async fn m_without_configs_only_complains() {
        for configs in [None, Some(Err(ApiError::Timeout))] {
            let (mut app, rx) = app_without_controller();
            app.configs = configs;
            app.update(Action::CycleMode);
            assert_eq!(app.toasts.back().unwrap().text, "尚未获取到当前模式");
            assert_eq!(requests_made(app, rx).await, (0, 0, 0, 0));
        }
    }

    #[tokio::test]
    async fn m_asks_the_kernel_for_the_next_mode_whatever_the_case() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

        for (current, next) in [
            ("rule", "global"),
            ("Global", "direct"),
            ("DIRECT", "rule"),
            // A mode tabby does not know goes back to rule.
            ("script", "rule"),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("PATCH"))
                .respond_with(ResponseTemplate::new(204))
                .mount(&server)
                .await;
            let (mut app, mut rx) = app_at(&server.uri());
            set_mode(&mut app, current);
            app.update(Action::CycleMode);
            assert!(matches!(rx.recv().await, Some(Msg::ModeSet(Ok(())))));

            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), 1, "{current}");
            assert_eq!(requests[0].url.path(), "/configs");
            let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
            assert_eq!(body, serde_json::json!({ "mode": next }), "{current}");
        }
    }

    #[test]
    fn mixed_case_global_still_leads_the_group_list() {
        let (mut app, _) = app_with_proxies();
        set_mode(&mut app, "Global");
        assert!(app.is_global_mode());
        assert_eq!(group_names(&app)[0], GLOBAL);
    }

    /// A connection that started `secs` past 10:00:00 UTC; `""` for a start
    /// the kernel could not give.
    fn flow(id: &str, secs: &str, upload: u64, download: u64) -> Connection {
        Connection {
            id: id.to_owned(),
            upload,
            download,
            start: secs
                .parse::<u32>()
                .ok()
                .map(|secs| format!("2026-11-05T10:00:{secs:02}Z").parse().unwrap()),
            ..Connection::default()
        }
    }

    fn feed(app: &mut App, connections: Vec<Connection>) {
        let frame = ConnectionSnapshot {
            connections,
            ..ConnectionSnapshot::default()
        };
        stream_event(app, Stream::Connections, StreamEvent::Connections(frame));
    }

    fn rows(app: &App) -> &[ConnectionRow] {
        app.connections_view.rows.as_deref().unwrap()
    }

    fn table_ids(app: &App) -> Vec<&str> {
        rows(app)
            .iter()
            .map(|row| row.connection.id.as_str())
            .collect()
    }

    fn row<'a>(app: &'a App, id: &str) -> &'a ConnectionRow {
        rows(app)
            .iter()
            .find(|row| row.connection.id == id)
            .unwrap()
    }

    fn is_fresh(app: &App, id: &str) -> bool {
        row(app, id).fresh
    }

    #[test]
    fn the_table_lists_the_newest_first_whatever_order_the_kernel_sends() {
        let (mut app, _rx) = app_without_controller();
        feed(
            &mut app,
            vec![
                flow("old", "01", 0, 0),
                flow("zz", "", 0, 0),
                flow("b", "05", 0, 0),
                flow("new", "09", 0, 0),
                flow("a", "05", 0, 0),
                flow("aa", "", 0, 0),
            ],
        );
        // Equal starts, and starts that could not be read, by id.
        assert_eq!(table_ids(&app), ["new", "a", "b", "old", "aa", "zz"]);
    }

    #[test]
    fn the_cursor_stays_on_its_connection_as_the_table_changes() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Connections;
        // Ids are single letters, started in alphabetical order.
        let pick = |ids: &str| -> Vec<Connection> {
            ids.chars()
                .map(|id| {
                    let secs = format!("{:02}", u32::from(id) - u32::from('a') + 1);
                    flow(&id.to_string(), &secs, 0, 0)
                })
                .collect()
        };
        feed(&mut app, pick("abc"));
        assert_eq!(table_ids(&app), ["c", "b", "a"]);
        app.update(Action::Cursor(Move::Down));
        assert_eq!(app.connections_view.cursor, 1);

        // Two new connections above push `b` down to the fourth row.
        feed(&mut app, pick("abcde"));
        assert_eq!(table_ids(&app), ["e", "d", "c", "b", "a"]);
        assert_eq!(app.connections_view.cursor, 3);

        // `b` ends with the cursor on it, which was past the new end.
        feed(&mut app, pick("acd"));
        assert_eq!(table_ids(&app), ["d", "c", "a"]);
        assert_eq!(app.connections_view.cursor, 2);

        // The cursor's own connection ends in the middle: its row stays.
        app.update(Action::Cursor(Move::Up));
        feed(&mut app, pick("ad"));
        assert_eq!(table_ids(&app), ["d", "a"]);
        assert_eq!(app.connections_view.cursor, 1);
    }

    #[test]
    fn a_vanished_connection_leaves_the_cursor_on_its_row() {
        let (mut app, _rx) = app_without_controller();
        let all: Vec<_> = ["a", "b", "c", "d"]
            .into_iter()
            .zip(["04", "03", "02", "01"])
            .map(|(id, secs)| flow(id, secs, 0, 0))
            .collect();
        feed(&mut app, all.clone());
        app.page = Page::Connections;
        app.update(Action::Cursor(Move::Down));
        app.update(Action::Cursor(Move::Down));
        assert_eq!(row_under_cursor(&app), "c");
        feed(
            &mut app,
            vec![all[0].clone(), all[1].clone(), all[3].clone()],
        );
        assert_eq!(app.connections_view.cursor, 2);
        assert_eq!(row_under_cursor(&app), "d");
    }

    fn row_under_cursor(app: &App) -> &str {
        &rows(app)[app.connections_view.cursor].connection.id
    }

    #[test]
    fn resuming_shows_one_frame_of_traffic_not_everything_moved_while_paused() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Connections;
        feed(&mut app, vec![flow("a", "01", 0, 1_000)]);
        feed(&mut app, vec![flow("a", "01", 0, 2_000)]);
        assert_eq!(row(&app, "a").rate, Some(Rate { up: 0, down: 1_000 }));

        app.update(Action::TogglePause);
        let as_of = app.connections_view.as_of;
        feed(&mut app, vec![flow("a", "01", 0, 5_000)]);
        feed(
            &mut app,
            vec![flow("a", "01", 0, 9_000), flow("late", "02", 0, 0)],
        );
        // The table is as it was, rate and clock included; the overview's
        // numbers are not held back.
        assert_eq!(table_ids(&app), ["a"]);
        assert_eq!(row(&app, "a").rate, Some(Rate { up: 0, down: 1_000 }));
        assert_eq!(row(&app, "a").connection.download, 2_000);
        assert_eq!(app.connections_view.as_of, as_of);
        assert_eq!(app.connections.unwrap().active, 2);

        app.update(Action::TogglePause);
        feed(
            &mut app,
            vec![flow("a", "01", 0, 9_500), flow("late", "02", 0, 70)],
        );
        assert_eq!(row(&app, "a").rate, Some(Rate { up: 0, down: 500 }));
        // It came up while paused: it has a baseline by now.
        assert_eq!(row(&app, "late").rate, Some(Rate { up: 0, down: 70 }));
    }

    #[test]
    fn a_table_paused_before_its_first_frame_still_gets_that_frame() {
        let (mut app, _rx) = app_without_controller();
        app.update(Action::TogglePause);
        feed(&mut app, vec![flow("a", "01", 0, 0)]);
        assert_eq!(table_ids(&app), ["a"]);
        feed(&mut app, vec![flow("a", "01", 0, 0), flow("b", "02", 0, 0)]);
        assert_eq!(table_ids(&app), ["a"]);
    }

    #[tokio::test]
    async fn new_connections_light_up_for_three_frames_except_in_the_first_frame() {
        let (mut app, _rx) = app_without_controller();
        let (a, b) = (flow("a", "01", 0, 0), flow("b", "02", 0, 0));
        connect(&mut app, Stream::Connections);
        feed(&mut app, vec![a.clone()]);
        assert!(!is_fresh(&app, "a"));

        feed(&mut app, vec![a.clone(), b.clone()]);
        assert!(is_fresh(&app, "b") && !is_fresh(&app, "a"));
        feed(&mut app, vec![a.clone(), b.clone()]);
        assert!(is_fresh(&app, "b"));
        feed(&mut app, vec![a.clone(), b.clone()]);
        assert!(is_fresh(&app, "b"));
        feed(&mut app, vec![a.clone(), b.clone()]);
        assert!(!is_fresh(&app, "b"));

        // After a reconnect the first frame is the whole table again.
        disconnect(&mut app, Stream::Connections);
        connect(&mut app, Stream::Connections);
        feed(&mut app, vec![a, b]);
        assert!(
            rows(&app)
                .iter()
                .all(|row| !row.fresh && row.rate.is_none())
        );
    }

    #[test]
    fn half_a_page_is_half_the_rows_that_fit_the_terminal() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Connections;
        let many: Vec<_> = (0..100)
            .map(|n| flow(&format!("{n:03}"), "01", 0, 0))
            .collect();
        feed(&mut app, many);
        // 20 rows: header, tabs, footer and the panel's borders and column
        // header leave 12 table rows.
        app.size = Size::new(100, 20);
        app.update(Action::Cursor(Move::HalfDown));
        assert_eq!(app.connections_view.cursor, 6);
        app.size = Size::new(100, 40);
        app.update(Action::Cursor(Move::HalfDown));
        assert_eq!(app.connections_view.cursor, 6 + 16);
    }

    /// A connection that started `secs` past 10:00:00 UTC, going to `host`
    /// from `process`.
    fn named(id: &str, secs: &str, host: &str, process: &str) -> Connection {
        let mut connection = flow(id, secs, 0, 0);
        connection.metadata.host = host.to_owned();
        connection.metadata.process = process.to_owned();
        connection
    }

    fn visible_ids(app: &App) -> Vec<&str> {
        app.connections_view
            .visible()
            .into_iter()
            .map(|row| row.connection.id.as_str())
            .collect()
    }

    fn selected_id(app: &App) -> &str {
        &app.connections_view
            .selected_connection()
            .unwrap()
            .connection
            .id
    }

    fn sort_by(app: &mut App, column: SortColumn, ascending: bool) {
        app.connections_view
            .set_sort(ConnectionSort { column, ascending });
    }

    fn key(ch: char) -> Action {
        Action::FilterInput(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
    }

    /// Types `text` into the current page's filter and accepts it.
    fn type_filter(app: &mut App, text: &str) {
        app.update(Action::StartFilter);
        for ch in text.chars() {
            app.update(key(ch));
        }
        app.update(Action::FilterAccept);
    }

    /// Two frames, so the connections of the first have a rate in the
    /// second; `n` only comes up in the second and has none.
    fn sortable_table() -> App {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Connections;
        let a = named("a", "03", "B.example", "curl");
        let b = named("b", "", "a10.example", "");
        let c = named("c", "05", "a9.example", "Wget");
        let n = named("n", "09", "", "aria2");
        feed(&mut app, vec![a.clone(), b.clone(), c.clone()]);
        let moved = |mut connection: Connection, upload, download| {
            connection.upload = upload;
            connection.download = download;
            connection
        };
        feed(
            &mut app,
            vec![
                moved(n, 5, 5),
                moved(a, 300, 10),
                moved(c, 200, 20),
                moved(b, 100, 30),
            ],
        );
        app
    }

    #[test]
    fn every_column_sorts_both_ways_with_missing_values_last() {
        let mut app = sortable_table();
        // Missing: `n` has no host (nor address) and no rate, `b` no
        // process and no start. Totals are never missing.
        for (column, ascending, descending) in [
            (SortColumn::Host, ["c", "b", "a", "n"], ["a", "b", "c", "n"]),
            (
                SortColumn::Process,
                ["n", "a", "c", "b"],
                ["c", "a", "n", "b"],
            ),
            (SortColumn::Up, ["b", "c", "a", "n"], ["a", "c", "b", "n"]),
            (SortColumn::Down, ["a", "c", "b", "n"], ["b", "c", "a", "n"]),
            (
                SortColumn::Total,
                ["n", "b", "c", "a"],
                ["a", "c", "b", "n"],
            ),
            // Ascending is the newest first.
            (
                SortColumn::Duration,
                ["n", "c", "a", "b"],
                ["a", "c", "n", "b"],
            ),
        ] {
            sort_by(&mut app, column, true);
            assert_eq!(table_ids(&app), ascending, "{column:?} ascending");
            sort_by(&mut app, column, false);
            assert_eq!(table_ids(&app), descending, "{column:?} descending");
        }
    }

    #[test]
    fn equal_values_go_by_id_in_either_direction() {
        let (mut app, _rx) = app_without_controller();
        let same = |id| named(id, "01", "same.example", "curl");
        let frame = vec![same("y"), same("x"), same("z")];
        feed(&mut app, frame.clone());
        feed(&mut app, frame);
        for column in SortColumn::ALL {
            for ascending in [true, false] {
                sort_by(&mut app, column, ascending);
                assert_eq!(table_ids(&app), ["x", "y", "z"], "{column:?} {ascending}");
            }
        }
    }

    #[test]
    fn s_moves_right_in_that_column_s_direction_and_capital_s_only_flips_it() {
        let mut app = sortable_table();
        let sort = |app: &App| {
            let sort = app.connections_view.sort;
            (sort.column, sort.ascending)
        };
        assert_eq!(sort(&app), (SortColumn::Duration, true));
        app.update(Action::ReverseSort);
        assert_eq!(sort(&app), (SortColumn::Duration, false));
        for expected in [
            (SortColumn::Host, true),
            (SortColumn::Process, true),
            (SortColumn::Up, false),
        ] {
            app.update(Action::CycleSort);
            assert_eq!(sort(&app), expected);
        }
        app.update(Action::ReverseSort);
        assert_eq!(sort(&app), (SortColumn::Up, true));
        // The next column starts over in its own direction.
        app.update(Action::CycleSort);
        assert_eq!(sort(&app), (SortColumn::Down, false));
        app.update(Action::CycleSort);
        assert_eq!(sort(&app), (SortColumn::Total, false));
        app.update(Action::CycleSort);
        assert_eq!(sort(&app), (SortColumn::Duration, true));
    }

    #[test]
    fn a_narrow_terminal_skips_the_columns_it_hides() {
        let mut app = sortable_table();
        app.size = Size::new(69, 24);
        let mut seen = vec![];
        for _ in 0..4 {
            app.update(Action::CycleSort);
            seen.push(app.connections_view.sort.column);
        }
        assert_eq!(
            seen,
            [
                SortColumn::Host,
                SortColumn::Up,
                SortColumn::Down,
                SortColumn::Duration
            ]
        );
        // Sorted by a column that went out of sight: on to the next shown.
        sort_by(&mut app, SortColumn::Process, true);
        app.update(Action::CycleSort);
        assert_eq!(app.connections_view.sort.column, SortColumn::Up);
    }

    #[test]
    fn the_cursor_keeps_its_connection_through_sorting_filtering_and_frames() {
        let mut app = sortable_table();
        // Newest first: n, c, a, b. `n` came in above the cursor's `c`.
        assert_eq!(table_ids(&app), ["n", "c", "a", "b"]);
        assert_eq!(selected_id(&app), "c");
        app.update(Action::CycleSort);
        assert_eq!(table_ids(&app), ["c", "b", "a", "n"]);
        assert_eq!(selected_id(&app), "c");

        // Rows above it filtered away: it moves up with its connection.
        app.update(Action::Cursor(Move::Down));
        assert_eq!(selected_id(&app), "b");
        app.update(Action::StartFilter);
        app.update(key('E'));
        assert_eq!(visible_ids(&app), ["c", "b", "a"]);
        assert_eq!(selected_id(&app), "b");
        for ch in "xAMPLE".chars() {
            app.update(key(ch));
        }
        app.update(Action::Dismiss);
        type_filter(&mut app, "A10");
        assert_eq!(visible_ids(&app), ["b"]);
        assert_eq!(app.connections_view.cursor, 0);
        app.update(Action::Dismiss);
        assert_eq!(visible_ids(&app), ["c", "b", "a", "n"]);
        assert_eq!(selected_id(&app), "b");

        // A new frame puts a connection ahead of it.
        let mut frame: Vec<Connection> = rows(&app)
            .iter()
            .map(|row| row.connection.clone())
            .collect();
        frame.push(named("d", "07", "a1.example", ""));
        feed(&mut app, frame);
        assert_eq!(table_ids(&app), ["d", "c", "b", "a", "n"]);
        assert_eq!(selected_id(&app), "b");
    }

    #[test]
    fn a_filtered_out_cursor_stays_at_its_position() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Connections;
        feed(
            &mut app,
            vec![
                named("a", "04", "keep.example", ""),
                named("b", "03", "keep.example", ""),
                named("c", "02", "drop.example", ""),
                named("d", "01", "keep.example", ""),
            ],
        );
        app.update(Action::Cursor(Move::Down));
        app.update(Action::Cursor(Move::Down));
        assert_eq!(selected_id(&app), "c");
        type_filter(&mut app, "keep");
        assert_eq!(visible_ids(&app), ["a", "b", "d"]);
        assert_eq!(selected_id(&app), "d");
        // Past the end of what is left: the last row.
        app.update(Action::Dismiss);
        app.update(Action::Cursor(Move::Bottom));
        type_filter(&mut app, "drop");
        assert_eq!(selected_id(&app), "c");
    }

    #[test]
    fn the_filter_matches_any_field_whatever_the_case() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Connections;
        let with = |id: &str, edit: fn(&mut Connection)| {
            let mut connection = flow(id, "01", 0, 0);
            edit(&mut connection);
            connection
        };
        feed(
            &mut app,
            vec![
                with("host", |c| c.metadata.host = "Www.Example.COM".to_owned()),
                with("ip", |c| {
                    c.metadata.destination_ip = "203.0.113.77".to_owned();
                }),
                with("process", |c| c.metadata.process = "Firefox".to_owned()),
                with("rule", |c| c.rule = "GeoSite".to_owned()),
                with("payload", |c| c.rule_payload = "Netflix".to_owned()),
                with("chain", |c| {
                    c.chains = vec!["香港 01".to_owned(), "Auto".to_owned()];
                }),
            ],
        );
        for (needle, id) in [
            ("example.com", "host"),
            ("113.77", "ip"),
            ("FIREFOX", "process"),
            ("geosite", "rule"),
            ("NetFlix", "payload"),
            ("香港", "chain"),
            ("auto", "chain"),
        ] {
            type_filter(&mut app, needle);
            assert_eq!(visible_ids(&app), [id], "{needle}");
            app.update(Action::Dismiss);
        }
    }

    #[test]
    fn the_filter_only_hides_rows() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Connections;
        feed(
            &mut app,
            vec![
                named("a", "02", "a.example", ""),
                named("b", "01", "b.example", ""),
            ],
        );
        type_filter(&mut app, "a.");
        let mut b = named("b", "01", "b.example", "");
        b.download = 700;
        feed(&mut app, vec![named("a", "02", "a.example", ""), b]);
        assert_eq!(visible_ids(&app), ["a"]);
        // The overview counts what is open, not what is shown.
        assert_eq!(app.connections.unwrap().active, 2);
        // The hidden row was measured all along.
        app.update(Action::Dismiss);
        assert_eq!(row(&app, "b").rate, Some(Rate { up: 0, down: 700 }));

        // Paused, the filter works on the frozen rows.
        app.update(Action::TogglePause);
        feed(&mut app, vec![named("c", "03", "c.example", "")]);
        type_filter(&mut app, "c.");
        assert_eq!(visible_ids(&app), Vec::<&str>::new());
        app.update(Action::Dismiss);
        type_filter(&mut app, "b.");
        assert_eq!(visible_ids(&app), ["b"]);
    }

    #[test]
    fn sorting_while_paused_reorders_the_frozen_rows_at_once() {
        let mut app = sortable_table();
        app.update(Action::TogglePause);
        app.update(Action::ReverseSort);
        assert_eq!(table_ids(&app), ["a", "c", "n", "b"]);
    }

    #[test]
    fn the_popup_shows_the_cursor_s_connection_and_follows_the_table() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Connections;
        feed(&mut app, vec![]);
        app.update(Action::Confirm);
        assert_eq!(app.overlay, None);

        let (a, mut b) = (flow("a", "02", 0, 0), flow("b", "01", 0, 0));
        feed(&mut app, vec![a.clone(), b.clone()]);
        app.update(Action::Cursor(Move::Down));
        app.update(Action::Confirm);
        assert_eq!(app.overlay, Some(Overlay::Connection));
        let detail = |app: &App| {
            let detail = app.connections_view.detail.as_ref().unwrap();
            (
                detail.row.connection.id.clone(),
                detail.row.connection.download,
                detail.row.rate,
                detail.closed,
            )
        };
        assert_eq!(detail(&app), ("b".to_owned(), 0, None, false));

        b.download = 500;
        feed(&mut app, vec![a.clone(), b.clone()]);
        let rate = Some(Rate { up: 0, down: 500 });
        assert_eq!(detail(&app), ("b".to_owned(), 500, rate, false));
        let as_of = app.connections_view.detail.as_ref().unwrap().as_of;

        // Paused: the popup holds still with the table.
        app.connections_view.paused = true;
        b.download = 900;
        feed(&mut app, vec![a.clone(), b]);
        assert_eq!(detail(&app), ("b".to_owned(), 500, rate, false));

        // Gone: what it last had stays, and its clock stops there.
        app.connections_view.paused = false;
        feed(&mut app, vec![a]);
        assert_eq!(detail(&app), ("b".to_owned(), 500, rate, true));
        assert_eq!(app.connections_view.detail.as_ref().unwrap().as_of, as_of);

        app.update(Action::Dismiss);
        assert_eq!(app.overlay, None);
        assert!(app.connections_view.detail.is_none());
    }

    #[tokio::test]
    async fn the_popup_ignores_other_keys_and_closes_with_the_stream() {
        let (mut app, rx) = app_without_controller();
        app.page = Page::Connections;
        connect(&mut app, Stream::Connections);
        feed(&mut app, vec![flow("a", "01", 0, 0)]);
        app.update(Action::Confirm);
        for action in [
            Action::ToggleHelp,
            Action::CycleSort,
            Action::StartFilter,
            Action::CloseConnection,
            Action::CloseConnections,
        ] {
            app.update(action);
        }
        assert_eq!(app.overlay, Some(Overlay::Connection));
        assert_eq!(app.connections_view.sort, ConnectionSort::default());
        assert!(!app.typing());
        assert!(app.pending.is_none());

        disconnect(&mut app, Stream::Connections);
        assert_eq!(app.overlay, None);
        assert!(app.connections_view.detail.is_none());
        assert_eq!(closes_sent(app, rx).await, Vec::<Sent>::new());
    }

    /// A close request the app sent.
    #[derive(Debug, PartialEq)]
    enum Sent {
        /// `DELETE /connections`.
        All,
        /// `DELETE /connections/{id}` for these, in table order.
        Each(Vec<String>),
    }

    fn each(ids: &[&str]) -> Sent {
        Sent::Each(ids.iter().map(|id| (*id).to_owned()).collect())
    }

    /// The closes the app sent, from the replies they leave on the channel.
    /// Dropping the app first lets the channel close once they are
    /// answered.
    async fn closes_sent(app: App, mut rx: mpsc::Receiver<Msg>) -> Vec<Sent> {
        drop(app);
        let mut sent = vec![];
        while let Some(msg) = rx.recv().await {
            match msg {
                Msg::AllConnectionsClosed { .. } => sent.push(Sent::All),
                Msg::ConnectionsClosed { results, .. } => {
                    sent.push(Sent::Each(results.into_iter().map(|(id, _)| id).collect()));
                }
                _ => {}
            }
        }
        sent
    }

    /// Newest first: `a`, `b`, `c`, on the connections page.
    fn three_connections() -> (App, mpsc::Receiver<Msg>) {
        let (mut app, rx) = app_without_controller();
        app.page = Page::Connections;
        connect(&mut app, Stream::Connections);
        feed(
            &mut app,
            vec![
                named("c", "01", "c.mirror.example", ""),
                named("a", "03", "a.mirror.example", ""),
                named("b", "02", "b.other.example", ""),
            ],
        );
        (app, rx)
    }

    fn dialog_body(app: &App) -> String {
        assert_eq!(app.overlay, Some(Overlay::Confirm));
        app.pending.as_ref().unwrap().body()
    }

    #[tokio::test]
    async fn the_dialog_goes_ahead_on_y_only_and_ignores_other_keys() {
        let (mut app, rx) = three_connections();
        for cancel in [Action::Cancel, Action::Dismiss] {
            app.update(press('X', Page::Connections, false).unwrap());
            assert_eq!(dialog_body(&app), "关闭全部 3 条连接？");
            app.update(cancel);
            assert_eq!(app.overlay, None);
            assert!(app.pending.is_none());
        }

        app.update(Action::CloseConnections);
        for key in ['?', 'x', 'X', '5', 'm', 'p', 's', '/', 'Y'] {
            if let Some(action) = press(key, Page::Connections, false) {
                app.update(action);
            }
        }
        app.update(Action::Confirm);
        assert_eq!(dialog_body(&app), "关闭全部 3 条连接？");
        assert_eq!(app.page, Page::Connections);
        assert!(!app.connections_view.paused && !app.typing());
        assert_eq!(app.connections_view.sort, ConnectionSort::default());
        // `m` would have complained that there is no mode yet.
        assert!(app.toasts.is_empty());

        app.update(press('y', Page::Connections, false).unwrap());
        assert_eq!(app.overlay, None);
        assert!(app.pending.is_none());
        assert_eq!(closes_sent(app, rx).await, [Sent::All]);
    }

    #[tokio::test]
    async fn capital_x_under_a_filter_closes_only_the_rows_shown() {
        let (mut app, rx) = three_connections();
        type_filter(&mut app, "MIRROR");
        app.update(Action::CloseConnections);
        assert_eq!(dialog_body(&app), "关闭匹配「MIRROR」的 2 条连接？");
        app.update(Action::Accept);

        // Nothing shown, nothing asked.
        type_filter(&mut app, "nomatch");
        app.update(Action::CloseConnections);
        assert_eq!(app.overlay, None);
        app.update(Action::FilterClear);
        feed(&mut app, vec![]);
        app.update(Action::CloseConnections);
        assert_eq!(app.overlay, None);

        assert_eq!(closes_sent(app, rx).await, [each(&["a", "c"])]);
    }

    #[tokio::test]
    async fn y_closes_only_what_the_table_still_has() {
        let (mut app, rx) = three_connections();
        type_filter(&mut app, "mirror");
        app.update(Action::CloseConnections);
        // `c` ends while the dialog is up.
        feed(&mut app, vec![flow("a", "03", 0, 0), flow("b", "02", 0, 0)]);
        app.update(Action::Accept);

        // All of them end: `y` closes nothing.
        feed(&mut app, vec![named("a", "03", "a.mirror.example", "")]);
        app.update(Action::CloseConnections);
        feed(&mut app, vec![]);
        app.update(Action::Accept);
        assert_eq!(app.overlay, None);

        assert_eq!(closes_sent(app, rx).await, [each(&["a"])]);
    }

    #[tokio::test]
    async fn the_dialog_closes_with_the_stream_and_y_then_sends_nothing() {
        let (mut app, rx) = three_connections();
        app.update(Action::CloseConnections);
        disconnect(&mut app, Stream::Connections);
        assert_eq!(app.overlay, None);
        assert!(app.pending.is_none());
        app.update(Action::Accept);
        assert_eq!(closes_sent(app, rx).await, Vec::<Sent>::new());
    }

    #[tokio::test]
    async fn x_closes_the_cursor_s_row_and_takes_it_out_at_once_even_paused() {
        let (mut app, rx) = three_connections();
        app.update(Action::Cursor(Move::Down));
        app.update(press('x', Page::Connections, false).unwrap());
        assert_eq!(app.overlay, None);

        app.handle_msg(Msg::ConnectionsClosed {
            results: vec![("b".to_owned(), Ok(()))],
            report: false,
        });
        assert_eq!(table_ids(&app), ["a", "c"]);
        // Its position, now `c`'s.
        assert_eq!(selected_id(&app), "c");

        // Paused, and the frame that dropped `c` held back: it still goes at
        // once. It had ended by itself (404): not an error either.
        app.connections_view.paused = true;
        feed(&mut app, vec![flow("a", "03", 0, 0)]);
        app.handle_msg(Msg::ConnectionsClosed {
            results: vec![("c".to_owned(), Err(ApiError::NotFound))],
            report: false,
        });
        assert_eq!(table_ids(&app), ["a"]);
        assert_eq!(selected_id(&app), "a");
        assert!(app.toasts.is_empty());

        assert_eq!(closes_sent(app, rx).await, [each(&["b"])]);
    }

    #[test]
    fn closing_one_by_one_reports_one_summary_of_the_failures() {
        let (mut app, _rx) = app_without_controller();
        feed(
            &mut app,
            ["a", "b", "c", "d", "e"]
                .map(|id| flow(id, "01", 0, 0))
                .to_vec(),
        );
        app.handle_msg(Msg::ConnectionsClosed {
            results: vec![
                ("a".to_owned(), Ok(())),
                ("b".to_owned(), Err(http(500))),
                ("c".to_owned(), Err(ApiError::Timeout)),
                ("d".to_owned(), Err(ApiError::NotFound)),
            ],
            report: true,
        });
        assert_eq!(table_ids(&app), ["b", "c", "e"]);
        let toasts: Vec<_> = app
            .toasts
            .iter()
            .map(|toast| (toast.kind, toast.text.as_str()))
            .collect();
        assert_eq!(
            toasts,
            [
                (ToastKind::Success, "已关闭 2 条连接"),
                (ToastKind::Error, "2 条关闭失败：控制器返回错误（HTTP 500）"),
            ]
        );
    }
}
