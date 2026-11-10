use std::{
    cell::Cell,
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures_util::future::join_all;
use jiff::{Timestamp, Zoned, civil};
use ratatui::layout::{Position, Rect, Size};
use tokio::sync::mpsc;
use tui_input::{Input, backend::crossterm::EventHandler};

use crate::{
    api::{
        backoff,
        client::ApiClient,
        error::ApiError,
        models::{
            self, Baseline, Configs, ConnectionSnapshot, ConnectionStats, CoreKind, Delay, GLOBAL,
            Log, LogKind, LogLevel, Mode, Proxies, Proxy, ProxyProvider, Rate, Rule, RuleProvider,
            Traffic, Version, next_kernel_log_level,
        },
        stream::{Event as StreamEvent, Stream, Streams},
    },
    config::{self, Origin, Profiles, Settings},
    core::{ManagedCore, Release, SystemProxy},
    format::natural_cmp,
    themes::{self, Entry, Picker},
    ui::{
        components::help,
        connections,
        hit::{Hits, Target},
        icons::Icons,
        logs::{self, Fit},
        providers, proxies, regions, rules,
        scroll::Scroll,
        settings,
        text::{describe, reason},
        theme::Theme,
    },
};

pub mod dns;
mod managed;
mod subscriptions;

pub use dns::DnsView;
pub use managed::{CoreEvent, CorePhase, LoginStart};
pub use subscriptions::{Ask, Fetching, SubEvent, SubscriptionsView, Transfer};

/// The eight top-level pages, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Overview,
    Proxies,
    Providers,
    Rules,
    Connections,
    Logs,
    Settings,
    /// The managed core's subscriptions; an external controller has none.
    Subscriptions,
}

impl Page {
    pub const ALL: [Self; 8] = [
        Self::Overview,
        Self::Proxies,
        Self::Providers,
        Self::Rules,
        Self::Connections,
        Self::Logs,
        Self::Settings,
        Self::Subscriptions,
    ];

    /// The page bound to number key `1`–`8`.
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

/// The two panels of the proxies page. On a narrow page the group list is
/// the picker row above the grid.
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
    /// Index into [`App::visible_groups`] of the row the picker's
    /// drop-down has its cursor on; becomes `group` on `Enter`.
    pub picker: usize,
    pub picker_scroll: Scroll,
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

    /// The order a click on the heading of `column` asks for: that column
    /// in its first direction, or, when the table already runs by it, the
    /// other way round.
    fn toward(self, column: SortColumn) -> Self {
        if self.column == column {
            self.reversed()
        } else {
            Self {
                column,
                ascending: column.ascending_first(),
            }
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

/// One row of the rules table.
#[derive(Debug, Clone)]
pub struct RuleRow {
    /// Where the kernel lists it, from 1. Filtering does not renumber.
    pub number: usize,
    /// The type as configurations spell it: `DOMAIN-SUFFIX`.
    pub kind: String,
    pub payload: String,
    pub proxy: String,
    /// `kind`, `payload` and `proxy` folded once, for the filter.
    folded: [String; 3],
}

impl RuleRow {
    fn new(number: usize, rule: Rule) -> Self {
        let kind = rules::type_name(&rule.kind);
        Self {
            number,
            folded: [&kind, &rule.payload, &rule.proxy].map(|text| rules::fold(text)),
            kind,
            payload: rule.payload,
            proxy: rule.proxy,
        }
    }

    /// Whether `needle` (folded) is in the type, the payload or the target.
    fn matches(&self, needle: &str) -> bool {
        self.folded.iter().any(|field| field.contains(needle))
    }
}

/// List, cursor and filter of the rules page.
#[derive(Debug, Default)]
pub struct RulesView {
    /// The rules, in the kernel's order; `None` until the first load is in.
    pub rows: Option<Result<Vec<RuleRow>, ApiError>>,
    /// Index into [`RulesView::visible`].
    pub cursor: usize,
    pub scroll: Scroll,
    /// Case-insensitive substring the type, payload or target must contain.
    pub filter: Input,
    /// The filter box has the keyboard.
    pub typing: bool,
}

impl RulesView {
    /// The rules that pass the filter, in the kernel's order.
    pub fn visible(&self) -> Vec<&RuleRow> {
        let Some(Ok(rows)) = &self.rows else {
            return vec![];
        };
        let needle = rules::fold(self.filter.value());
        rows.iter()
            .filter(|row| needle.is_empty() || row.matches(&needle))
            .collect()
    }

    /// The number and position of the rule under the cursor. Rules have no
    /// id; the number is the closest thing.
    fn anchor(&self) -> Option<(usize, usize)> {
        let visible = self.visible();
        let index = self.cursor.min(visible.len().checked_sub(1)?);
        Some((visible[index].number, index))
    }

    /// Puts the cursor back on the rule numbered as in `anchor`; when no
    /// rule shown has that number, at the same position, within the list.
    fn restore(&mut self, anchor: Option<(usize, usize)>) {
        let visible = self.visible();
        self.cursor = anchor
            .map_or(0, |(number, index)| {
                visible
                    .iter()
                    .position(|row| row.number == number)
                    .unwrap_or(index)
            })
            .min(visible.len().saturating_sub(1));
    }

    /// Takes in a `/rules` reply. A failed refetch keeps the rules on
    /// screen; anything else that failed shows as the error page.
    fn apply(&mut self, refetch: bool, result: Result<Vec<Rule>, ApiError>) {
        match result {
            Ok(rules) => {
                let anchor = self.anchor();
                let rows = rules
                    .into_iter()
                    .enumerate()
                    .map(|(index, rule)| RuleRow::new(index + 1, rule))
                    .collect();
                self.rows = Some(Ok(rows));
                self.restore(anchor);
            }
            Err(_) if refetch && matches!(self.rows, Some(Ok(_))) => {}
            Err(err) => self.rows = Some(Err(err)),
        }
    }

    /// Changes the filter; the cursor goes to the first rule that passes.
    fn edit_filter(&mut self, edit: impl FnOnce(&mut Input)) {
        edit(&mut self.filter);
        self.cursor = 0;
    }

    /// Drops the filter and gives the keyboard back. The cursor stays on its
    /// rule, now among the others.
    fn clear_filter(&mut self) {
        let anchor = self.anchor();
        self.filter.reset();
        self.typing = false;
        self.restore(anchor);
    }
}

/// Which list a provider is in, which also picks the endpoints for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Proxy,
    Rule,
}

/// A provider by list and name: what an update or a check is sent for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderRef {
    pub kind: ProviderKind,
    pub name: String,
}

/// What is being done to a provider while its row spins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    Updating,
    Checking,
}

/// One row of the providers page.
#[derive(Debug, Clone, Copy)]
pub enum ProviderRow<'a> {
    Proxy(&'a ProxyProvider),
    Rule(&'a RuleProvider),
}

impl ProviderRow<'_> {
    pub fn name(&self) -> &str {
        match self {
            Self::Proxy(provider) => &provider.name,
            Self::Rule(provider) => &provider.name,
        }
    }

    pub fn target(&self) -> ProviderRef {
        ProviderRef {
            kind: match self {
                Self::Proxy(_) => ProviderKind::Proxy,
                Self::Rule(_) => ProviderKind::Rule,
            },
            name: self.name().to_owned(),
        }
    }
}

/// One list of the providers page; `None` until its first load is in.
pub type ProviderSlot<T> = Option<Result<Vec<T>, ApiError>>;

/// The two lists, cursor, filter and work in progress of the providers
/// page. The cursor runs through the proxy providers, then the rule
/// providers, as one list.
#[derive(Debug, Default)]
pub struct ProvidersView {
    pub proxies: ProviderSlot<ProxyProvider>,
    /// Stays `None` under original Clash, which has no rule providers.
    pub rules: ProviderSlot<RuleProvider>,
    /// Index into [`ProvidersView::rows`].
    pub cursor: usize,
    pub scroll: Scroll,
    /// Case-insensitive substring the names must contain.
    pub filter: Input,
    /// The filter box has the keyboard.
    pub typing: bool,
    /// Providers with an update or a health check in flight.
    pub busy: HashMap<ProviderRef, Work>,
}

impl ProvidersView {
    /// Neither list is in yet: the page is one spinner.
    pub fn loading(&self) -> bool {
        self.proxies.is_none() && self.rules.is_none()
    }

    fn needle(&self) -> String {
        self.filter.value().to_lowercase()
    }

    /// The proxy providers whose names pass the filter.
    pub fn visible_proxies(&self) -> Vec<&ProxyProvider> {
        let needle = self.needle();
        let Some(Ok(providers)) = &self.proxies else {
            return vec![];
        };
        providers
            .iter()
            .filter(|provider| provider.name.to_lowercase().contains(&needle))
            .collect()
    }

    /// The rule providers whose names pass the filter.
    pub fn visible_rules(&self) -> Vec<&RuleProvider> {
        let needle = self.needle();
        let Some(Ok(providers)) = &self.rules else {
            return vec![];
        };
        providers
            .iter()
            .filter(|provider| provider.name.to_lowercase().contains(&needle))
            .collect()
    }

    /// What the cursor runs through: the visible proxy providers, then the
    /// visible rule providers.
    pub fn rows(&self) -> Vec<ProviderRow<'_>> {
        let proxies = self.visible_proxies().into_iter().map(ProviderRow::Proxy);
        let rules = self.visible_rules().into_iter().map(ProviderRow::Rule);
        proxies.chain(rules).collect()
    }

    /// The row under the cursor.
    pub fn selected(&self) -> Option<ProviderRow<'_>> {
        let rows = self.rows();
        rows.get(self.cursor.min(rows.len().checked_sub(1)?))
            .copied()
    }

    /// The provider under the cursor and its position.
    fn anchor(&self) -> Option<(ProviderRef, usize)> {
        let rows = self.rows();
        let index = self.cursor.min(rows.len().checked_sub(1)?);
        Some((rows[index].target(), index))
    }

    /// Puts the cursor back on the provider in `anchor`; when it is not
    /// shown any more, at the same position, within the list.
    fn restore(&mut self, anchor: Option<(ProviderRef, usize)>) {
        let rows = self.rows();
        self.cursor = anchor
            .map_or(0, |(target, index)| {
                rows.iter()
                    .position(|row| row.target() == target)
                    .unwrap_or(index)
            })
            .min(rows.len().saturating_sub(1));
    }

    /// Takes in a reply for the list `slot` picks. A failed refetch keeps
    /// the list on screen; anything else that failed shows as an error.
    /// The cursor stays on its provider while the list was on screen
    /// before. A list coming in (again) keeps the cursor's position
    /// instead: the rule providers answering before the proxy providers
    /// must not drag the cursor down with them when those fill in above.
    fn apply<T>(
        &mut self,
        slot: fn(&mut Self) -> &mut ProviderSlot<T>,
        refetch: bool,
        result: Result<Vec<T>, ApiError>,
    ) {
        let shown = matches!(slot(self), Some(Ok(_)));
        let anchor = self.anchor();
        let list = slot(self);
        match result {
            Ok(providers) => *list = Some(Ok(providers)),
            Err(_) if refetch && matches!(list, Some(Ok(_))) => {}
            Err(err) => *list = Some(Err(err)),
        }
        if shown {
            self.restore(anchor);
        } else {
            self.cursor = self.cursor.min(self.rows().len().saturating_sub(1));
        }
    }

    /// Changes the filter; the cursor goes to the first provider that
    /// passes.
    fn edit_filter(&mut self, edit: impl FnOnce(&mut Input)) {
        edit(&mut self.filter);
        self.cursor = 0;
    }

    /// Drops the filter and gives the keyboard back. The cursor stays on
    /// its provider, now among the others.
    fn clear_filter(&mut self) {
        let anchor = self.anchor();
        self.filter.reset();
        self.typing = false;
        self.restore(anchor);
    }
}

/// Log lines kept: on screen, and in the queue a pause fills, alike.
pub const LOG_CAPACITY: usize = 2000;

/// One log line as received.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Local time it came in; the kernel's line carries none.
    pub time: civil::Time,
    pub kind: LogKind,
    /// The kernel's text, tabs turned into spaces and other control
    /// characters but `\n` dropped: they would garble the screen.
    pub message: String,
    /// `message` lowercased once, for the filter.
    lowercase: String,
    /// The rows `message` last took and the width that was at: the
    /// scrollbar adds up every shown line on each frame. `(0, 0)` until
    /// first laid out.
    rows: Cell<(usize, usize)>,
}

impl LogEntry {
    pub fn new(log: Log, time: civil::Time) -> Self {
        let message: String = log
            .payload
            .chars()
            .filter_map(|ch| match ch {
                '\t' => Some(' '),
                '\n' => Some(ch),
                _ if ch.is_control() => None,
                _ => Some(ch),
            })
            .collect();
        Self {
            time,
            kind: log.kind,
            lowercase: message.to_lowercase(),
            message,
            rows: Cell::new((0, 0)),
        }
    }

    /// Rows the message takes in a message column `width` wide; wrapped
    /// once per width.
    fn rows(&self, width: usize) -> usize {
        match self.rows.get() {
            (at, rows) if at == width && rows > 0 => rows,
            _ => {
                let rows = logs::wrap_message(&self.message, width).len();
                self.rows.set((width, rows));
                rows
            }
        }
    }
}

/// A place in the log: row `row` of the wrapped message of entry `entry`
/// (an index into [`LogsView::entries`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogSpot {
    pub entry: usize,
    pub row: usize,
}

/// Where the log stream stands, as the logs page tells it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogLink {
    /// Not connected: not yet, or dropped.
    #[default]
    Down,
    /// Resubscribing at another level: the old connection is gone and the
    /// new one is not in yet. Usually a few dozen milliseconds; for as long
    /// as the controller is unreachable otherwise.
    Resubscribing,
    Live,
}

/// Buffer, level, pause, filter and scroll state of the logs page.
///
/// Scrolling counts screen rows, so a long message wrapped over several
/// rows is read one row at a time. Positions are laid out on demand from
/// the spot they start at, never for the whole buffer.
#[derive(Debug, Default)]
pub struct LogsView {
    /// What is shown, oldest first; at most [`LOG_CAPACITY`].
    pub entries: VecDeque<LogEntry>,
    /// What came in while paused, oldest first; appended on resume. At most
    /// [`LOG_CAPACITY`].
    pending: VecDeque<LogEntry>,
    /// Lines that came in while paused but were pushed out of `pending`.
    missed: usize,
    /// What the stream is subscribed at.
    pub level: LogLevel,
    /// What is shown stays as it is; new lines queue up.
    pub paused: bool,
    /// The stream's state.
    pub link: LogLink,
    /// Case-insensitive substring the message must contain.
    pub filter: Input,
    /// The filter box has the keyboard.
    pub typing: bool,
    /// The top row of the view while it stays where the user scrolled to;
    /// `None` while it follows the newest line.
    pub top: Option<LogSpot>,
}

impl LogsView {
    /// Takes in a line: onto the screen, or into the queue while paused.
    fn receive(&mut self, entry: LogEntry) {
        if self.paused {
            if self.pending.len() == LOG_CAPACITY {
                self.pending.pop_front();
                self.missed += 1;
            }
            self.pending.push_back(entry);
        } else {
            self.append(entry);
        }
    }

    /// Adds a line to what is shown, dropping the oldest when full. A view
    /// that stays put moves its spot along, so it keeps the same lines.
    fn append(&mut self, entry: LogEntry) {
        if self.entries.len() == LOG_CAPACITY {
            self.entries.pop_front();
            if let Some(top) = &mut self.top {
                *top = match top.entry.checked_sub(1) {
                    Some(entry) => LogSpot { entry, ..*top },
                    // Its line is gone: the oldest one left takes its place.
                    None => LogSpot { entry: 0, row: 0 },
                };
            }
        }
        self.entries.push_back(entry);
    }

    /// `p`. Resuming shows what queued up, in order.
    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        if !self.paused {
            for entry in std::mem::take(&mut self.pending) {
                self.append(entry);
            }
            self.missed = 0;
        }
    }

    /// `c`: drops what is shown and what is queued; the view follows again.
    fn clear(&mut self) {
        self.entries.clear();
        self.pending.clear();
        self.missed = 0;
        self.top = None;
    }

    /// Lines that came in since pausing, those the queue had to let go of
    /// included.
    pub fn arrived(&self) -> usize {
        self.pending.len() + self.missed
    }

    /// Nothing to show yet and no stream to bring any.
    pub fn waiting(&self) -> bool {
        self.link == LogLink::Down && self.entries.is_empty() && self.pending.is_empty()
    }

    fn needle(&self) -> String {
        self.filter.value().to_lowercase()
    }

    /// The entries from index `from` on that pass the filter `needle`.
    fn shown_from<'a>(
        &'a self,
        from: usize,
        needle: &str,
    ) -> impl Iterator<Item = (usize, &'a LogEntry)> {
        let from = from.min(self.entries.len());
        self.entries
            .range(from..)
            .enumerate()
            .map(move |(offset, entry)| (from + offset, entry))
            .filter(move |(_, entry)| entry.lowercase.contains(needle))
    }

    /// The entries before index `end` that pass the filter `needle`, newest
    /// first.
    fn shown_before<'a>(
        &'a self,
        end: usize,
        needle: &str,
    ) -> impl Iterator<Item = (usize, &'a LogEntry)> {
        self.entries
            .range(..end.min(self.entries.len()))
            .enumerate()
            .rev()
            .filter(move |(_, entry)| entry.lowercase.contains(needle))
    }

    /// How many entries pass the filter.
    pub fn shown_count(&self) -> usize {
        let needle = self.needle();
        self.shown_from(0, &needle).count()
    }

    /// `spot` on an entry that is shown: itself, its row kept within the
    /// message, or else the next shown entry from its top. `None` when no
    /// entry from there on is shown.
    fn shown_spot(&self, spot: LogSpot, needle: &str, width: usize) -> Option<LogSpot> {
        let (entry, shown) = self.shown_from(spot.entry, needle).next()?;
        let row = if entry == spot.entry {
            spot.row.min(shown.rows(width) - 1)
        } else {
            0
        };
        Some(LogSpot { entry, row })
    }

    /// Rows from `spot` (a shown one) to the end, counted up to `limit`.
    fn rows_from(&self, spot: LogSpot, needle: &str, width: usize, limit: usize) -> usize {
        let mut rows = 0;
        for (index, entry) in self.shown_from(spot.entry, needle) {
            rows += entry.rows(width);
            if index == spot.entry {
                rows -= spot.row;
            }
            if rows >= limit {
                break;
            }
        }
        rows
    }

    /// The top row of a view that follows: the one `fit.height` rows above
    /// the end, or the very first when everything fits. `None` when nothing
    /// is shown.
    fn bottom_top(&self, needle: &str, fit: Fit) -> Option<LogSpot> {
        let mut rows = 0;
        let mut top = None;
        for (entry, shown) in self.shown_before(self.entries.len(), needle) {
            rows += shown.rows(fit.width);
            top = Some(LogSpot {
                entry,
                row: rows.saturating_sub(fit.height),
            });
            if rows >= fit.height {
                break;
            }
        }
        top
    }

    /// The spot `rows` rows above `spot`, or the very first row.
    fn up(&self, mut spot: LogSpot, mut rows: usize, needle: &str, width: usize) -> LogSpot {
        while rows > 0 {
            if spot.row > 0 {
                let step = rows.min(spot.row);
                spot.row -= step;
                rows -= step;
                continue;
            }
            let Some((entry, shown)) = self.shown_before(spot.entry, needle).next() else {
                break;
            };
            spot = LogSpot {
                entry,
                row: shown.rows(width) - 1,
            };
            rows -= 1;
        }
        spot
    }

    /// The spot `rows` rows below `spot`, or the very last row.
    fn down(&self, mut spot: LogSpot, mut rows: usize, needle: &str, width: usize) -> LogSpot {
        while rows > 0 {
            let last = self
                .entries
                .get(spot.entry)
                .map_or(0, |entry| entry.rows(width) - 1);
            if spot.row < last {
                let step = rows.min(last - spot.row);
                spot.row += step;
                rows -= step;
                continue;
            }
            let Some((entry, _)) = self.shown_from(spot.entry + 1, needle).next() else {
                break;
            };
            spot = LogSpot { entry, row: 0 };
            rows -= 1;
        }
        spot
    }

    /// Where the top of a view that stays put is, when it does and still
    /// has more below than fits; `None` means drawing from the bottom.
    fn resting_top(&self, needle: &str, fit: Fit) -> Option<LogSpot> {
        let top = self.shown_spot(self.top?, needle, fit.width)?;
        (self.rows_from(top, needle, fit.width, fit.height + 1) > fit.height).then_some(top)
    }

    /// Follows the newest line again once the view reached the bottom: by
    /// scrolling there, or because the filter or the size left too little
    /// below it.
    fn settle(&mut self, fit: Fit) {
        let needle = self.needle();
        self.top = self.resting_top(&needle, fit);
    }

    /// Scrolls by screen rows. Leaving the bottom stops following; reaching
    /// it, or `G`, follows again.
    fn scroll(&mut self, movement: Move, fit: Fit) {
        let needle = self.needle();
        let Some((first, _)) = self.shown_from(0, &needle).next() else {
            return;
        };
        let first = LogSpot {
            entry: first,
            row: 0,
        };
        let current = self
            .resting_top(&needle, fit)
            .or_else(|| self.bottom_top(&needle, fit))
            .unwrap_or(first);
        let half = (fit.height / 2).max(1);
        let top = match movement {
            Move::Up => self.up(current, 1, &needle, fit.width),
            Move::Down => self.down(current, 1, &needle, fit.width),
            Move::HalfUp => self.up(current, half, &needle, fit.width),
            Move::HalfDown => self.down(current, half, &needle, fit.width),
            Move::Top => first,
            Move::Bottom => {
                self.top = None;
                return;
            }
            Move::Left | Move::Right => return,
        };
        self.top = Some(top);
        self.settle(fit);
    }

    /// What a view of `fit` shows, top to bottom: for each screen row the
    /// entry, which row of its message it is, and that row's text. Only the
    /// entries in view are laid out; one that follows lays out from the
    /// bottom up.
    pub fn page(&self, fit: Fit) -> Vec<(&LogEntry, usize, String)> {
        let needle = self.needle();
        let mut page = Vec::with_capacity(fit.height);
        if let Some(top) = self.resting_top(&needle, fit) {
            for (index, entry) in self.shown_from(top.entry, &needle) {
                let skip = if index == top.entry { top.row } else { 0 };
                let rows = logs::wrap_message(&entry.message, fit.width);
                for (row, text) in rows.into_iter().enumerate().skip(skip) {
                    if page.len() == fit.height {
                        return page;
                    }
                    page.push((entry, row, text));
                }
            }
            return page;
        }
        let mut above = vec![];
        let mut rows = 0;
        for (_, entry) in self.shown_before(self.entries.len(), &needle) {
            let wrapped = logs::wrap_message(&entry.message, fit.width);
            rows += wrapped.len();
            above.push((entry, wrapped));
            if rows >= fit.height {
                break;
            }
        }
        let skip = rows.saturating_sub(fit.height);
        page.extend(
            above
                .into_iter()
                .rev()
                .flat_map(|(entry, wrapped)| {
                    wrapped
                        .into_iter()
                        .enumerate()
                        .map(move |(row, text)| (entry, row, text))
                })
                .skip(skip),
        );
        page
    }

    /// Where a view of `fit` is, in screen rows, for the scrollbar: the
    /// rows above its top, and the rows of every shown line. A view that
    /// follows sits at the bottom.
    pub fn extent(&self, fit: Fit) -> (usize, usize) {
        let needle = self.needle();
        let top = self.resting_top(&needle, fit);
        let (mut above, mut rows) = (0, 0);
        for (index, entry) in self.shown_from(0, &needle) {
            if let Some(top) = top
                && index == top.entry
            {
                above = rows + top.row;
            }
            rows += entry.rows(fit.width);
        }
        if top.is_none() {
            above = rows.saturating_sub(fit.height);
        }
        (above, rows)
    }
}

/// A user intent, derived from a terminal event.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// connection's details, toggles or runs a setting (`Space` too).
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
    /// `p`: freeze or resume the connections table or the log.
    TogglePause,
    /// `l`: subscribe the log at the next level.
    CycleLevel,
    /// `c`: empty the log.
    ClearLogs,
    /// `u`: update the provider under the cursor.
    UpdateProvider,
    /// `U`: ask to update the providers the page shows.
    UpdateProviders,
    /// `a` on the subscriptions page: type an address or a file to import.
    AddSubscription,
    /// `u` on the subscriptions page: read the subscription under the
    /// cursor again.
    UpdateSubscription,
    /// `U` on the subscriptions page: ask to update every subscription it
    /// shows.
    UpdateSubscriptions,
    /// `d` on the subscriptions page: ask to delete the subscription under
    /// the cursor.
    DeleteSubscription,
    /// `t` on the subscriptions page: turn the automatic update of the
    /// subscription under the cursor on or off.
    ToggleAutoUpdate,
    /// `e` on the subscriptions page: type another name for the
    /// subscription under the cursor.
    RenameSubscription,
    /// `i` on the subscriptions page: type the hours between the automatic
    /// updates of the subscription under the cursor.
    EditInterval,
    /// `K` on the subscriptions page: type another age key for the
    /// encrypted subscription under the cursor.
    ChangeAgeKey,
    /// `h` on the providers page: health-check the proxy provider under the
    /// cursor.
    HealthCheck,
    /// `/`: start typing the current page's filter.
    StartFilter,
    /// A key typed into the input box that has the keyboard: a filter, or
    /// the port being edited on the settings page.
    FilterInput(KeyEvent),
    /// `Enter` in the input box: keep the filter and stop typing, or send
    /// the port.
    FilterAccept,
    /// `Esc` in the input box: drop the filter, or the port being typed.
    FilterClear,
    ToggleHelp,
    /// `P`: open the profile popup, or close it.
    Profiles,
    /// `C`: open the theme picker, or close it.
    Themes,
    /// `Esc`: closes the topmost overlay, or clears an applied filter.
    Dismiss,
    /// A left click on something the last frame drew.
    Pick(Target),
    /// Two quick left clicks on the same row: picks it, then does what
    /// `Enter` does there.
    Activate(Target),
    /// A click on a column heading of the connections table: sorts by it,
    /// or the other way round when it already does.
    SortBy(SortColumn),
    /// A click on a mode of the overview: asks the kernel for it, as `m`
    /// does for the next one.
    SetMode(Mode),
    /// A turn of the wheel over something that scrolls: one step of
    /// `Up` or `Down`.
    Wheel(Target, Move),
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
    /// Reply to a change a setting (or `m`) asked of the kernel.
    Changed {
        change: Change,
        result: Result<(), ApiError>,
    },
    /// Reply to a load of the proxies page: `/proxies` and, for the nodes it
    /// leaves out, `/providers/proxies`, fetched together. `refetch` marks
    /// one made while data is already on screen, whose failure must not take
    /// that data away.
    Proxies {
        seq: u64,
        refetch: bool,
        result: Result<Proxies, ApiError>,
        providers: Result<Vec<ProxyProvider>, ApiError>,
    },
    /// Reply to a `/rules` fetch. `refetch` marks one made while rules are
    /// already on screen, whose failure must not take them away.
    Rules {
        seq: u64,
        refetch: bool,
        result: Result<Vec<Rule>, ApiError>,
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
    /// Something about the providers page.
    Providers(ProviderMsg),
    /// A WebSocket stream connected, dropped, or delivered a frame.
    Stream {
        stream: Stream,
        event: StreamEvent,
    },
    /// Something about the managed kernel.
    Core(CoreEvent),
    /// Something about the subscriptions of the managed kernel.
    Subscription(SubEvent),
}

/// A [`Msg`] stamped with the connection it was sent for.
#[derive(Debug)]
pub struct Envelope {
    /// The [`Outbox::epoch`] of the sender.
    pub epoch: u64,
    pub msg: Msg,
}

/// Where the background work for one connection reports: the app's
/// channel, every message stamped with the epoch of that connection. A
/// profile switch moves the app to the next epoch, and [`App::receive`]
/// drops whatever the work for an earlier one still sends.
#[derive(Debug, Clone)]
pub struct Outbox {
    epoch: u64,
    tx: mpsc::Sender<Envelope>,
}

impl Outbox {
    pub fn new(epoch: u64, tx: mpsc::Sender<Envelope>) -> Self {
        Self { epoch, tx }
    }

    /// The outbox of the connection that replaces this one.
    fn next(&self) -> Self {
        Self::new(self.epoch + 1, self.tx.clone())
    }

    /// Delivers `msg`. `false` when the app has hung up.
    pub async fn send(&self, msg: Msg) -> bool {
        let envelope = Envelope {
            epoch: self.epoch,
            msg,
        };
        self.tx.send(envelope).await.is_ok()
    }

    /// Like [`Outbox::send`], without waiting: `msg` is dropped when the
    /// channel is full. For hints that the next message makes stale.
    pub fn try_send(&self, msg: Msg) {
        let envelope = Envelope {
            epoch: self.epoch,
            msg,
        };
        let _ = self.tx.try_send(envelope);
    }
}

/// A reply to a request of the providers page.
#[derive(Debug)]
pub enum ProviderMsg {
    /// Reply to a `/providers/proxies` fetch. `refetch` marks one made
    /// while the list is on screen, whose failure must not take it away.
    Proxies {
        seq: u64,
        refetch: bool,
        result: Result<Vec<ProxyProvider>, ApiError>,
    },
    /// Reply to a `/providers/rules` fetch, as for [`ProviderMsg::Proxies`].
    Rules {
        seq: u64,
        refetch: bool,
        result: Result<Vec<RuleProvider>, ApiError>,
    },
    /// Reply to `u` or `h`: one provider, updated or checked.
    Done {
        target: ProviderRef,
        work: Work,
        result: Result<(), ApiError>,
    },
    /// Replies to `U`, all sent at once, back together.
    Updated {
        results: Vec<(ProviderRef, Result<(), ApiError>)>,
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
            // The pages with a sortable list.
            let sorted = matches!(page, Page::Proxies | Page::Connections);
            let filtered = matches!(
                page,
                Page::Proxies
                    | Page::Providers
                    | Page::Rules
                    | Page::Connections
                    | Page::Logs
                    | Page::Subscriptions
            );
            let pausable = matches!(page, Page::Connections | Page::Logs);
            match key.code {
                KeyCode::Char('c') if ctrl => Some(Action::Quit),
                KeyCode::Char('d') if ctrl => Some(Action::Cursor(Move::HalfDown)),
                KeyCode::Char('u') if ctrl => Some(Action::Cursor(Move::HalfUp)),
                KeyCode::Tab => Some(Action::NextPage),
                KeyCode::BackTab => Some(Action::PrevPage),
                KeyCode::Esc => Some(Action::Dismiss),
                KeyCode::Enter => Some(Action::Confirm),
                KeyCode::Char(' ') if page == Page::Settings => Some(Action::Confirm),
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
                KeyCode::Char('s') if sorted => Some(Action::CycleSort),
                KeyCode::Char('S') if page == Page::Connections => Some(Action::ReverseSort),
                KeyCode::Char('/') if filtered => Some(Action::StartFilter),
                KeyCode::Char('p') if pausable => Some(Action::TogglePause),
                KeyCode::Char('l') if page == Page::Logs => Some(Action::CycleLevel),
                KeyCode::Char('c') if page == Page::Logs => Some(Action::ClearLogs),
                KeyCode::Char('x') if page == Page::Connections => Some(Action::CloseConnection),
                KeyCode::Char('X') if page == Page::Connections => Some(Action::CloseConnections),
                KeyCode::Char('u') if page == Page::Providers => Some(Action::UpdateProvider),
                KeyCode::Char('U') if page == Page::Providers => Some(Action::UpdateProviders),
                KeyCode::Char('h') if page == Page::Providers => Some(Action::HealthCheck),
                KeyCode::Char('a') if page == Page::Subscriptions => Some(Action::AddSubscription),
                KeyCode::Char('u') if page == Page::Subscriptions => {
                    Some(Action::UpdateSubscription)
                }
                KeyCode::Char('U') if page == Page::Subscriptions => {
                    Some(Action::UpdateSubscriptions)
                }
                KeyCode::Char('d') if page == Page::Subscriptions => {
                    Some(Action::DeleteSubscription)
                }
                KeyCode::Char('t') if page == Page::Subscriptions => Some(Action::ToggleAutoUpdate),
                KeyCode::Char('e') if page == Page::Subscriptions => {
                    Some(Action::RenameSubscription)
                }
                KeyCode::Char('i') if page == Page::Subscriptions => Some(Action::EditInterval),
                KeyCode::Char('K') if page == Page::Subscriptions => Some(Action::ChangeAgeKey),
                // Only lowercase: the dialog says `y`, and a dangerous action
                // should not go ahead on a Shift slip.
                KeyCode::Char('y') => Some(Action::Accept),
                KeyCode::Char('n') => Some(Action::Cancel),
                KeyCode::Char('m') => Some(Action::CycleMode),
                KeyCode::Char('P') => Some(Action::Profiles),
                KeyCode::Char('C') => Some(Action::Themes),
                KeyCode::Char(c) => Page::from_digit(c).map(Action::SelectPage),
                _ => None,
            }
        }
        Event::Resize(width, height) => Some(Action::Resize(Size::new(*width, *height))),
        _ => None,
    }
}

/// Two presses on the same row within this long are a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

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
    /// The connections in [`App::profiles`] to switch between.
    Profiles,
    /// The themes in [`App::themes`] to pick from.
    Themes,
    /// The drop-down of the narrow proxies page's group picker.
    Groups,
    /// The DNS keys of a managed core's settings, kept in
    /// [`App::dns_view`].
    Dns,
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
    /// `U`: the providers the page showed, under `filter` when it is not
    /// empty.
    UpdateProviders {
        filter: String,
        targets: Vec<ProviderRef>,
    },
    /// `U` on the subscriptions page: the subscriptions it showed, under
    /// `filter` when it is not empty.
    UpdateSubscriptions {
        filter: String,
        ids: Vec<String>,
    },
    /// `d` on the subscriptions page.
    DeleteSubscription {
        id: String,
        name: String,
    },
    /// A setting that changes routing, reloads, downloads or replaces the
    /// kernel.
    Change(Change),
    /// `停止内核` on the settings page.
    StopCore,
    /// `重载配置` on a managed core: the config is made again from the
    /// subscription in use, then read.
    RegenerateConfig,
    /// `开启 TUN` on a managed core whose kernel has no capabilities yet.
    AuthorizeTun,
    /// The kernel to download, on first start (`upgrade` false) or as an
    /// upgrade of the installed one.
    InstallCore {
        release: Release,
        upgrade: bool,
    },
}

impl Confirm {
    pub fn title(&self) -> &'static str {
        match self {
            Self::CloseConnections(_) => "关闭连接",
            Self::UpdateProviders { .. } => "更新 Provider",
            Self::UpdateSubscriptions { .. } => "更新订阅",
            Self::DeleteSubscription { .. } => "删除订阅",
            Self::StopCore => "停止内核",
            Self::RegenerateConfig => "重载配置",
            Self::AuthorizeTun => "开启 TUN",
            Self::InstallCore { upgrade: true, .. } => "升级内核",
            Self::InstallCore { .. } => "下载内核",
            Self::Change(change) => change.title(),
        }
    }

    pub fn body(&self) -> String {
        match self {
            Self::CloseConnections(Closing::All { count }) => format!("关闭全部 {count} 条连接？"),
            Self::CloseConnections(Closing::Matching { filter, ids }) => {
                format!("关闭匹配「{filter}」的 {} 条连接？", ids.len())
            }
            Self::UpdateProviders { filter, targets } if filter.is_empty() => {
                format!("更新全部 {} 个 provider？", targets.len())
            }
            Self::UpdateProviders { filter, targets } => {
                format!("更新匹配「{filter}」的 {} 个 provider？", targets.len())
            }
            Self::StopCore => "停止托管内核？经由它的连接都会断开。".to_owned(),
            Self::RegenerateConfig => "用当前订阅重新生成配置？".to_owned(),
            Self::AuthorizeTun => {
                "开启 TUN 需要授权：tabby 会用 setcap 给内核加上 CAP_NET_ADMIN 和 CAP_NET_BIND_SERVICE。"
                    .to_owned()
            }
            Self::UpdateSubscriptions { filter, ids } if filter.is_empty() => {
                format!("更新全部 {} 个订阅？", ids.len())
            }
            Self::UpdateSubscriptions { filter, ids } => {
                format!("更新匹配「{filter}」的 {} 个订阅？", ids.len())
            }
            Self::DeleteSubscription { name, .. } => {
                format!("删除订阅「{name}」及其文件？")
            }
            Self::InstallCore {
                release,
                upgrade: true,
            } => format!("升级内核到 {}？内核会重启。", release.tag),
            Self::InstallCore { release, .. } => format!("下载 mihomo {}？", release.tag),
            Self::Change(change) => change.question().to_owned(),
        }
    }

    /// What `y` does, for the dialog and the key bar.
    pub fn verb(&self) -> &'static str {
        match self {
            Self::CloseConnections(_) => "确认",
            Self::UpdateProviders { .. } | Self::UpdateSubscriptions { .. } => "更新",
            Self::StopCore => "停止",
            Self::RegenerateConfig => "重载",
            Self::AuthorizeTun => "授权",
            Self::DeleteSubscription { .. } => "删除",
            Self::InstallCore { upgrade: true, .. } => "升级",
            Self::InstallCore { .. } => "下载",
            Self::Change(change) => change.verb(),
        }
    }
}

/// What the box on the settings page is typed into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Port(Port),
    /// The proxy subscriptions are downloaded through.
    Proxy,
}

impl Field {
    /// The row it is typed on.
    pub fn setting(self) -> Setting {
        match self {
            Self::Port(port) => Setting::Port(port),
            Self::Proxy => Setting::SubscriptionDownload,
        }
    }
}

/// A port the kernel takes proxy clients on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Port {
    Http,
    Socks,
    Mixed,
    Redir,
    Tproxy,
}

impl Port {
    /// Its key in `/configs`, read and patched alike.
    fn key(self) -> &'static str {
        match self {
            Self::Http => "port",
            Self::Socks => "socks-port",
            Self::Mixed => "mixed-port",
            Self::Redir => "redir-port",
            Self::Tproxy => "tproxy-port",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Http => "HTTP",
            Self::Socks => "SOCKS",
            Self::Mixed => "Mixed",
            Self::Redir => "Redir",
            Self::Tproxy => "TProxy",
        }
    }

    /// The port number `configs` reports, 0 when off; `None` when the
    /// kernel has no such port.
    pub fn of(self, configs: &Configs) -> Option<u16> {
        match self {
            Self::Http => configs.port,
            Self::Socks => configs.socks_port,
            Self::Mixed => configs.mixed_port,
            Self::Redir => configs.redir_port,
            Self::Tproxy => configs.tproxy_port,
        }
    }
}

/// The rows of the settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Setting {
    Mode,
    AllowLan,
    /// The kernel's own `log-level`, not the level the log page subscribes
    /// at.
    LogLevel,
    Ipv6,
    Port(Port),
    Tun,
    Reload,
    FlushFakeIp,
    UpdateGeo,
    Restart,
    Upgrade,
    /// The managed core's kernel, as a line of information.
    CoreBinary,
    /// Upgrades the managed core's kernel.
    UpgradeCore,
    /// Starts the managed core when the user logs in.
    LoginStart,
    /// Points the desktop's proxy at the managed core.
    SystemProxy,
    /// Edits the DNS keys the managed core sets over the subscription's.
    Dns,
    /// The way subscriptions are downloaded.
    SubscriptionDownload,
    /// Stops the managed core.
    StopCore,
}

impl Setting {
    /// Top to bottom, as the page lists them.
    pub const ALL: [Self; 22] = [
        Self::Mode,
        Self::AllowLan,
        Self::LogLevel,
        Self::Ipv6,
        Self::Port(Port::Http),
        Self::Port(Port::Socks),
        Self::Port(Port::Mixed),
        Self::Port(Port::Redir),
        Self::Port(Port::Tproxy),
        Self::Tun,
        Self::Reload,
        Self::FlushFakeIp,
        Self::UpdateGeo,
        Self::Restart,
        Self::Upgrade,
        Self::CoreBinary,
        Self::UpgradeCore,
        Self::LoginStart,
        Self::SystemProxy,
        Self::Dns,
        Self::SubscriptionDownload,
        Self::StopCore,
    ];

    /// The group heading it is listed under.
    pub fn section(self) -> &'static str {
        match self {
            Self::Mode | Self::AllowLan | Self::LogLevel | Self::Ipv6 => "基础",
            Self::Port(_) => "端口",
            Self::Tun => "TUN",
            Self::Reload | Self::FlushFakeIp | Self::UpdateGeo | Self::Restart | Self::Upgrade => {
                "维护"
            }
            Self::CoreBinary
            | Self::UpgradeCore
            | Self::LoginStart
            | Self::SystemProxy
            | Self::Dns
            | Self::SubscriptionDownload
            | Self::StopCore => "托管内核",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Mode => "代理模式",
            Self::AllowLan => "局域网连接",
            Self::LogLevel => "内核日志级别",
            Self::Ipv6 => "IPv6",
            Self::Port(port) => port.label(),
            Self::Tun => "启用",
            Self::Reload => "重载配置",
            Self::FlushFakeIp => "刷新 FakeIP",
            Self::UpdateGeo => "更新 GeoData",
            Self::Restart => "重启内核",
            Self::StopCore => "停止内核",
            Self::LoginStart => "登录时启动",
            Self::SystemProxy => "系统代理",
            Self::Dns => "DNS 覆盖",
            Self::SubscriptionDownload => "订阅下载",
            Self::CoreBinary => "内核",
            Self::Upgrade | Self::UpgradeCore => "升级内核",
        }
    }

    /// It asks before going ahead: it changes the system's routes, throws
    /// away what was changed at runtime, downloads, or takes the kernel
    /// down for a moment.
    pub fn asks(self) -> bool {
        matches!(
            self,
            Self::Tun
                | Self::Reload
                | Self::UpdateGeo
                | Self::Restart
                | Self::Upgrade
                | Self::StopCore
        )
    }

    /// Only mihomo has it; original Clash has neither the field nor the
    /// endpoint.
    fn mihomo_only(self) -> bool {
        matches!(
            self,
            Self::Tun | Self::FlushFakeIp | Self::UpdateGeo | Self::Restart | Self::Upgrade
        )
    }

    /// Only a managed core has it.
    fn managed_only(self) -> bool {
        matches!(
            self,
            Self::CoreBinary
                | Self::UpgradeCore
                | Self::LoginStart
                | Self::SystemProxy
                | Self::Dns
                | Self::SubscriptionDownload
                | Self::StopCore
        )
    }

    /// Only an external controller has it: tabby upgrades a managed core
    /// itself, not through the kernel's own `/upgrade`.
    fn external_only(self) -> bool {
        matches!(self, Self::Upgrade)
    }
}

/// Whether a setting can be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Open,
    /// The kernel did not report the value, though it is not known to be
    /// original Clash.
    Missing,
    /// Original Clash, which does not have it.
    NeedsMihomo,
    /// The kernel is the user's own binary (`[core].binary`, or one on
    /// `$PATH`): tabby does not replace it.
    SelfManaged,
    /// A managed core listens on its Mixed port alone.
    MixedOnly,
    /// The login start is a unit of the systemd user manager.
    NeedsSystemd,
    /// The desktop has no system proxy tabby can set.
    NoDesktopProxy,
}

/// A change to the running configuration, sent as `PATCH /configs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Patch {
    Mode(Mode),
    AllowLan(bool),
    LogLevel(&'static str),
    Ipv6(bool),
    Port(Port, u16),
    Tun(bool),
}

impl Patch {
    fn setting(self) -> Setting {
        match self {
            Self::Mode(_) => Setting::Mode,
            Self::AllowLan(_) => Setting::AllowLan,
            Self::LogLevel(_) => Setting::LogLevel,
            Self::Ipv6(_) => Setting::Ipv6,
            Self::Port(port, _) => Setting::Port(port),
            Self::Tun(_) => Setting::Tun,
        }
    }

    /// The request body: only the one key that changes.
    fn body(self) -> serde_json::Value {
        use serde_json::json;
        match self {
            Self::Mode(mode) => json!({ "mode": mode.name() }),
            Self::AllowLan(on) => json!({ "allow-lan": on }),
            Self::LogLevel(level) => json!({ "log-level": level }),
            Self::Ipv6(on) => json!({ "ipv6": on }),
            Self::Port(port, number) => json!({ (port.key()): number }),
            Self::Tun(on) => json!({ "tun": { "enable": on } }),
        }
    }
}

/// What a settings row, or `m`, asks the kernel to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Patch(Patch),
    Reload,
    FlushFakeIp,
    UpdateGeo,
    Restart,
    Upgrade,
}

impl Change {
    pub fn setting(self) -> Setting {
        match self {
            Self::Patch(patch) => patch.setting(),
            Self::Reload => Setting::Reload,
            Self::FlushFakeIp => Setting::FlushFakeIp,
            Self::UpdateGeo => Setting::UpdateGeo,
            Self::Restart => Setting::Restart,
            Self::Upgrade => Setting::Upgrade,
        }
    }

    async fn send(self, client: &ApiClient) -> Result<(), ApiError> {
        match self {
            Self::Patch(patch) => client.patch_configs(&patch.body()).await,
            Self::Reload => client.reload_config().await,
            Self::FlushFakeIp => client.flush_fake_ip().await,
            Self::UpdateGeo => client.update_geo().await,
            Self::Restart => client.restart().await,
            Self::Upgrade => client.upgrade().await,
        }
    }

    /// The success toast; none for the mode, which the header and the
    /// overview already show.
    fn done(self) -> Option<String> {
        let state = |on: bool| if on { "已开启" } else { "已关闭" };
        Some(match self {
            Self::Patch(Patch::Mode(_)) => return None,
            Self::Patch(Patch::AllowLan(on)) => format!("局域网连接{}", state(on)),
            Self::Patch(Patch::LogLevel(level)) => format!("内核日志级别已设为 {level}"),
            Self::Patch(Patch::Ipv6(on)) => format!("IPv6 {}", state(on)),
            Self::Patch(Patch::Port(port, 0)) => format!("{} 端口已关闭", port.label()),
            Self::Patch(Patch::Port(port, number)) => {
                format!("{} 端口已改为 {number}", port.label())
            }
            Self::Patch(Patch::Tun(on)) => format!("TUN {}", state(on)),
            Self::Reload => "已重载配置".to_owned(),
            Self::FlushFakeIp => "已刷新 FakeIP".to_owned(),
            Self::UpdateGeo => "GeoData 已更新".to_owned(),
            Self::Restart => "内核正在重启".to_owned(),
            Self::Upgrade => "已升级，内核正在重启".to_owned(),
        })
    }

    /// What the failure toast starts with.
    fn failed(self) -> &'static str {
        match self {
            Self::Patch(Patch::Mode(_)) => "切换模式失败",
            Self::Patch(_) => "修改失败",
            Self::Reload => "重载配置失败",
            Self::FlushFakeIp => "刷新 FakeIP 失败",
            Self::UpdateGeo => "更新 GeoData 失败",
            Self::Restart => "重启失败",
            Self::Upgrade => "升级失败",
        }
    }

    /// The confirm dialog's title, for the ones that ask.
    fn title(self) -> &'static str {
        match self {
            Self::Patch(Patch::Tun(true)) => "开启 TUN",
            Self::Patch(Patch::Tun(false)) => "关闭 TUN",
            Self::Patch(_) => "修改设置",
            Self::Reload => "重载配置",
            Self::FlushFakeIp => "刷新 FakeIP",
            Self::UpdateGeo => "更新 GeoData",
            Self::Restart => "重启内核",
            Self::Upgrade => "升级内核",
        }
    }

    fn question(self) -> &'static str {
        match self {
            Self::Patch(Patch::Tun(true)) => "开启 TUN？内核会接管本机流量并修改系统路由。",
            Self::Patch(Patch::Tun(false)) => "关闭 TUN？经 TUN 进入内核的流量将改走本机网络。",
            Self::Patch(_) => "修改这项设置？",
            Self::Reload => "重新加载配置文件？运行中改过的设置会被文件里的值覆盖。",
            Self::FlushFakeIp => "清空 FakeIP 缓存？",
            Self::UpdateGeo => "下载 GeoIP / GeoSite 数据库？需要联网，可能要等一会儿。",
            Self::Restart => "重启内核？现有连接会全部断开，控制器短暂不可用。",
            Self::Upgrade => "升级内核？会下载并替换内核程序，完成后内核自动重启。",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Patch(Patch::Tun(true)) => "开启",
            Self::Patch(Patch::Tun(false)) => "关闭",
            Self::Patch(_) | Self::FlushFakeIp => "确认",
            Self::Reload => "重载",
            Self::UpdateGeo => "更新",
            Self::Restart => "重启",
            Self::Upgrade => "升级",
        }
    }
}

/// Progress of a change. Another press before it ends would race two
/// requests, or patch from a value the kernel no longer has, so it waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// The request is in flight.
    Sending,
    /// Done; waiting for the `/configs` fetch with this number (or a later
    /// one), which says what the kernel now runs.
    Refetching(u64),
}

/// Reads a typed port: `Ok(None)` when nothing was typed, which drops the
/// edit, else the number, or why it is not a port. 0 turns the port off.
fn parse_port(text: &str) -> Result<Option<u16>, &'static str> {
    if text.is_empty() {
        return Ok(None);
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("端口只能是数字");
    }
    let digits = text.trim_start_matches('0');
    // Five digits at most, then the range: no overflow whatever is typed.
    if digits.len() > 5 {
        return Err("端口须在 0–65535 之间");
    }
    let number: u32 = digits.parse().unwrap_or(0);
    u16::try_from(number)
        .map(Some)
        .map_err(|_| "端口须在 0–65535 之间")
}

/// Cursor and the box being typed into on the settings page.
#[derive(Debug, Default)]
pub struct SettingsView {
    /// Index into [`Setting::ALL`].
    pub cursor: usize,
    pub scroll: Scroll,
    /// What is being typed and into what, while the box is open. It has
    /// the keyboard meanwhile, as a filter box does.
    pub editing: Option<(Field, Input)>,
}

impl SettingsView {
    /// A key typed into the box: a port takes digits and the keys that
    /// edit or move, an address anything.
    fn type_key(&mut self, key: KeyEvent) {
        let Some((field, input)) = &mut self.editing else {
            return;
        };
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if let KeyCode::Char(ch) = key.code
            && plain
            && matches!(field, Field::Port(_))
            && !ch.is_ascii_digit()
        {
            return;
        }
        input.handle_event(&Event::Key(key));
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
    /// The managed kernel's process is gone: waiting cannot help, `r`
    /// starts it again.
    pub exited: bool,
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
    pub icons: Icons,
    pub page: Page,
    client: ApiClient,
    /// Where background work reports, stamped with the current connection.
    outbox: Outbox,
    /// Where the managed kernel is; `None` on an external controller.
    pub core_phase: Option<CorePhase>,
    /// Whether the managed kernel's file has the capabilities TUN needs, as
    /// last looked; `None` until it was.
    pub tun_authorized: Option<bool>,
    /// Whether the managed kernel starts at login, as last looked; `None`
    /// until it was.
    pub login_start: Option<LoginStart>,
    /// Whether the system proxy points at the managed kernel, as last
    /// looked; `None` until it was.
    pub system_proxy: Option<SystemProxy>,
    /// The managed core, while `sudo` has to be run on the terminal, which
    /// only the main loop can give.
    suspend: Option<std::sync::Arc<ManagedCore>>,
    /// The connections `P` offers, and which one is in use.
    pub profiles: Profiles,
    /// Index into [`Profiles::choices`] of the row the profile popup's
    /// cursor is on.
    pub profile_cursor: usize,
    pub profile_scroll: Scroll,
    /// The theme picker `C` opens.
    pub themes: Picker,
    pub version: Option<Result<Version, ApiError>>,
    pub configs: Option<Result<Configs, ApiError>>,
    pub proxies: Option<Result<Proxies, ApiError>>,
    pub proxies_view: ProxiesView,
    /// `/configs` fetches issued so far; numbers them.
    config_fetches: u64,
    /// The newest `/configs` fetch whose reply has been applied.
    applied_config: u64,
    /// Settings with a change in progress (the mode's included, whichever
    /// key sent it); a second press waits for it to end.
    changing: HashMap<Setting, Stage>,
    /// The settings page.
    pub settings_view: SettingsView,
    /// The subscriptions page.
    pub subscriptions_view: SubscriptionsView,
    /// The popup that edits the managed core's DNS overrides.
    pub dns_view: DnsView,
    /// Nodes with a delay test in flight.
    pub testing: HashSet<String>,
    /// `/proxies` fetches issued so far; numbers them.
    fetches: u64,
    /// The newest fetch whose reply has been applied; older ones are dropped.
    applied_fetch: u64,
    /// Delay results measured here, with the value of `fetches` when each was
    /// recorded. A reply replaces one only if its fetch came after it.
    local_delays: HashMap<String, u64>,
    /// The rules page, loaded the first time it is shown.
    pub rules_view: RulesView,
    /// `/rules` fetches issued so far; numbers them.
    rule_fetches: u64,
    /// The newest `/rules` fetch whose reply has been applied.
    applied_rule_fetch: u64,
    /// The providers page, loaded the first time it is shown.
    pub providers_view: ProvidersView,
    /// `/providers/proxies` fetches issued so far; numbers them.
    proxy_provider_fetches: u64,
    /// The newest `/providers/proxies` fetch whose reply has been applied.
    applied_proxy_provider_fetch: u64,
    /// `/providers/rules` fetches issued so far; numbers them.
    rule_provider_fetches: u64,
    /// The newest `/providers/rules` fetch whose reply has been applied.
    applied_rule_provider_fetch: u64,
    /// A `/providers/rules` fetch asked for before the kernel was known to
    /// be mihomo, with its `refetch` flag; sent once `/version` says so.
    deferred_rule_providers: Option<bool>,
    pub toasts: VecDeque<Toast>,
    pub overlay: Option<Overlay>,
    /// What the confirm dialog asks about, while it is open.
    pub pending: Option<Confirm>,
    /// The first row of the help the overlay shows, when it does not fit.
    pub help_scroll: usize,
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
    /// The log page, from `/logs`. Unlike the connections table, what was
    /// received stays when the stream goes down: it is history.
    pub logs_view: LogsView,
    /// `Some` from the first failed `/version` probe until one succeeds.
    pub reconnect: Option<Reconnect>,
    /// Set by `r` until the next `/version` reply, which then reports success.
    refreshing: bool,
    should_quit: bool,
    /// The terminal size, from the start and every resize event.
    pub size: Size,
    /// What the last frame drew that the mouse can act on.
    pub hits: Hits,
    /// The last left press that landed on something, and when.
    last_click: Option<(Target, Instant)>,
    dirty: bool,
}

impl App {
    /// An app drawn with `theme` (`[theme_overrides]` of `settings` not
    /// applied yet).
    pub fn new(
        settings: Settings,
        theme: Theme,
        client: ApiClient,
        msg_tx: mpsc::Sender<Envelope>,
        size: Size,
        profiles: Profiles,
        themes: Picker,
    ) -> Self {
        let theme = theme.with(&settings.theme_overrides);
        let outbox = Outbox::new(0, msg_tx);
        Self::connect(settings, theme, client, outbox, size, profiles, themes)
    }

    /// An app on the controller of `settings`, nothing loaded yet, its
    /// background work reporting to `outbox`.
    fn connect(
        settings: Settings,
        theme: Theme,
        client: ApiClient,
        outbox: Outbox,
        size: Size,
        profiles: Profiles,
        themes: Picker,
    ) -> Self {
        let streams = Streams::new(&settings.url, settings.secret.as_deref(), outbox.clone());
        let core_phase = profiles
            .current_choice()
            .filter(|choice| choice.origin == Origin::Managed)
            .map(|_| CorePhase::Idle);
        Self {
            theme,
            icons: settings.icons.into(),
            settings,
            page: Page::Overview,
            client,
            outbox,
            core_phase,
            tun_authorized: None,
            login_start: None,
            system_proxy: None,
            suspend: None,
            profile_cursor: profiles.current,
            profiles,
            profile_scroll: Scroll::default(),
            themes,
            version: None,
            configs: None,
            proxies: None,
            config_fetches: 0,
            applied_config: 0,
            changing: HashMap::new(),
            settings_view: SettingsView::default(),
            subscriptions_view: SubscriptionsView::default(),
            dns_view: DnsView::default(),
            proxies_view: ProxiesView::default(),
            testing: HashSet::new(),
            fetches: 0,
            applied_fetch: 0,
            rules_view: RulesView::default(),
            rule_fetches: 0,
            applied_rule_fetch: 0,
            providers_view: ProvidersView::default(),
            proxy_provider_fetches: 0,
            applied_proxy_provider_fetch: 0,
            rule_provider_fetches: 0,
            applied_rule_provider_fetch: 0,
            deferred_rule_providers: None,
            local_delays: HashMap::new(),
            toasts: VecDeque::new(),
            overlay: None,
            pending: None,
            help_scroll: 0,
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
            logs_view: LogsView::default(),
            reconnect: None,
            refreshing: false,
            should_quit: false,
            size,
            hits: Hits::default(),
            last_click: None,
            dirty: false,
        }
    }

    /// Spawns the startup requests and the WebSocket streams. Calling it
    /// again repeats the requests; running streams are left alone.
    pub fn start(&mut self) {
        match self.core_phase {
            None | Some(CorePhase::Running) => self.start_requests(),
            // The kernel comes first; its controller is what the requests
            // are for.
            Some(CorePhase::Idle) => self.begin_core(),
            Some(CorePhase::Starting(_) | CorePhase::Failed(_) | CorePhase::Stopped) => {}
        }
    }

    /// The startup requests and the streams of a controller that answers.
    fn start_requests(&mut self) {
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

    /// Loads what the proxies page shows: `/proxies`, and the proxy
    /// providers for the nodes it leaves out. Both requests come back as one
    /// reply, so the page never takes in half of a load. Every load and
    /// reload of the page goes through here.
    fn fetch_proxies(&mut self, refetch: bool) {
        self.fetches += 1;
        let seq = self.fetches;
        self.spawn(move |client| async move {
            let (result, providers) = tokio::join!(client.proxies(), client.proxy_providers());
            Msg::Proxies {
                seq,
                refetch,
                result,
                providers,
            }
        });
    }

    /// Fetches `/rules` in the background.
    fn fetch_rules(&mut self, refetch: bool) {
        self.rule_fetches += 1;
        let seq = self.rule_fetches;
        self.spawn(move |client| async move {
            Msg::Rules {
                seq,
                refetch,
                result: client.rules().await,
            }
        });
    }

    /// Fetches `/providers/proxies` in the background.
    fn fetch_proxy_providers(&mut self, refetch: bool) {
        self.proxy_provider_fetches += 1;
        let seq = self.proxy_provider_fetches;
        self.spawn(move |client| async move {
            Msg::Providers(ProviderMsg::Proxies {
                seq,
                refetch,
                result: client.proxy_providers().await,
            })
        });
    }

    /// Fetches `/providers/rules` in the background, from mihomo only:
    /// original Clash has no such endpoint, and its section says so instead.
    /// While the kernel is not known (not asked yet, or unreachable), the
    /// fetch waits for the `/version` reply that tells.
    fn fetch_rule_providers(&mut self, refetch: bool) {
        match self.core_kind() {
            Some(CoreKind::Mihomo) => {
                self.deferred_rule_providers = None;
                self.rule_provider_fetches += 1;
                let seq = self.rule_provider_fetches;
                self.spawn(move |client| async move {
                    Msg::Providers(ProviderMsg::Rules {
                        seq,
                        refetch,
                        result: client.rule_providers().await,
                    })
                });
            }
            Some(CoreKind::Clash) => {}
            None => self.deferred_rule_providers = Some(refetch),
        }
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
        let outbox = self.outbox.clone();
        let msg = request(self.client.clone());
        tokio::spawn(async move {
            outbox.send(msg.await).await;
        });
    }

    pub fn update(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::Resize(size) => self.resize(size),
            // A click outside a popup is `Esc` to it.
            Action::Pick(Target::Backdrop) if self.overlay.is_some() => {
                self.update(Action::Dismiss);
            }
            Action::Pick(target) => {
                self.pick(&target);
            }
            // Two quick clicks are a click and `Enter`.
            Action::Activate(target) => {
                if self.pick(&target) {
                    self.update(Action::Confirm);
                }
            }
            _ if self.overlay == Some(Overlay::Confirm) => self.answer(&action),
            _ if self.overlay == Some(Overlay::Profiles) => self.pick_profile(&action),
            _ if self.overlay == Some(Overlay::Themes) => self.pick_theme(&action),
            _ if self.overlay == Some(Overlay::Groups) => self.pick_group(&action),
            _ if self.overlay == Some(Overlay::Dns) => self.pick_dns(&action),
            Action::ToggleHelp => {
                // The connection popup is not swapped for the help.
                self.overlay = match self.overlay {
                    Some(Overlay::Help) => None,
                    None => {
                        self.help_scroll = 0;
                        Some(Overlay::Help)
                    }
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
            Action::Cursor(movement) if self.overlay == Some(Overlay::Help) => {
                self.scroll_help(movement);
            }
            // Overlays are modal: everything else waits until they close.
            _ if self.overlay.is_some() => {}
            Action::Wheel(target, movement) => self.wheel(&target, movement),
            Action::Profiles => {
                self.profile_cursor = self.profiles.current;
                self.overlay = Some(Overlay::Profiles);
                self.dirty = true;
            }
            Action::Themes => self.open_themes(),
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
            Action::CycleSort => self.cycle_sort(),
            Action::ReverseSort => {
                let view = &mut self.connections_view;
                view.set_sort(view.sort.reversed());
                self.dirty = true;
            }
            Action::SortBy(column) => {
                self.release_input();
                let view = &mut self.connections_view;
                view.set_sort(view.sort.toward(column));
                self.dirty = true;
            }
            Action::SetMode(mode) => self.set_mode(mode),
            Action::TogglePause => self.toggle_pause(),
            Action::CycleLevel => self.cycle_level(),
            Action::ClearLogs => {
                self.logs_view.clear();
                self.dirty = true;
            }
            Action::StartFilter => self.set_typing(true),
            Action::FilterInput(key) => self.type_into_filter(key),
            Action::FilterAccept => self.accept_input(),
            Action::FilterClear => {
                self.clear_filter();
                self.dirty = true;
            }
            Action::UpdateProvider => self.update_provider(),
            Action::UpdateProviders => self.ask_update_providers(),
            Action::HealthCheck => self.health_check(),
            Action::AddSubscription => self.add_subscription(),
            Action::UpdateSubscription => self.update_subscription(),
            Action::UpdateSubscriptions => self.ask_update_subscriptions(),
            Action::DeleteSubscription => self.ask_delete_subscription(),
            Action::ToggleAutoUpdate => self.toggle_auto_update(),
            Action::RenameSubscription => self.rename_subscription(),
            Action::EditInterval => self.edit_interval(),
            Action::ChangeAgeKey => self.change_age_key(),
            Action::Refresh => self.refresh(),
        }
    }

    /// `r`: retries what failed and repeats the startup requests at once,
    /// plus the current page's own data where it has some.
    fn refresh(&mut self) {
        if self.retry_core() {
            return;
        }
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
        // Rules and providers are not among the startup requests: `r` on
        // their page is what reloads them.
        match self.page {
            Page::Rules => {
                if matches!(self.rules_view.rows, Some(Err(_))) {
                    self.rules_view.rows = None;
                }
                self.fetch_rules(false);
            }
            Page::Subscriptions => self.load_subscriptions(),
            Page::Providers => {
                let view = &mut self.providers_view;
                if matches!(view.proxies, Some(Err(_))) {
                    view.proxies = None;
                }
                if matches!(view.rules, Some(Err(_))) {
                    view.rules = None;
                }
                self.fetch_proxy_providers(false);
                self.fetch_rule_providers(false);
            }
            _ => {}
        }
        self.refreshing = true;
        self.streams.retry_now();
        self.start();
    }

    /// `m`: asks the kernel for the next mode and, once it agrees, fetches
    /// `/configs` to show what it really runs. An unknown mode goes to rule.
    fn cycle_mode(&mut self) {
        self.change_mode(None);
    }

    /// A click on a mode: the same request as `m`, for that mode. The one
    /// the kernel already runs is left alone.
    fn set_mode(&mut self, mode: Mode) {
        self.release_input();
        self.change_mode(Some(mode));
    }

    /// Asks the kernel for `wanted`, or for the mode after the current one.
    fn change_mode(&mut self, wanted: Option<Mode>) {
        if self.changing.contains_key(&Setting::Mode) {
            return;
        }
        let Some(Ok(configs)) = &self.configs else {
            self.toast(ToastKind::Error, "尚未获取到当前模式".to_owned());
            return;
        };
        let current = configs.mode();
        if wanted.is_some() && wanted == current {
            return;
        }
        let target = wanted.unwrap_or_else(|| current.map_or(Mode::Rule, Mode::next));
        self.send_change(Change::Patch(Patch::Mode(target)));
    }

    /// `s`: the next ordering of the proxies' nodes, or the next column the
    /// connections are sorted by.
    fn cycle_sort(&mut self) {
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
        match self.page {
            Page::Connections => {
                self.move_in_connections(movement);
                return;
            }
            Page::Logs => {
                let fit = self.logs_fit();
                self.logs_view.scroll(movement, fit);
                self.dirty = true;
                return;
            }
            Page::Rules => {
                self.move_in_rules(movement);
                return;
            }
            Page::Settings => {
                let screen = Rect::from(self.size);
                let capacity = settings::capacity(regions(screen, self.reconnect.is_some()).body);
                let half = (capacity / 2).max(1);
                let rows = self.settings().len();
                let view = &mut self.settings_view;
                view.cursor = movement.apply(view.cursor, rows, half);
                self.dirty = true;
                return;
            }
            Page::Providers => {
                let screen = Rect::from(self.size);
                let capacity = providers::capacity(regions(screen, self.reconnect.is_some()).body);
                let half = (capacity / 2).max(1);
                let view = &mut self.providers_view;
                view.cursor = movement.apply(view.cursor, view.rows().len(), half);
                self.dirty = true;
                return;
            }
            Page::Subscriptions => {
                let screen = Rect::from(self.size);
                let capacity = crate::ui::subscriptions::capacity(
                    regions(screen, self.reconnect.is_some()).body,
                );
                let half = (capacity / 2).max(1);
                let view = &mut self.subscriptions_view;
                view.cursor = movement.apply(view.cursor, view.visible().len(), half);
                self.dirty = true;
                return;
            }
            Page::Proxies => {}
            Page::Overview => return,
        }
        // Narrow, the groups are one picker row: down or right leaves it
        // for the grid, and nothing else moves on it.
        let stacked = self.proxies_fit().stacked;
        match self.proxies_view.focus {
            Pane::Groups if movement == Move::Right || (stacked && movement == Move::Down) => {
                if self.selected_group().is_some() {
                    self.proxies_view.focus = Pane::Nodes;
                }
            }
            Pane::Groups if stacked => {}
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

    /// Applies a new terminal size: what is laid out by it settles, and the
    /// group picker's drop-down closes once the page is wide enough for the
    /// group list. The page's focus and selection stay as they were.
    fn resize(&mut self, size: Size) {
        self.size = size;
        let fit = self.logs_fit();
        self.logs_view.settle(fit);
        if self.overlay == Some(Overlay::Groups) && !self.proxies_fit().stacked {
            self.close_overlay();
        }
        let screen = Rect::from(size);
        self.help_scroll = self.help_scroll.min(help::max_scroll(screen, self.page));
        self.dirty = true;
    }

    /// Scrolls the help by `movement`, a row at a time or half of what
    /// shows, within what it has to show.
    fn scroll_help(&mut self, movement: Move) {
        let screen = Rect::from(self.size);
        let rows = help::max_scroll(screen, self.page) + 1;
        let half = (help::visible_rows(screen, self.page) / 2).max(1);
        self.help_scroll = movement.apply(self.help_scroll, rows, half);
        self.dirty = true;
    }

    /// The group picker's drop-down answers to the cursor keys, `Enter`
    /// (which selects the group under its cursor) and `Esc`; quitting and
    /// resizing are handled before it is asked.
    fn pick_group(&mut self, action: &Action) {
        let len = self.visible_groups().count();
        match action {
            Action::Cursor(movement) => {
                let half = (self.proxies_fit().groups / 2).max(1);
                let view = &mut self.proxies_view;
                view.picker = movement.apply(view.picker, len, half);
                self.dirty = true;
            }
            Action::Confirm => {
                let view = &mut self.proxies_view;
                let group = view.picker.min(len.saturating_sub(1));
                if group != view.group {
                    view.group = group;
                    view.node = None;
                }
                self.close_overlay();
            }
            Action::Dismiss => self.close_overlay(),
            _ => {}
        }
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

    /// The room for log lines in the terminal, laid out by the function that
    /// draws them.
    fn logs_fit(&self) -> Fit {
        let screen = Rect::from(self.size);
        logs::fit(regions(screen, self.reconnect.is_some()).body)
    }

    /// Moves the rules cursor; half a page is half the rows the table
    /// shows, laid out by the function that draws it.
    fn move_in_rules(&mut self, movement: Move) {
        let screen = Rect::from(self.size);
        let capacity = rules::capacity(regions(screen, self.reconnect.is_some()).body);
        let half = (capacity / 2).max(1);
        let view = &mut self.rules_view;
        let len = view.visible().len();
        view.cursor = movement.apply(view.cursor, len, half);
        self.dirty = true;
    }

    /// `l`: resubscribes the log at the next level. What is shown stays.
    ///
    /// The old connection goes without a word and the new one reports in by
    /// itself, so this is no outage, no banner and no refetch. Until it is
    /// in, the stream does not count as up: should every other stream drop
    /// meanwhile, that is still noticed. Nor does the page call it live: a
    /// new connection that never gets in never reports going down either.
    fn cycle_level(&mut self) {
        let level = self.logs_view.level.next();
        self.logs_view.level = level;
        self.streams.set_log_level(level);
        self.links.up.remove(&Stream::Logs);
        if self.logs_view.link == LogLink::Live {
            self.logs_view.link = LogLink::Resubscribing;
        }
        self.dirty = true;
    }

    fn move_in_connections(&mut self, movement: Move) {
        let half = (self.connections_capacity() / 2).max(1);
        let view = &mut self.connections_view;
        let len = view.visible().len();
        view.cursor = movement.apply(view.cursor, len, half);
        self.dirty = true;
    }

    /// Whether the current page's input box has the keyboard: a filter, the
    /// port being edited on the settings page, or a DNS key in its popup.
    pub fn typing(&self) -> bool {
        if self.overlay == Some(Overlay::Dns) {
            return self.dns_view.editing.is_some();
        }
        match self.page {
            Page::Proxies => self.proxies_view.typing,
            Page::Providers => self.providers_view.typing,
            Page::Connections => self.connections_view.typing,
            Page::Rules => self.rules_view.typing,
            Page::Logs => self.logs_view.typing,
            Page::Settings => self.settings_view.editing.is_some(),
            Page::Subscriptions => {
                self.subscriptions_view.typing || self.subscriptions_view.adding.is_some()
            }
            Page::Overview => false,
        }
    }

    /// The current page's filter, on pages that have one.
    pub fn filter(&self) -> Option<&Input> {
        match self.page {
            Page::Proxies => Some(&self.proxies_view.filter),
            Page::Providers => Some(&self.providers_view.filter),
            Page::Connections => Some(&self.connections_view.filter),
            Page::Rules => Some(&self.rules_view.filter),
            Page::Logs => Some(&self.logs_view.filter),
            // What is typed to import goes where the filter goes.
            Page::Subscriptions => Some(
                self.subscriptions_view
                    .adding
                    .as_ref()
                    .unwrap_or(&self.subscriptions_view.filter),
            ),
            _ => None,
        }
    }

    /// Maps a mouse event to an [`Action`] by what the last frame drew
    /// under it; `None` means the event is ignored. A press that lands on
    /// nothing, and every other button and movement, is ignored; so is
    /// everything while the terminal is too small to draw into, which
    /// registers nothing.
    ///
    /// The wheel moves what a popup lists wherever the pointer is, as the
    /// keys do; without a popup, it moves the list under the pointer.
    pub fn mouse_action(&mut self, event: MouseEvent, now: Instant) -> Option<Action> {
        let position = Position::new(event.column, event.row);
        let movement = match event.kind {
            MouseEventKind::Down(MouseButton::Left) => return self.click(position, now),
            MouseEventKind::ScrollUp => Move::Up,
            MouseEventKind::ScrollDown => Move::Down,
            _ => return None,
        };
        if self.overlay.is_some() {
            return Some(Action::Cursor(movement));
        }
        let target = self.hits.scroll_area(position)?;
        Some(Action::Wheel(target, movement))
    }

    /// A left press at `position`: a pick, or an activation when it is the
    /// second one on the same row within [`DOUBLE_CLICK`] of the first.
    fn click(&mut self, position: Position, now: Instant) -> Option<Action> {
        let target = self.hits.at(position)?;
        let again = matches!(
            &self.last_click,
            Some((last, at)) if *last == target && now.saturating_duration_since(*at) <= DOUBLE_CLICK
        );
        if again && target.activatable() {
            // A third press starts over.
            self.last_click = None;
            return Some(Action::Activate(target));
        }
        self.last_click = Some((target.clone(), now));
        // These two are not a cursor move but a change of order or mode.
        Some(match target {
            Target::Column(column) => Action::SortBy(column),
            Target::Mode(mode) => Action::SetMode(mode),
            target => Action::Pick(target),
        })
    }

    /// A click on `target`: puts the cursor on it, or opens what it opens.
    /// An input box that has the keyboard lets go of it first, as `Enter`
    /// (a filter) or `Esc` (a port, whose value is not sent) would make it.
    ///
    /// Returns whether it took: what was drawn can be a tick old, and
    /// something that is no longer there is ignored, not replaced by what
    /// the cursor was on.
    fn pick(&mut self, target: &Target) -> bool {
        self.dirty = true;
        if self.overlay.is_some() {
            return self.pick_in_popup(target);
        }
        self.release_input();
        match target {
            Target::Tab(page) => {
                self.set_page(*page);
                true
            }
            Target::Group(name) => {
                let Some(index) = self.visible_groups().position(|group| group.name == *name)
                else {
                    return false;
                };
                let view = &mut self.proxies_view;
                view.focus = Pane::Groups;
                if view.group != index {
                    view.group = index;
                    view.node = None;
                }
                true
            }
            Target::GroupPicker => {
                self.proxies_view.focus = Pane::Groups;
                self.confirm();
                true
            }
            Target::Node(name) => {
                let Some(index) = self.visible_nodes().iter().position(|node| node == name) else {
                    return false;
                };
                self.proxies_view.focus = Pane::Nodes;
                self.proxies_view.node = Some(index);
                true
            }
            Target::Connection(id) => {
                let view = &mut self.connections_view;
                let Some(index) = view
                    .visible()
                    .iter()
                    .position(|row| row.connection.id == *id)
                else {
                    return false;
                };
                view.cursor = index;
                true
            }
            Target::Rule(number) => {
                let view = &mut self.rules_view;
                let Some(index) = view.visible().iter().position(|row| row.number == *number)
                else {
                    return false;
                };
                view.cursor = index;
                true
            }
            Target::Provider(provider) => {
                let view = &mut self.providers_view;
                let Some(index) = view.rows().iter().position(|row| row.target() == *provider)
                else {
                    return false;
                };
                view.cursor = index;
                true
            }
            Target::Subscription(id) => {
                let view = &mut self.subscriptions_view;
                let Some(index) = view.visible().iter().position(|sub| sub.id == *id) else {
                    return false;
                };
                view.cursor = index;
                true
            }
            Target::Setting(setting) => {
                let Some(index) = self.settings().iter().position(|each| each == setting) else {
                    return false;
                };
                self.settings_view.cursor = index;
                true
            }
            Target::Column(_)
            | Target::Mode(_)
            | Target::Pane(_)
            | Target::List
            | Target::Backdrop
            | Target::Popup
            | Target::PickerItem(_)
            | Target::Profile(_)
            | Target::Theme(_) => false,
        }
    }

    /// A click on a row of the popup that is open: its cursor goes there,
    /// and the theme picker previews the theme.
    fn pick_in_popup(&mut self, target: &Target) -> bool {
        match (self.overlay, target) {
            (Some(Overlay::Profiles), &Target::Profile(index))
                if index < self.profiles.choices.len() =>
            {
                self.profile_cursor = index;
                true
            }
            (Some(Overlay::Themes), &Target::Theme(index)) if index < self.themes.entries.len() => {
                self.themes.cursor = index;
                self.preview_theme();
                true
            }
            (Some(Overlay::Groups), Target::PickerItem(name)) => {
                let Some(index) = self.visible_groups().position(|group| group.name == *name)
                else {
                    return false;
                };
                self.proxies_view.picker = index;
                true
            }
            _ => false,
        }
    }

    /// A wheel step over `target`: the list moves a row, and a pane of the
    /// proxies page takes the focus.
    fn wheel(&mut self, target: &Target, movement: Move) {
        // The port being typed belongs to the row the cursor is on.
        if self.page == Page::Settings && self.typing() {
            return;
        }
        match target {
            Target::Pane(pane) if self.page == Page::Proxies => {
                if self.selected_group().is_some() {
                    self.proxies_view.focus = *pane;
                    self.move_cursor(movement);
                }
            }
            Target::List => self.move_cursor(movement),
            _ => {}
        }
    }

    /// Takes the keyboard from the input box that has it, the way a click
    /// elsewhere does.
    fn release_input(&mut self) {
        if !self.typing() {
            return;
        }
        match self.page {
            Page::Settings => self.settings_view.editing = None,
            Page::Subscriptions => {
                self.subscriptions_view.adding = None;
                self.subscriptions_view.ask = Ask::Import;
                self.set_typing(false);
            }
            _ => self.set_typing(false),
        }
        self.dirty = true;
    }

    /// `p`: freezes or resumes the current page's list.
    fn toggle_pause(&mut self) {
        match self.page {
            Page::Connections => {
                self.connections_view.paused = !self.connections_view.paused;
            }
            Page::Logs => self.logs_view.toggle_pause(),
            _ => {}
        }
        self.dirty = true;
    }

    /// A key typed into the current page's input box.
    fn type_into_filter(&mut self, key: KeyEvent) {
        let event = Event::Key(key);
        match self.page {
            Page::Proxies => {
                self.proxies_view.filter.handle_event(&event);
                self.proxies_view.node = None;
            }
            Page::Providers => self.providers_view.edit_filter(|filter| {
                filter.handle_event(&event);
            }),
            Page::Connections => self.connections_view.edit_filter(|filter| {
                filter.handle_event(&event);
            }),
            Page::Rules => self.rules_view.edit_filter(|filter| {
                filter.handle_event(&event);
            }),
            Page::Logs => {
                self.logs_view.filter.handle_event(&event);
                let fit = self.logs_fit();
                self.logs_view.settle(fit);
            }
            Page::Settings => self.settings_view.type_key(key),
            Page::Subscriptions => {
                let view = &mut self.subscriptions_view;
                match &mut view.adding {
                    // Hours are digits.
                    Some(_)
                        if matches!(view.ask, Ask::Interval(_))
                            && matches!(key.code, KeyCode::Char(ch)
                            if !ch.is_ascii_digit()
                                && !key.modifiers.intersects(
                                    KeyModifiers::CONTROL | KeyModifiers::ALT,
                                )) => {}
                    Some(input) => {
                        input.handle_event(&event);
                    }
                    None => view.edit_filter(|filter| {
                        filter.handle_event(&event);
                    }),
                }
            }
            Page::Overview => {}
        }
        self.dirty = true;
    }

    /// Gives the keyboard to the current page's filter box, or back.
    fn set_typing(&mut self, typing: bool) {
        match self.page {
            Page::Proxies => self.proxies_view.typing = typing,
            Page::Providers => self.providers_view.typing = typing,
            Page::Connections => self.connections_view.typing = typing,
            Page::Rules => self.rules_view.typing = typing,
            Page::Logs => self.logs_view.typing = typing,
            Page::Subscriptions => self.subscriptions_view.typing = typing,
            _ => {}
        }
        self.dirty = true;
    }

    /// `Enter` in the input box: keeps the filter, or sends what was typed
    /// on the settings page.
    fn accept_input(&mut self) {
        if self.page == Page::Settings {
            match self.settings_view.editing.as_ref().map(|(field, _)| *field) {
                Some(Field::Port(_)) => self.submit_port(),
                Some(Field::Proxy) => self.submit_proxy(),
                None => {}
            }
        } else if self.subscriptions_view.adding.is_some() && self.page == Page::Subscriptions {
            self.submit_subscription();
        } else {
            self.set_typing(false);
        }
    }

    /// Drops the current page's filter, or the port being typed, and gives
    /// the keyboard back.
    fn clear_filter(&mut self) {
        match self.page {
            Page::Proxies => {
                self.proxies_view.filter.reset();
                self.proxies_view.typing = false;
                self.proxies_view.node = None;
            }
            Page::Providers => self.providers_view.clear_filter(),
            Page::Connections => self.connections_view.clear_filter(),
            Page::Rules => self.rules_view.clear_filter(),
            Page::Logs => {
                self.logs_view.filter.reset();
                self.logs_view.typing = false;
                let fit = self.logs_fit();
                self.logs_view.settle(fit);
            }
            Page::Settings => self.settings_view.editing = None,
            Page::Subscriptions => {
                // The box of an import first, then the filter.
                self.subscriptions_view.ask = Ask::Import;
                if self.subscriptions_view.adding.take().is_none() {
                    self.subscriptions_view.clear_filter();
                }
            }
            Page::Overview => {}
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
    /// opens the details of the connection under the cursor. On the
    /// settings page (`Space` too), acts on the setting under the cursor.
    fn confirm(&mut self) {
        match self.page {
            Page::Proxies if self.selected_group().is_some() => match self.proxies_view.focus {
                // Narrow, the picker row opens its drop-down on the
                // selected group.
                Pane::Groups if self.proxies_fit().stacked => {
                    let len = self.visible_groups().count();
                    self.proxies_view.picker = self.proxies_view.group.min(len - 1);
                    self.overlay = Some(Overlay::Groups);
                    self.dirty = true;
                }
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
            Page::Settings => self.activate_setting(),
            Page::Subscriptions => self.switch_subscription(),
            _ => {}
        }
    }

    /// Whether the kernel can take `setting`: original Clash has none of
    /// the mihomo additions, and a value the kernel did not report cannot
    /// be changed from. An unknown kernel counts as mihomo; the request
    /// would say otherwise.
    pub fn gate(&self, setting: Setting, configs: &Configs) -> Gate {
        let clash = self.core_kind() == Some(CoreKind::Clash);
        if clash && setting.mihomo_only() {
            return Gate::NeedsMihomo;
        }
        if setting == Setting::UpgradeCore
            && self
                .managed_core()
                .is_some_and(|core| core.is_user_binary())
        {
            return Gate::SelfManaged;
        }
        if setting == Setting::LoginStart && self.login_start.is_some_and(|state| !state.supported)
        {
            return Gate::NeedsSystemd;
        }
        if setting == Setting::SystemProxy
            && self.system_proxy.is_some_and(|state| !state.supported)
        {
            return Gate::NoDesktopProxy;
        }
        // One port is enough for a kernel the user does not run by hand.
        if self.is_managed() && matches!(setting, Setting::Port(port) if port != Port::Mixed) {
            return Gate::MixedOnly;
        }
        let reported = match setting {
            Setting::AllowLan => configs.allow_lan.is_some(),
            Setting::LogLevel => !configs.log_level.is_empty(),
            Setting::Ipv6 => configs.ipv6.is_some(),
            Setting::Port(port) => port.of(configs).is_some(),
            Setting::Tun => configs.tun.is_some(),
            Setting::Mode
            | Setting::Reload
            | Setting::FlushFakeIp
            | Setting::UpdateGeo
            | Setting::Restart
            | Setting::Upgrade
            | Setting::StopCore
            | Setting::CoreBinary
            | Setting::LoginStart
            | Setting::SystemProxy
            | Setting::Dns
            | Setting::SubscriptionDownload
            | Setting::UpgradeCore => true,
        };
        match (reported, clash) {
            (true, _) => Gate::Open,
            (false, true) => Gate::NeedsMihomo,
            (false, false) => Gate::Missing,
        }
    }

    /// Whether a change to `setting` is in progress.
    pub fn is_changing(&self, setting: Setting) -> bool {
        self.changing.contains_key(&setting)
    }

    /// `Enter` / `Space` on a setting: toggles a switch, steps the mode or
    /// the kernel log level, opens a port for editing, or runs a
    /// maintenance action, asking first where [`Setting::asks`]. One still
    /// in progress is left alone; one the kernel lacks only says so.
    fn activate_setting(&mut self) {
        let Some(Ok(configs)) = &self.configs else {
            return;
        };
        let setting = self.selected_setting();
        if self.is_changing(setting) {
            return;
        }
        let gate = self.gate(setting, configs);
        let flip = |on: Option<bool>| !on.unwrap_or_default();
        let change = match setting {
            Setting::Mode
            | Setting::Port(_)
            | Setting::StopCore
            | Setting::CoreBinary
            | Setting::LoginStart
            | Setting::SystemProxy
            | Setting::Dns
            | Setting::SubscriptionDownload
            | Setting::UpgradeCore => None,
            Setting::AllowLan => Some(Change::Patch(Patch::AllowLan(flip(configs.allow_lan)))),
            Setting::LogLevel => Some(Change::Patch(Patch::LogLevel(next_kernel_log_level(
                &configs.log_level,
            )))),
            Setting::Ipv6 => Some(Change::Patch(Patch::Ipv6(flip(configs.ipv6)))),
            Setting::Tun => Some(Change::Patch(Patch::Tun(flip(
                configs.tun.as_ref().map(|tun| tun.enable),
            )))),
            Setting::Reload => Some(Change::Reload),
            Setting::FlushFakeIp => Some(Change::FlushFakeIp),
            Setting::UpdateGeo => Some(Change::UpdateGeo),
            Setting::Restart => Some(Change::Restart),
            Setting::Upgrade => Some(Change::Upgrade),
        };
        let current_port = match setting {
            Setting::Port(port) => port.of(configs),
            _ => None,
        };
        match gate {
            Gate::Open => {}
            Gate::SelfManaged => {
                self.toast(
                    ToastKind::Error,
                    "内核由你自己管理，tabby 不更新它".to_owned(),
                );
                return;
            }
            Gate::MixedOnly => {
                self.toast(ToastKind::Error, "托管内核只用 Mixed 端口".to_owned());
                return;
            }
            Gate::NeedsMihomo => {
                self.toast(ToastKind::Error, "需要 mihomo".to_owned());
                return;
            }
            Gate::NoDesktopProxy => {
                self.toast(ToastKind::Error, "当前桌面不支持系统代理".to_owned());
                return;
            }
            Gate::NeedsSystemd => {
                self.toast(ToastKind::Error, "登录时启动需要 systemd".to_owned());
                return;
            }
            Gate::Missing => {
                self.toast(ToastKind::Error, "内核没有提供这一项".to_owned());
                return;
            }
        }
        match (setting, change) {
            (Setting::StopCore, _) => self.ask_stop_core(),
            (Setting::LoginStart, _) => self.toggle_login_start(),
            (Setting::SystemProxy, _) => self.toggle_system_proxy(),
            (Setting::Dns, _) => self.open_dns(),
            (Setting::SubscriptionDownload, _) => self.cycle_subscription_download(),
            // The kernel needs the capabilities before it can run a TUN.
            (Setting::Tun, Some(change @ Change::Patch(Patch::Tun(true)))) if self.is_managed() => {
                self.ask_enable_tun(change);
            }
            // The config is made again from the subscription first.
            (Setting::Reload, _) if self.is_managed() => {
                self.pending = Some(Confirm::RegenerateConfig);
                self.overlay = Some(Overlay::Confirm);
                self.dirty = true;
            }
            (Setting::CoreBinary, _) => {}
            (Setting::UpgradeCore, _) => self.check_upgrade(),
            (Setting::Mode, _) => self.cycle_mode(),
            (Setting::Port(port), _) => {
                // Off is typed as nothing rather than as a 0 to delete first.
                let typed = current_port
                    .filter(|number| *number != 0)
                    .map(|number| number.to_string())
                    .unwrap_or_default();
                self.settings_view.editing = Some((Field::Port(port), Input::new(typed)));
                self.dirty = true;
            }
            (_, Some(change)) if setting.asks() => {
                self.pending = Some(Confirm::Change(change));
                self.overlay = Some(Overlay::Confirm);
                self.dirty = true;
            }
            (_, Some(change)) => self.send_change(change),
            (_, None) => {}
        }
    }

    /// `Enter` in the port box: sends a valid port that differs from the
    /// current one and closes the box; empty closes it without a word;
    /// anything else is refused with the box left open to fix it.
    fn submit_port(&mut self) {
        let Some((Field::Port(port), input)) = self.settings_view.editing.take() else {
            return;
        };
        self.dirty = true;
        match parse_port(input.value()) {
            Ok(None) => {}
            Ok(Some(number)) => {
                let current = match &self.configs {
                    Some(Ok(configs)) => port.of(configs),
                    _ => None,
                };
                if current != Some(number) {
                    self.send_change(Change::Patch(Patch::Port(port, number)));
                }
            }
            Err(why) => {
                self.settings_view.editing = Some((Field::Port(port), input));
                self.toast(ToastKind::Error, why.to_owned());
            }
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
    fn answer(&mut self, action: &Action) {
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
            Some(Overlay::Dns) => self.close_dns(),
            Some(Overlay::Help | Overlay::Profiles | Overlay::Themes | Overlay::Groups) | None => {}
        }
        self.dirty = true;
    }

    /// The profile popup answers to the cursor keys, `Enter`, `Esc` and
    /// `P`; quitting and resizing are handled before it is asked. Without a
    /// profile to offer it only explains how to add one, and only closes.
    fn pick_profile(&mut self, action: &Action) {
        match action {
            Action::Cursor(movement) if !self.profiles.is_empty() => {
                let len = self.profiles.choices.len();
                let half = (len / 2).max(1);
                self.profile_cursor = movement.apply(self.profile_cursor, len, half);
                self.dirty = true;
            }
            Action::Confirm if !self.profiles.is_empty() => {
                self.close_overlay();
                if self.profile_cursor != self.profiles.current {
                    self.switch_profile(self.profile_cursor);
                }
            }
            Action::Dismiss | Action::Profiles => self.close_overlay(),
            _ => {}
        }
    }

    /// `C`: offers every theme, the cursor on the one in use. The themes
    /// directory is read again, so a theme file edited meanwhile shows as
    /// it is now.
    fn open_themes(&mut self) {
        let picker = &mut self.themes;
        picker.entries = themes::scan(picker.dir.as_deref(), &self.settings.theme);
        picker.cursor = picker
            .entries
            .iter()
            .position(|entry| entry.id == self.settings.theme)
            .unwrap_or(0);
        picker.before = Some(self.theme);
        self.overlay = Some(Overlay::Themes);
        self.dirty = true;
    }

    /// The theme picker answers to the cursor keys, `Enter`, `Esc` and `C`;
    /// quitting and resizing are handled before it is asked. The whole
    /// screen shows the theme under the cursor as it moves.
    fn pick_theme(&mut self, action: &Action) {
        match action {
            Action::Cursor(movement) => {
                let len = self.themes.entries.len();
                let half = (len / 2).max(1);
                self.themes.cursor = movement.apply(self.themes.cursor, len, half);
                self.preview_theme();
                self.dirty = true;
            }
            Action::Confirm => self.choose_theme(),
            Action::Dismiss | Action::Themes => {
                if let Some(before) = self.themes.before.take() {
                    self.theme = before;
                }
                self.close_overlay();
            }
            _ => {}
        }
    }

    /// Shows the theme under the picker's cursor on the whole screen. A
    /// theme that cannot be used shows the one in use.
    fn preview_theme(&mut self) {
        self.theme = match self.themes.entries.get(self.themes.cursor) {
            Some(Entry {
                theme: Ok(theme), ..
            }) => theme.with(&self.settings.theme_overrides),
            _ => self.themes.before.unwrap_or(self.theme),
        };
    }

    /// `Enter` in the theme picker: keeps the theme under the cursor and
    /// saves it as `theme` in `config.toml`. One that cannot be used is
    /// not taken: the picker stays open, its row saying why.
    fn choose_theme(&mut self) {
        let Some(entry) = self.themes.entries.get(self.themes.cursor) else {
            return;
        };
        let &Ok(theme) = &entry.theme else {
            return;
        };
        let id = entry.id.clone();
        self.theme = theme.with(&self.settings.theme_overrides);
        self.themes.before = None;
        self.close_overlay();
        if id == self.settings.theme {
            return;
        }
        tracing::info!(theme = %id, "switching theme");
        self.settings.theme.clone_from(&id);
        let saved = match &self.themes.config {
            Some(path) => config::save_theme(path, &id).map_err(|err| format!("{err:#}")),
            None => Err("找不到配置文件的位置".to_owned()),
        };
        match saved {
            Ok(()) if self.settings.theme_pinned => self.toast(
                ToastKind::Success,
                format!("已切换到主题 {id} 并保存；下次启动仍以 --theme / TABBY_THEME 为准"),
            ),
            Ok(()) => self.toast(ToastKind::Success, format!("已切换到主题 {id} 并保存")),
            Err(err) => {
                tracing::warn!("cannot save theme: {err}");
                self.toast(
                    ToastKind::Error,
                    format!("已切换到主题 {id}，但未能保存：{err}"),
                );
            }
        }
    }

    /// Connects to the controller of choice `index` of [`App::profiles`]
    /// in place of the current one.
    ///
    /// Everything learnt from the old controller goes, the way it came: the
    /// app starts over on the new one as it does at launch. Only what is
    /// the user's and not the controller's stays: the theme, the icons and
    /// the delay test settings, the page, the orderings of the proxies and the
    /// connections, and the level the log is subscribed at. The old
    /// streams stop as their [`Streams`] is dropped; what they or the old
    /// requests still send is stamped with the old epoch and dropped on
    /// arrival.
    fn switch_profile(&mut self, index: usize) {
        let Some(choice) = self.profiles.choices.get(index) else {
            return;
        };
        let label = choice.label().to_owned();
        tracing::info!(profile = %choice.name, url = %choice.url, "switching profile");
        match self.rebuild(index) {
            Ok(page) => {
                self.start();
                // The page loads what it loads on a first visit.
                self.set_page(page);
                self.toast(ToastKind::Success, format!("已切换到 {label}"));
            }
            Err(err) => self.toast(ToastKind::Error, format!("无法切换到 {label}：{err}")),
        }
    }

    /// Starts over on choice `index`: everything learnt from the old
    /// controller goes, the way it came, and the app is where it is at
    /// launch, on the first page. Returns the page the user was on.
    fn rebuild(&mut self, index: usize) -> Result<Page, color_eyre::Report> {
        let Some(choice) = self.profiles.choices.get(index) else {
            return Ok(self.page);
        };
        let client = ApiClient::new(&choice.url, choice.secret.as_deref())?;
        let settings = Settings {
            profile: choice.name.clone(),
            url: choice.url.clone(),
            secret: choice.secret.clone(),
            ..self.settings.clone()
        };
        let mut profiles = std::mem::take(&mut self.profiles);
        profiles.current = index;
        let themes = std::mem::take(&mut self.themes);
        let mut app = Self::connect(
            settings,
            self.theme,
            client,
            self.outbox.next(),
            self.size,
            profiles,
            themes,
        );
        app.proxies_view.sort = self.proxies_view.sort;
        app.connections_view.sort = self.connections_view.sort;
        app.logs_view.level = self.logs_view.level;
        app.streams.set_log_level(self.logs_view.level);
        let page = self.page;
        *self = app;
        Ok(page)
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
            Confirm::UpdateProviders { targets, .. } => self.update_providers(targets),
            Confirm::Change(change) => self.send_change(change),
            Confirm::StopCore => self.stop_core(),
            Confirm::RegenerateConfig => self.regenerate_config(),
            Confirm::AuthorizeTun => self.begin_authorization(),
            Confirm::UpdateSubscriptions { ids, .. } => self.update_subscriptions(ids),
            Confirm::DeleteSubscription { id, .. } => self.delete_subscription(id),
            Confirm::InstallCore { release, upgrade } => {
                if upgrade {
                    self.upgrade_core(release);
                } else {
                    self.install_core(release);
                }
            }
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
                // A managed kernel that is gone will not answer however
                // often it is asked.
                if first_failure && self.is_managed() {
                    self.check_core_alive();
                }
            }
        }
        self.version = Some(result);
        // Rule providers wait to know the kernel; Clash has none.
        match self.core_kind() {
            Some(CoreKind::Mihomo) => {
                if let Some(refetch) = self.deferred_rule_providers.take() {
                    self.fetch_rule_providers(refetch);
                }
            }
            Some(CoreKind::Clash) => {
                self.deferred_rule_providers = None;
                self.providers_view.rules = None;
            }
            None => {}
        }
    }

    /// The kernel behind the controller, as its latest `/version` reply
    /// says; `None` before one came in or after one failed.
    pub fn core_kind(&self) -> Option<CoreKind> {
        self.version.as_ref()?.as_ref().ok().map(Version::kind)
    }

    /// Swaps in a `/configs` reply from fetch `seq`, and ends the changes
    /// for which this is the refetch they waited for (or a later one).
    fn apply_configs(&mut self, seq: u64, refetch: bool, result: Result<Configs, ApiError>) {
        self.applied_config = seq;
        self.changing
            .retain(|_, stage| !matches!(*stage, Stage::Refetching(awaited) if seq >= awaited));
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

    /// Sends `change`, unless its setting is already being changed; the
    /// row spins until it ends.
    fn send_change(&mut self, change: Change) {
        let setting = change.setting();
        if self.changing.contains_key(&setting) {
            return;
        }
        self.changing.insert(setting, Stage::Sending);
        self.dirty = true;
        // The managed core is restarted through its supervisor.
        if change == Change::Restart && self.restart_core() {
            return;
        }
        self.spawn(move |client| async move {
            let result = change.send(&client).await;
            Msg::Changed { change, result }
        });
    }

    /// The kernel's answer to a change. A changed setting asks what the
    /// kernel now runs rather than assuming, and stays busy until that is
    /// in; a reload refreshes whatever else is loaded, too. A restart (or
    /// an upgrade, which restarts) goes through the outage handling: the
    /// streams drop and their return replays the startup requests.
    fn apply_change(&mut self, change: Change, result: Result<(), ApiError>) {
        let setting = change.setting();
        if let Err(err) = result {
            self.changing.remove(&setting);
            self.toast(
                ToastKind::Error,
                format!("{}：{}", change.failed(), reason(&err)),
            );
            return;
        }
        if let Some(text) = change.done() {
            self.toast(ToastKind::Success, text);
        }
        // What a managed kernel is changed to is what it is started with
        // next time.
        if let Change::Patch(patch) = change {
            self.persist_patch(patch);
        }
        match change {
            Change::Patch(_) | Change::Reload => {
                let seq = self.fetch_configs(true);
                self.changing.insert(setting, Stage::Refetching(seq));
            }
            Change::FlushFakeIp | Change::UpdateGeo | Change::Restart | Change::Upgrade => {
                self.changing.remove(&setting);
            }
        }
        if change == Change::Reload {
            if matches!(self.proxies, Some(Ok(_))) {
                self.fetch_proxies(true);
            }
            self.reload_visited();
        }
    }

    /// Takes in the reply to `/rules` fetch `seq`. One older than a reply
    /// already applied is dropped: it would roll the page back.
    fn apply_rules(&mut self, seq: u64, refetch: bool, result: Result<Vec<Rule>, ApiError>) {
        if seq < self.applied_rule_fetch {
            return;
        }
        self.applied_rule_fetch = seq;
        self.note_failure(&result);
        self.rules_view.apply(refetch, result);
    }

    /// Takes in a reply about the providers page.
    fn on_providers(&mut self, msg: ProviderMsg) {
        match msg {
            ProviderMsg::Proxies {
                seq,
                refetch,
                result,
            } => {
                // An older reply than one applied would roll the list back.
                if seq < self.applied_proxy_provider_fetch {
                    return;
                }
                self.applied_proxy_provider_fetch = seq;
                self.note_failure(&result);
                self.providers_view
                    .apply(|view| &mut view.proxies, refetch, result);
            }
            ProviderMsg::Rules {
                seq,
                refetch,
                result,
            } => {
                if seq < self.applied_rule_provider_fetch {
                    return;
                }
                self.applied_rule_provider_fetch = seq;
                self.note_failure(&result);
                self.providers_view
                    .apply(|view| &mut view.rules, refetch, result);
            }
            ProviderMsg::Done {
                target,
                work,
                result,
            } => self.provider_done(&target, work, result),
            ProviderMsg::Updated { results } => self.providers_updated(results),
        }
    }

    /// Entering the providers page: each list loads the first time,
    /// refreshes behind what is shown after that, and after a failure
    /// loads again with the spinner. A load still on its way is not asked
    /// for twice.
    fn load_providers(&mut self) {
        match &self.providers_view.proxies {
            Some(Ok(_)) => self.fetch_proxy_providers(true),
            Some(Err(_)) => {
                self.providers_view.proxies = None;
                self.fetch_proxy_providers(false);
            }
            None if self.proxy_provider_fetches == 0 => self.fetch_proxy_providers(false),
            None => {}
        }
        match &self.providers_view.rules {
            Some(Ok(_)) => self.fetch_rule_providers(true),
            Some(Err(_)) => {
                self.providers_view.rules = None;
                self.fetch_rule_providers(false);
            }
            None if self.rule_provider_fetches == 0 => self.fetch_rule_providers(false),
            None => {}
        }
    }

    /// After an outage: the lists loaded on demand reload too, if they ever
    /// were. Ones on screen refresh behind it; a failed one is replaced by
    /// whatever comes back. A page never visited is not asked for.
    fn reload_visited(&mut self) {
        if let Some(rows) = &self.rules_view.rows {
            self.fetch_rules(rows.is_ok());
        }
        if let Some(list) = &self.providers_view.proxies {
            self.fetch_proxy_providers(list.is_ok());
        }
        if let Some(list) = &self.providers_view.rules {
            self.fetch_rule_providers(list.is_ok());
        }
    }

    /// `u`: updates the provider under the cursor, unless something is
    /// already being done to it.
    fn update_provider(&mut self) {
        if let Some(row) = self.providers_view.selected() {
            let target = row.target();
            self.start_work(target, Work::Updating);
        }
    }

    /// `h`: health-checks the proxy provider under the cursor. Rule
    /// providers have nothing to check.
    fn health_check(&mut self) {
        let Some(row) = self.providers_view.selected() else {
            return;
        };
        let target = row.target();
        if target.kind == ProviderKind::Rule {
            self.toast(ToastKind::Error, "规则集合不支持健康检查".to_owned());
            return;
        }
        self.start_work(target, Work::Checking);
    }

    /// Sends `work` for `target`, which spins until the reply.
    fn start_work(&mut self, target: ProviderRef, work: Work) {
        if self.providers_view.busy.contains_key(&target) {
            return;
        }
        self.providers_view.busy.insert(target.clone(), work);
        self.dirty = true;
        let timeout = healthcheck_timeout(self.settings.delay_timeout_ms);
        self.spawn(move |client| async move {
            let result = match work {
                Work::Updating => update_one(&client, &target).await,
                Work::Checking => {
                    client
                        .healthcheck_proxy_provider(&target.name, timeout)
                        .await
                }
            };
            Msg::Providers(ProviderMsg::Done {
                target,
                work,
                result,
            })
        });
    }

    /// `U`: asks before updating what the page shows: every provider, or
    /// with a filter only the ones that pass it. Nothing shown, nothing
    /// asked.
    fn ask_update_providers(&mut self) {
        let view = &self.providers_view;
        let targets: Vec<ProviderRef> = view.rows().iter().map(ProviderRow::target).collect();
        if targets.is_empty() {
            return;
        }
        self.pending = Some(Confirm::UpdateProviders {
            filter: view.filter.value().to_owned(),
            targets,
        });
        self.overlay = Some(Overlay::Confirm);
        self.dirty = true;
    }

    /// Updates what `U` was confirmed for, all at once; the replies come
    /// back together in one message. Providers already being worked on
    /// are left to that.
    fn update_providers(&mut self, mut targets: Vec<ProviderRef>) {
        let busy = &mut self.providers_view.busy;
        targets.retain(|target| !busy.contains_key(target));
        if targets.is_empty() {
            return;
        }
        for target in &targets {
            busy.insert(target.clone(), Work::Updating);
        }
        self.spawn(move |client| async move {
            let results = join_all(targets.iter().map(|target| update_one(&client, target))).await;
            Msg::Providers(ProviderMsg::Updated {
                results: targets.into_iter().zip(results).collect(),
            })
        });
    }

    /// The reply to `u` or `h`: a toast either way, and on success the list
    /// fetched again to show the new count, health and time. A health check
    /// changes the delays the proxies page shows too.
    fn provider_done(&mut self, target: &ProviderRef, work: Work, result: Result<(), ApiError>) {
        self.providers_view.busy.remove(target);
        let name = &target.name;
        match (work, result) {
            (Work::Updating, Ok(())) => {
                self.toast(ToastKind::Success, format!("已更新 {name}"));
                self.refetch_providers(target.kind);
            }
            (Work::Checking, Ok(())) => {
                self.toast(ToastKind::Success, format!("{name} 健康检查完成"));
                self.refetch_providers(ProviderKind::Proxy);
                if matches!(self.proxies, Some(Ok(_))) {
                    self.fetch_proxies(true);
                }
            }
            // No name: a toast has room for about 44 columns, the reason
            // takes most of them, and the provider is the one under the
            // cursor anyway.
            (Work::Updating, Err(err)) => {
                self.toast(ToastKind::Error, format!("更新失败：{}", describe(&err)));
            }
            (Work::Checking, Err(err)) => {
                self.toast(
                    ToastKind::Error,
                    format!("健康检查失败：{}", describe(&err)),
                );
            }
        }
    }

    /// The replies to `U`, in one toast: how many were updated and, if any
    /// failed, how many and the first reason. The lists that changed are
    /// fetched again.
    fn providers_updated(&mut self, results: Vec<(ProviderRef, Result<(), ApiError>)>) {
        let mut updated = 0;
        let mut changed = HashSet::new();
        let mut failed = vec![];
        for (target, result) in results {
            self.providers_view.busy.remove(&target);
            match result {
                Ok(()) => {
                    updated += 1;
                    changed.insert(target.kind);
                }
                Err(err) => failed.push(err),
            }
        }
        match failed.first() {
            None => self.toast(ToastKind::Success, format!("已更新 {updated} 个 provider")),
            Some(first) => self.toast(
                ToastKind::Error,
                format!("成功 {updated}，失败 {}：{}", failed.len(), describe(first)),
            ),
        }
        for kind in [ProviderKind::Proxy, ProviderKind::Rule] {
            if changed.contains(&kind) {
                self.refetch_providers(kind);
            }
        }
    }

    /// Fetches one list again behind what is shown.
    fn refetch_providers(&mut self, kind: ProviderKind) {
        match kind {
            ProviderKind::Proxy => self.fetch_proxy_providers(true),
            ProviderKind::Rule => self.fetch_rule_providers(true),
        }
    }

    /// Applies a message from the background, unless it was sent for a
    /// controller the app has since switched away from: a reply, a frame
    /// or a stream going up or down there says nothing about the one in
    /// use now.
    pub fn receive(&mut self, envelope: Envelope) {
        if envelope.epoch == self.outbox.epoch {
            self.handle_msg(envelope.msg);
        } else {
            tracing::debug!(
                epoch = envelope.epoch,
                "dropped a message for a previous profile"
            );
        }
    }

    fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Version(result) => self.apply_version(result),
            // A reply older than one already applied would roll the mode back.
            Msg::Configs { seq, .. } if seq < self.applied_config => {}
            Msg::Configs {
                seq,
                refetch,
                result,
            } => self.apply_configs(seq, refetch, result),
            Msg::Changed { change, result } => self.apply_change(change, result),
            Msg::Proxies {
                seq,
                refetch,
                result,
                providers,
            } => self.on_proxies(seq, refetch, result, providers),
            Msg::Rules {
                seq,
                refetch,
                result,
            } => self.apply_rules(seq, refetch, result),
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
            Msg::Providers(msg) => self.on_providers(msg),
            Msg::Core(event) => self.on_core(event),
            Msg::Subscription(event) => self.on_subscription(event),
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
        let spinning = match self.page {
            Page::Proxies => !self.testing.is_empty(),
            Page::Providers => self.providers_spinning(),
            Page::Settings => !self.changing.is_empty(),
            Page::Subscriptions => self.subscriptions_view.working(),
            _ => false,
        };
        if self.is_loading() || spinning {
            self.spinner = self.spinner.wrapping_add(1);
            self.dirty = true;
        }
        self.auto_update_due(now);
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
    ///   repeat the startup requests, as a REST retry does, and reload the
    ///   pages loaded on demand that were. Streams that are already running
    ///   are left alone.
    fn on_stream(&mut self, stream: Stream, event: StreamEvent) {
        match event {
            StreamEvent::Connected => {
                self.links.up.insert(stream);
                if stream == Stream::Logs {
                    self.logs_view.link = LogLink::Live;
                }
                if std::mem::take(&mut self.links.outage) {
                    self.start();
                    self.reload_visited();
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
                    // What came in is history and stays.
                    Stream::Logs => self.logs_view.link = LogLink::Down,
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
            StreamEvent::Log(log) => {
                let entry = LogEntry::new(log, Zoned::now().time());
                self.logs_view.receive(entry);
            }
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
            exited: false,
        });
    }

    /// Whether the current page is waiting for data (and so shows a spinner).
    pub fn is_loading(&self) -> bool {
        if matches!(
            self.core_phase,
            Some(CorePhase::Idle | CorePhase::Starting(_))
        ) {
            return true;
        }
        match self.page {
            Page::Overview | Page::Settings => self.configs.is_none(),
            Page::Subscriptions => self.is_managed() && self.subscriptions_view.list.is_none(),
            Page::Proxies => self.proxies.is_none(),
            Page::Connections => self.connections_view.rows.is_none(),
            Page::Rules => self.rules_view.rows.is_none(),
            Page::Providers => self.providers_view.loading(),
            Page::Logs => self.logs_view.waiting(),
        }
    }

    /// Whether the kernel can have rule providers: anything but original
    /// Clash, which has none. Not known yet counts as yes.
    pub fn rule_providers_supported(&self) -> bool {
        self.core_kind() != Some(CoreKind::Clash)
    }

    /// Whether something on the providers page spins besides the page
    /// spinner: a list still loading, or a provider being worked on.
    fn providers_spinning(&self) -> bool {
        let view = &self.providers_view;
        !view.busy.is_empty()
            || view.proxies.is_none()
            || (view.rules.is_none() && self.rule_providers_supported())
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

    /// Takes in the reply to proxies load `seq`. One older than a reply
    /// already applied is dropped: it would roll the page back.
    fn on_proxies(
        &mut self,
        seq: u64,
        refetch: bool,
        result: Result<Proxies, ApiError>,
        providers: Result<Vec<ProxyProvider>, ApiError>,
    ) {
        if seq < self.applied_fetch {
            return;
        }
        self.applied_fetch = seq;
        self.note_failure(&result);
        match result {
            Ok(mut fresh) => {
                // Original Clash may not have the endpoint; the page then
                // shows what `/proxies` has, as it did before providers.
                match providers {
                    Ok(providers) => fresh.add_provider_nodes(providers),
                    Err(err) => tracing::debug!("proxy providers left out: {err}"),
                }
                self.apply_proxies(fresh, seq);
            }
            Err(_) if refetch && matches!(self.proxies, Some(Ok(_))) => {}
            Err(err) => self.proxies = Some(Err(err)),
        }
    }

    /// Swaps in a fresh load from fetch `seq`. Delay results measured after
    /// that fetch was issued survive it, and the node cursor stays on its
    /// node.
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
        let provider = match &self.proxies {
            Some(Ok(proxies)) => proxies.get(&name).and_then(|node| node.provider.clone()),
            _ => None,
        };
        let url = self.settings.delay_url.clone();
        let timeout = self.settings.delay_timeout_ms;
        self.dirty = true;
        self.spawn(move |client| async move {
            let result = client
                .proxy_delay(&name, provider.as_deref(), &url, timeout)
                .await;
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
        match (&self.core_phase, &self.version) {
            (Some(CorePhase::Failed(_) | CorePhase::Stopped), _) => Connection::Disconnected,
            (Some(CorePhase::Idle | CorePhase::Starting(_)), _) | (_, None) => {
                Connection::Connecting
            }
            (_, Some(Ok(_))) => Connection::Connected,
            (_, Some(Err(_))) => Connection::Disconnected,
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
        let entering = page != self.page;
        self.page = page;
        self.dirty = true;
        // A page waiting for the managed kernel loads once it is up.
        if entering && self.controller_up() {
            self.load_page(page);
        }
    }

    /// Loads what `page` loads when it is shown.
    fn load_page(&mut self, page: Page) {
        match page {
            // Shows what is there and refreshes behind it, without a spinner.
            Page::Proxies if self.proxies.is_some() => self.fetch_proxies(true),
            // Rules load on the first visit, not at startup.
            Page::Rules => match &self.rules_view.rows {
                Some(Ok(_)) => self.fetch_rules(true),
                // The last load failed: try again, with the spinner.
                Some(Err(_)) => {
                    self.rules_view.rows = None;
                    self.fetch_rules(false);
                }
                None if self.rule_fetches == 0 => self.fetch_rules(false),
                // The first load is still on its way.
                None => {}
            },
            // Like the rules, one list at a time.
            Page::Providers => self.load_providers(),
            // What is on screen may have changed outside tabby.
            Page::Settings if matches!(self.configs, Some(Ok(_))) => {
                self.fetch_configs(true);
            }
            _ => {}
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

/// Updates one provider, at the endpoint of its kind.
async fn update_one(client: &ApiClient, target: &ProviderRef) -> Result<(), ApiError> {
    match target.kind {
        ProviderKind::Proxy => client.update_proxy_provider(&target.name).await,
        ProviderKind::Rule => client.update_rule_provider(&target.name).await,
    }
}

/// How long a provider health check may take: twice the delay timeout plus
/// 30 s, 40 s by default. The kernel only replies once every node is
/// tested, at most 10 at a time, a dead one after the provider's own test
/// timeout (5 s unless configured), so this is well above the usual request
/// limit.
fn healthcheck_timeout(delay_timeout_ms: u32) -> Duration {
    Duration::from_millis(u64::from(delay_timeout_ms) * 2) + Duration::from_secs(30)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::{Connection, ConnectionSnapshot, Memory};
    use crate::core::{ManagedCore, Route, Step, testing};
    use std::sync::Arc;

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
    fn app_without_controller() -> (App, mpsc::Receiver<Envelope>) {
        app_at("http://127.0.0.1:1")
    }

    fn app_at(url: &str) -> (App, mpsc::Receiver<Envelope>) {
        let (tx, rx) = mpsc::channel(16);
        let overrides = crate::config::Overrides {
            url: Some(url.to_owned()),
            ..Default::default()
        };
        let settings = crate::config::Config::default()
            .resolve(&overrides)
            .unwrap();
        let app = launch(settings, tx, Profiles::default(), Picker::default());
        (app, rx)
    }

    /// An app on the controller of `settings`, as `main` starts one.
    fn launch(
        settings: Settings,
        tx: mpsc::Sender<Envelope>,
        profiles: Profiles,
        themes: Picker,
    ) -> App {
        let client = ApiClient::new(&settings.url, settings.secret.as_deref()).unwrap();
        let theme = themes::load(&settings.theme, themes.dir.as_deref()).unwrap();
        App::new(
            settings,
            theme,
            client,
            tx,
            Size::new(120, 40),
            profiles,
            themes,
        )
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

    /// A node whose history has `delays`, as the API sends it.
    fn node(name: &str, delays: &[u32]) -> serde_json::Value {
        let history: Vec<_> = delays
            .iter()
            .map(|delay| serde_json::json!({"delay": delay}))
            .collect();
        serde_json::json!({"name": name, "type": "Direct", "history": history})
    }

    /// A reply with one selector group `G` over `nodes`, each given with the
    /// delays of its history.
    fn reply(nodes: &[(&str, &[u32])]) -> Proxies {
        reply_using(nodes, &[])
    }

    /// Like [`reply`], with `G` also using the nodes `provided` by a proxy
    /// provider, after `nodes`. As in `/proxies`, only their names are
    /// there: [`provider`] has the nodes.
    fn reply_using(nodes: &[(&str, &[u32])], provided: &[(&str, &[u32])]) -> Proxies {
        let mut proxies = serde_json::Map::new();
        let names: Vec<&str> = nodes
            .iter()
            .chain(provided)
            .map(|(name, _)| *name)
            .collect();
        proxies.insert(
            "G".to_owned(),
            serde_json::json!({"name": "G", "type": "Selector", "all": names, "now": names[0]}),
        );
        for (name, delays) in nodes {
            proxies.insert((*name).to_owned(), node(name, delays));
        }
        serde_json::from_value(serde_json::json!({ "proxies": proxies })).unwrap()
    }

    /// The proxy provider `P` holding `nodes`.
    fn provider(nodes: &[(&str, &[u32])]) -> Vec<ProxyProvider> {
        let proxies = nodes
            .iter()
            .map(|(name, delays)| serde_json::from_value(node(name, delays)).unwrap())
            .collect();
        vec![ProxyProvider {
            name: "P".to_owned(),
            vehicle_type: "File".to_owned(),
            proxies,
            ..ProxyProvider::default()
        }]
    }

    /// The reply to load `seq` of a kernel without proxy providers.
    fn fetched(seq: u64, refetch: bool, proxies: Proxies) -> Msg {
        Msg::Proxies {
            seq,
            refetch,
            result: Ok(proxies),
            providers: Ok(vec![]),
        }
    }

    /// The reply to load `seq`: `G` over `nodes` from `/proxies` and the
    /// nodes `provided` by a proxy provider.
    fn loaded(
        seq: u64,
        refetch: bool,
        nodes: &[(&str, &[u32])],
        provided: &[(&str, &[u32])],
    ) -> Msg {
        Msg::Proxies {
            seq,
            refetch,
            result: Ok(reply_using(nodes, provided)),
            providers: Ok(provider(provided)),
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

    /// A rules reply: one rule per payload, all `DOMAIN-SUFFIX` to `DIRECT`.
    fn rules_of(payloads: &[&str]) -> Vec<Rule> {
        payloads
            .iter()
            .map(|payload| rule("DomainSuffix", payload, "DIRECT"))
            .collect()
    }

    fn rule(kind: &str, payload: &str, proxy: &str) -> Rule {
        Rule {
            kind: kind.to_owned(),
            payload: payload.to_owned(),
            proxy: proxy.to_owned(),
        }
    }

    /// Replies to the proxies load and the `/rules` fetch numbered `seq`:
    /// node `a` says `value`, provider node `p` `value + 1`, the rule's
    /// payload `value`.
    fn replies(seq: u64, refetch: bool, value: u32) -> [Msg; 2] {
        [
            loaded(seq, refetch, &[("a", &[value])], &[("p", &[value + 1])]),
            Msg::Rules {
                seq,
                refetch,
                result: Ok(rules_of(&[&value.to_string()])),
            },
        ]
    }

    /// What the two pages show: the delays of nodes `a` and `p`, the first
    /// rule's payload.
    fn shown(app: &App) -> (Option<Delay>, Option<Delay>, Option<String>) {
        let payload = app
            .rules_view
            .visible()
            .first()
            .map(|row| row.payload.clone());
        (delay_of(app, "a"), delay_of(app, "p"), payload)
    }

    #[tokio::test]
    async fn replies_older_than_the_applied_one_are_dropped() {
        let mut app = app_with(reply(&[("a", &[])]));
        for msg in replies(2, true, 70).into_iter().chain(replies(1, true, 90)) {
            app.handle_msg(msg);
        }
        assert_eq!(
            shown(&app),
            (
                Some(Delay::Millis(70)),
                Some(Delay::Millis(71)),
                Some("70".to_owned())
            )
        );
    }

    #[tokio::test]
    async fn failed_refetches_keep_the_data_but_failed_loads_do_not() {
        let mut app = app_with(reply(&[("a", &[])]));
        app.rules_view.apply(false, Ok(rules_of(&["a"])));
        let failed = |seq, refetch| {
            [
                Msg::Proxies {
                    seq,
                    refetch,
                    result: Err(http(500)),
                    providers: Ok(provider(&[("p", &[])])),
                },
                Msg::Rules {
                    seq,
                    refetch,
                    result: Err(http(500)),
                },
            ]
        };
        for msg in failed(1, true) {
            app.handle_msg(msg);
        }
        assert!(matches!(app.proxies, Some(Ok(_))));
        assert!(matches!(app.rules_view.rows, Some(Ok(_))));
        // Both say the same: it shows once.
        assert_eq!(app.toasts.len(), 1);
        for msg in failed(2, false) {
            app.handle_msg(msg);
        }
        assert!(matches!(app.proxies, Some(Err(_))));
        assert!(matches!(app.rules_view.rows, Some(Err(_))));
    }

    #[tokio::test]
    async fn without_provider_nodes_the_page_shows_what_proxies_has() {
        // Original Clash may have no `/providers/proxies` at all; neither a
        // load nor a refetch makes that an error page.
        let degraded = |seq, refetch| Msg::Proxies {
            seq,
            refetch,
            result: Ok(reply_using(&[("a", &[70])], &[("p", &[])])),
            providers: Err(ApiError::NotFound),
        };
        let (mut app, _rx) = app_without_controller();
        app.handle_msg(degraded(1, false));
        assert_eq!(delay_of(&app, "a"), Some(Delay::Millis(70)));
        app.handle_msg(loaded(2, true, &[("a", &[])], &[("p", &[40])]));
        assert_eq!(delay_of(&app, "p"), Some(Delay::Millis(40)));
        app.handle_msg(degraded(3, true));
        assert_eq!(delay_of(&app, "a"), Some(Delay::Millis(70)));
        assert_eq!(delay_of(&app, "p"), None);
        assert!(app.toasts.is_empty());
    }

    #[tokio::test]
    async fn rules_load_on_the_first_visit_and_refresh_behind_later_ones() {
        let (mut app, _rx) = app_without_controller();
        app.start();
        assert_eq!(app.rule_fetches, 0);
        app.update(Action::SelectPage(Page::Rules));
        assert!(app.is_loading());
        // Away and back before the first reply: it is not asked twice.
        app.update(Action::SelectPage(Page::Logs));
        app.update(Action::SelectPage(Page::Rules));
        assert_eq!(app.rule_fetches, 1);
        app.handle_msg(Msg::Rules {
            seq: 1,
            refetch: false,
            result: Ok(rules_of(&["a"])),
        });
        app.update(Action::SelectPage(Page::Logs));
        app.update(Action::SelectPage(Page::Rules));
        assert_eq!(app.rule_fetches, 2);
        assert!(!app.is_loading());
    }

    /// The numbers of the rules the table shows.
    fn rule_numbers(app: &App) -> Vec<usize> {
        app.rules_view
            .visible()
            .iter()
            .map(|row| row.number)
            .collect()
    }

    #[test]
    fn the_rule_filter_matches_type_payload_or_target_in_any_case() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Rules;
        app.rules_view.apply(
            false,
            Ok(vec![
                rule("DomainSuffix", "Example.com", "节点选择"),
                rule("IPCIDR", "10.0.0.0/8", "DIRECT"),
                rule("RuleSet", "广告", "REJECT"),
                rule("Match", "", "直连 A"),
            ]),
        );
        for (filter, numbers) in [
            // The type as shown, not as the kernel spells it.
            ("ip-c", &[2][..]),
            ("ipcidr", &[]),
            ("EXAMPLE", &[1]),
            ("direct", &[2]),
            ("直连", &[4]),
            ("广告", &[3]),
            ("e", &[1, 2, 3]),
        ] {
            type_filter(&mut app, filter);
            assert_eq!(rule_numbers(&app), numbers, "{filter}");
            app.update(Action::FilterClear);
        }
    }

    #[test]
    fn the_rule_cursor_goes_to_the_first_match_and_back_to_its_rule() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Rules;
        app.rules_view
            .apply(false, Ok(rules_of(&["a.cn", "b.com", "c.cn", "d.com"])));
        app.rules_view.cursor = 1;
        type_filter(&mut app, ".c");
        assert_eq!(app.rules_view.cursor, 0);
        // Narrowed further while the cursor was moved: back to the first.
        app.update(Action::Cursor(Move::Bottom));
        app.update(key('n'));
        assert_eq!(rule_numbers(&app), [1, 3]);
        assert_eq!(app.rules_view.cursor, 0);
        app.update(Action::Cursor(Move::Down));
        // Clearing it shows the rule among the others, still under the
        // cursor.
        app.update(Action::FilterClear);
        assert_eq!(app.rules_view.cursor, 2);
    }

    #[test]
    fn a_rules_refetch_keeps_the_cursor_on_its_number_or_the_end() {
        let mut view = RulesView::default();
        view.apply(false, Ok(rules_of(&["q1", "b", "q3", "q4"])));
        // Only in payloads: not in `DOMAIN-SUFFIX` nor in `DIRECT`.
        view.filter = Input::new("q".to_owned());
        // On number 4, third of the matches 1, 3, 4.
        view.cursor = 2;
        // Now 1 to 4 all match: number 4 is fourth.
        view.apply(true, Ok(rules_of(&["q1", "q2", "q3", "q4"])));
        assert_eq!(view.cursor, 3);
        // Gone, and the list shorter than where the cursor was: the end.
        view.apply(true, Ok(rules_of(&["q1", "q2"])));
        assert_eq!(view.cursor, 1);
        // A number that is there but filtered out: the same position.
        view.cursor = 0;
        view.apply(true, Ok(rules_of(&["b", "q2"])));
        assert_eq!((rule_number(&view), view.cursor), (2, 0));
    }

    fn rule_number(view: &RulesView) -> usize {
        view.visible()[view.cursor].number
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
        // The last four come from a proxy provider: their history counts
        // the same.
        let mut app = app_with(reply(&[("fast", &[])]));
        app.handle_msg(loaded(
            1,
            true,
            &[
                ("untested-1", &[]),
                ("failed-1", &[0]),
                ("slow-a", &[300]),
                ("timeout-1", &[]),
                ("fast", &[20]),
            ],
            &[
                ("failed-2", &[0]),
                ("slow-b", &[300]),
                ("timeout-2", &[]),
                ("untested-2", &[]),
            ],
        ));
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

    /// How many `/version`, `/configs`, `/proxies` and change (mode switch,
    /// setting) requests the app made, counted from the replies they leave
    /// on the channel. Dropping the app first stops its streams, so the
    /// channel closes once the requests are answered and nothing can still
    /// be on its way.
    async fn requests_made(
        app: App,
        mut rx: mpsc::Receiver<Envelope>,
    ) -> (usize, usize, usize, usize) {
        drop(app);
        let (mut versions, mut configs, mut proxies, mut changes) = (0, 0, 0, 0);
        while let Some(Envelope { msg, .. }) = rx.recv().await {
            match msg {
                Msg::Version(_) => versions += 1,
                Msg::Configs { .. } => configs += 1,
                Msg::Proxies { .. } => proxies += 1,
                Msg::Changed { .. } => changes += 1,
                _ => {}
            }
        }
        (versions, configs, proxies, changes)
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
        for page in [
            Page::Proxies,
            Page::Providers,
            Page::Rules,
            Page::Connections,
            Page::Logs,
        ] {
            let (mut app, _rx) = app_without_controller();
            app.page = page;
            app.logs_view.receive(log_entry(LogLevel::Info, "kept"));
            app.update(press('/', page, app.typing()).unwrap());
            for key in ['p', 's', 'S', 'm', '5', 'l', 'c', 'u', 'U', 'h', 'P'] {
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
            assert_eq!(app.filter().unwrap().value(), "psSm5lcuUhP", "{page:?}");
            assert_eq!(app.page, page);
            assert!(!app.connections_view.paused);
            let logs = &app.logs_view;
            assert!(!logs.paused && logs.level == LogLevel::Info && logs.entries.len() == 1);
            assert_eq!(app.connections_view.sort, ConnectionSort::default());
            assert_eq!(app.proxies_view.sort, Sort::Default);
            assert!(app.providers_view.busy.is_empty() && app.overlay.is_none());
            // `m` would have complained that there is no mode yet.
            assert!(app.toasts.is_empty());
        }
    }

    /// The reply to the mode switch `m` sent.
    fn mode_set(result: Result<(), ApiError>) -> Msg {
        Msg::Changed {
            change: Change::Patch(Patch::Mode(Mode::Global)),
            result,
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
        app.handle_msg(mode_set(Ok(())));
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
        app.handle_msg(mode_set(Err(http(500))));
        assert_eq!(app.toasts.len(), 1);
        assert!(matches!(app.configs, Some(Ok(_))));

        app.update(Action::CycleMode);
        app.handle_msg(mode_set(Ok(())));
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
            assert!(matches!(
                rx.recv().await.map(|envelope| envelope.msg),
                Some(Msg::Changed { result: Ok(()), .. })
            ));

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
        app.page = Page::Connections;
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
    async fn closes_sent(app: App, mut rx: mpsc::Receiver<Envelope>) -> Vec<Sent> {
        drop(app);
        let mut sent = vec![];
        while let Some(Envelope { msg, .. }) = rx.recv().await {
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
    fn three_connections() -> (App, mpsc::Receiver<Envelope>) {
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

    fn log_entry(level: LogLevel, message: &str) -> LogEntry {
        let log = Log {
            kind: LogKind::Level(level),
            payload: message.to_owned(),
        };
        LogEntry::new(log, civil::time(12, 0, 0, 0))
    }

    /// Feeds lines `0`, `1` … up to `count`, each one row long.
    fn feed_logs(view: &mut LogsView, from: usize, count: usize) {
        for n in from..from + count {
            view.receive(log_entry(LogLevel::Info, &n.to_string()));
        }
    }

    /// 20 columns of message, 4 rows.
    const FIT: Fit = Fit {
        width: 20,
        height: 4,
    };

    /// The screen rows of `view`, as text.
    fn screen(view: &LogsView) -> Vec<String> {
        view.page(FIT)
            .into_iter()
            .map(|(_, _, text)| text)
            .collect()
    }

    #[test]
    fn the_log_keeps_the_newest_2000_lines() {
        let mut view = LogsView::default();
        feed_logs(&mut view, 0, LOG_CAPACITY + 5);
        assert_eq!(view.entries.len(), LOG_CAPACITY);
        assert_eq!(view.entries.front().unwrap().message, "5");
        assert_eq!(view.entries.back().unwrap().message, "2004");
    }

    #[test]
    fn a_following_view_stays_at_the_bottom() {
        let mut view = LogsView::default();
        feed_logs(&mut view, 0, 10);
        assert_eq!(screen(&view), ["6", "7", "8", "9"]);
        feed_logs(&mut view, 10, LOG_CAPACITY);
        assert_eq!(view.top, None);
        assert_eq!(screen(&view), ["2006", "2007", "2008", "2009"]);
    }

    #[test]
    fn a_view_scrolled_up_keeps_its_lines_while_old_ones_are_dropped() {
        let mut view = LogsView::default();
        feed_logs(&mut view, 0, LOG_CAPACITY);
        view.scroll(Move::Up, FIT);
        assert_eq!(screen(&view), ["1995", "1996", "1997", "1998"]);
        // Full: each new line pushes out the oldest, and the view moves
        // its spot along instead of drifting down the log.
        feed_logs(&mut view, LOG_CAPACITY, 10);
        assert_eq!(screen(&view), ["1995", "1996", "1997", "1998"]);
        assert_eq!(view.top.unwrap().entry, 1985);

        // At the very top its line goes, and the oldest left takes over.
        view.scroll(Move::Top, FIT);
        assert_eq!(screen(&view), ["10", "11", "12", "13"]);
        feed_logs(&mut view, 2010, 1);
        assert_eq!(screen(&view), ["11", "12", "13", "14"]);
    }

    #[test]
    fn reaching_the_bottom_or_g_follows_again() {
        let mut view = LogsView::default();
        feed_logs(&mut view, 0, 10);
        view.scroll(Move::Up, FIT);
        view.scroll(Move::Up, FIT);
        assert_eq!(screen(&view), ["4", "5", "6", "7"]);
        view.scroll(Move::Down, FIT);
        assert!(view.top.is_some());
        view.scroll(Move::Down, FIT);
        assert_eq!(view.top, None);
        feed_logs(&mut view, 10, 1);
        assert_eq!(screen(&view), ["7", "8", "9", "10"]);

        view.scroll(Move::HalfUp, FIT);
        feed_logs(&mut view, 11, 1);
        assert_eq!(screen(&view), ["5", "6", "7", "8"]);
        view.scroll(Move::Bottom, FIT);
        assert_eq!(view.top, None);
        assert_eq!(screen(&view), ["8", "9", "10", "11"]);
    }

    #[test]
    fn a_log_that_fits_on_screen_does_not_scroll() {
        let mut view = LogsView::default();
        feed_logs(&mut view, 0, 3);
        view.scroll(Move::Up, FIT);
        view.scroll(Move::Top, FIT);
        assert_eq!(view.top, None);
        assert_eq!(screen(&view), ["0", "1", "2"]);
    }

    #[test]
    fn scrolling_goes_one_screen_row_at_a_time() {
        let mut view = LogsView::default();
        feed_logs(&mut view, 0, 2);
        // 45 columns: three rows at 20.
        view.receive(log_entry(LogLevel::Warning, &"x".repeat(45)));
        let rows: Vec<(usize, String)> = view
            .page(FIT)
            .into_iter()
            .map(|(_, row, text)| (row, text))
            .collect();
        let long = |n| "x".repeat(n);
        assert_eq!(
            rows,
            [
                (0, "1".to_owned()),
                (0, long(20)),
                (1, long(20)),
                (2, long(5))
            ]
        );
        view.scroll(Move::Up, FIT);
        assert_eq!(screen(&view), ["0", "1", &long(20), &long(20)]);
        // A narrower terminal keeps the spot, its row within the message.
        let narrow = Fit { width: 10, ..FIT };
        view.scroll(Move::Up, narrow);
        view.scroll(Move::Down, narrow);
        view.scroll(Move::Down, narrow);
        assert_eq!(view.top, Some(LogSpot { entry: 2, row: 0 }));
    }

    #[test]
    fn the_scroll_extent_counts_wrapped_rows_and_follows_at_the_bottom() {
        let mut view = LogsView::default();
        feed_logs(&mut view, 0, 5);
        // 45 columns: three rows at 20, five at 10.
        view.receive(log_entry(LogLevel::Warning, &"x".repeat(45)));
        feed_logs(&mut view, 5, 4);
        assert_eq!(view.extent(FIT), (8, 12));
        view.scroll(Move::Top, FIT);
        assert_eq!(view.extent(FIT), (0, 12));
        view.scroll(Move::Down, FIT);
        view.scroll(Move::HalfDown, FIT);
        assert_eq!(view.top, Some(LogSpot { entry: 3, row: 0 }));
        assert_eq!(view.extent(FIT), (3, 12));
        // Into the long line: its rows count one by one.
        view.scroll(Move::HalfDown, FIT);
        assert_eq!(view.top, Some(LogSpot { entry: 5, row: 0 }));
        view.scroll(Move::Down, FIT);
        assert_eq!(view.extent(FIT), (6, 12));
        // A narrower column wraps again, keeping the spot.
        let narrow = Fit { width: 10, ..FIT };
        assert_eq!(view.extent(narrow), (6, 14));

        // The filter leaves out what it hides.
        view.filter = Input::new("x".to_owned());
        assert_eq!(view.extent(FIT), (0, 3));
        view.filter = Input::new("7".to_owned());
        assert_eq!(view.extent(FIT), (0, 1));
    }

    #[test]
    fn a_long_log_shows_a_scrollbar_that_tracks_the_view() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Logs;
        app.logs_view.link = LogLink::Live;
        feed_logs(&mut app.logs_view, 0, 500);
        // On the right border: just above the bottom corner while it
        // follows, just under the top corner at the top.
        let following = thumb_rows(&app);
        assert!(row_ends_with(&app, *following.last().unwrap() + 1, '╯'));
        app.update(Action::Cursor(Move::Top));
        let top = thumb_rows(&app);
        assert_eq!(top.len(), following.len());
        assert!(row_ends_with(&app, top[0] - 1, '╮'));

        // Nothing to scroll, no thumb.
        app.update(press('c', Page::Logs, false).unwrap());
        feed_logs(&mut app.logs_view, 0, 3);
        assert_eq!(thumb_rows(&app), Vec::<usize>::new());
    }

    #[test]
    fn a_pause_freezes_the_screen_and_resuming_adds_what_came_in_order() {
        let mut view = LogsView::default();
        feed_logs(&mut view, 0, 4);
        view.toggle_pause();
        feed_logs(&mut view, 4, 3);
        assert_eq!(view.entries.len(), 4);
        assert_eq!(view.arrived(), 3);
        assert_eq!(screen(&view), ["0", "1", "2", "3"]);
        view.toggle_pause();
        assert_eq!(view.arrived(), 0);
        let messages: Vec<&str> = view.entries.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages, ["0", "1", "2", "3", "4", "5", "6"]);
    }

    #[test]
    fn the_pause_queue_keeps_the_newest_2000_and_counts_every_arrival() {
        let mut view = LogsView::default();
        view.toggle_pause();
        feed_logs(&mut view, 0, LOG_CAPACITY + 3);
        assert_eq!(view.arrived(), LOG_CAPACITY + 3);
        assert!(view.entries.is_empty());
        view.toggle_pause();
        assert_eq!(view.entries.len(), LOG_CAPACITY);
        assert_eq!(view.entries.front().unwrap().message, "3");
        assert_eq!(view.entries.back().unwrap().message, "2002");
    }

    #[test]
    fn clearing_drops_the_shown_and_the_queued_lines() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Logs;
        feed_logs(&mut app.logs_view, 0, 10);
        app.update(Action::Cursor(Move::Up));
        app.update(press('p', Page::Logs, false).unwrap());
        feed_logs(&mut app.logs_view, 10, 2);
        app.update(press('c', Page::Logs, false).unwrap());
        let view = &app.logs_view;
        assert!(view.entries.is_empty() && view.arrived() == 0 && view.top.is_none());
        assert!(view.paused);
    }

    #[test]
    fn the_filter_only_hides_lines() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Logs;
        for message in ["a.example ok", "b.example", "其他 A.EXAMPLE"] {
            app.logs_view.receive(log_entry(LogLevel::Info, message));
        }
        app.update(Action::StartFilter);
        for key in "a.ex".chars() {
            app.update(Action::FilterInput(KeyEvent::new(
                KeyCode::Char(key),
                KeyModifiers::NONE,
            )));
        }
        // Lines that do not match still come in, and stay for later.
        app.logs_view
            .receive(log_entry(LogLevel::Info, "c.example"));
        assert_eq!(app.logs_view.shown_count(), 2);
        assert_eq!(app.logs_view.entries.len(), 4);
        assert_eq!(screen(&app.logs_view), ["a.example ok", "其他 A.EXAMPLE"]);
        app.update(Action::FilterClear);
        assert_eq!(app.logs_view.shown_count(), 4);
    }

    #[tokio::test]
    async fn a_new_log_level_is_no_outage() {
        let (mut app, rx) = app_without_controller();
        app.page = Page::Logs;
        for stream in [Stream::Traffic, Stream::Logs, Stream::Connections] {
            connect(&mut app, stream);
        }
        stream_event(
            &mut app,
            Stream::Traffic,
            StreamEvent::Traffic(Traffic::default()),
        );
        app.logs_view.receive(log_entry(LogLevel::Info, "kept"));
        app.update(press('l', Page::Logs, false).unwrap());
        assert_eq!(app.logs_view.level, LogLevel::Warning);
        // The resubscribed stream reports in by itself.
        connect(&mut app, Stream::Logs);
        assert!(app.reconnect.is_none() && !app.links.outage);
        assert!(app.logs_view.link == LogLink::Live && app.logs_view.entries.len() == 1);
        assert!(app.traffic.is_some());
        assert_eq!(requests_made(app, rx).await, (0, 0, 0, 0));
    }

    #[tokio::test]
    async fn a_new_log_level_is_no_outage_even_as_the_only_stream_up() {
        let (mut app, rx) = app_without_controller();
        app.page = Page::Logs;
        connect(&mut app, Stream::Logs);
        app.update(press('l', Page::Logs, false).unwrap());
        connect(&mut app, Stream::Logs);
        assert_eq!(requests_made(app, rx).await, (0, 0, 0, 0));
    }

    #[tokio::test]
    async fn the_title_is_not_live_until_the_resubscribed_log_is_in() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Logs;
        let status = |app: &App| logs::status(app).content.into_owned();
        connect(&mut app, Stream::Logs);
        assert_eq!(status(&app).trim(), "● 实时");
        // The new connection may never get in, and would then never report
        // going down either.
        app.update(press('l', Page::Logs, false).unwrap());
        assert_eq!(status(&app).trim(), "◌ 连接中");
        // Paused, it still does not pass for connected.
        app.update(press('p', Page::Logs, false).unwrap());
        assert_eq!(status(&app).trim(), "◌ 连接中");
        app.update(press('p', Page::Logs, false).unwrap());
        // Nor does it hide the panel behind a spinner.
        assert!(!app.is_loading());
        connect(&mut app, Stream::Logs);
        assert_eq!(status(&app).trim(), "● 实时");
        // A stream that was down stays so until it is in.
        disconnect(&mut app, Stream::Logs);
        app.update(press('l', Page::Logs, false).unwrap());
        assert_eq!(status(&app).trim(), "○ 未连接");
        connect(&mut app, Stream::Logs);
        assert_eq!(status(&app).trim(), "● 实时");
    }

    #[tokio::test]
    async fn the_log_outlives_its_stream() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Logs;
        assert!(app.is_loading());
        connect(&mut app, Stream::Logs);
        assert!(!app.is_loading());
        let log = Log {
            kind: LogKind::Other("silent".to_owned()),
            payload: "a\tb\u{1b}[0m".to_owned(),
        };
        stream_event(&mut app, Stream::Logs, StreamEvent::Log(log));
        disconnect(&mut app, Stream::Logs);
        assert!(app.logs_view.link == LogLink::Down && !app.is_loading());
        // Kept, the tab a space and the escape dropped.
        assert_eq!(app.logs_view.entries[0].message, "a b[0m");
    }

    fn kernel(meta: bool) -> Version {
        Version {
            version: "v1".to_owned(),
            meta,
        }
    }

    /// On the providers page of a mihomo kernel, with these proxy and rule
    /// providers loaded.
    fn providers_app(proxies: &[&str], rules: &[&str]) -> (App, mpsc::Receiver<Envelope>) {
        let (mut app, rx) = app_without_controller();
        app.page = Page::Providers;
        app.version = Some(Ok(kernel(true)));
        let view = &mut app.providers_view;
        view.proxies = Some(Ok(proxies
            .iter()
            .map(|name| ProxyProvider {
                name: (*name).to_owned(),
                vehicle_type: "HTTP".to_owned(),
                ..ProxyProvider::default()
            })
            .collect()));
        view.rules = Some(Ok(rules
            .iter()
            .map(|name| RuleProvider {
                name: (*name).to_owned(),
                vehicle_type: "File".to_owned(),
                ..RuleProvider::default()
            })
            .collect()));
        (app, rx)
    }

    fn proxy_ref(name: &str) -> ProviderRef {
        ProviderRef {
            kind: ProviderKind::Proxy,
            name: name.to_owned(),
        }
    }

    fn rule_ref(name: &str) -> ProviderRef {
        ProviderRef {
            kind: ProviderKind::Rule,
            name: name.to_owned(),
        }
    }

    /// A request the providers page made, told by its reply.
    #[derive(Debug, PartialEq, Eq)]
    enum ProviderSent {
        /// A list fetch, and its `refetch` flag.
        Proxies(bool),
        Rules(bool),
        /// `/rules`, for the rules page.
        RulePage(bool),
        Done(ProviderRef, Work),
        Updated(Vec<ProviderRef>),
    }

    /// The provider requests the app made, from the replies they leave on
    /// the channel, in no particular order.
    async fn providers_sent(app: App, mut rx: mpsc::Receiver<Envelope>) -> Vec<ProviderSent> {
        drop(app);
        let mut sent = vec![];
        while let Some(Envelope { msg, .. }) = rx.recv().await {
            sent.push(match msg {
                Msg::Providers(ProviderMsg::Proxies { refetch, .. }) => {
                    ProviderSent::Proxies(refetch)
                }
                Msg::Providers(ProviderMsg::Rules { refetch, .. }) => ProviderSent::Rules(refetch),
                Msg::Rules { refetch, .. } => ProviderSent::RulePage(refetch),
                Msg::Providers(ProviderMsg::Done { target, work, .. }) => {
                    ProviderSent::Done(target, work)
                }
                Msg::Providers(ProviderMsg::Updated { results }) => {
                    ProviderSent::Updated(results.into_iter().map(|(target, _)| target).collect())
                }
                _ => continue,
            });
        }
        sent
    }

    #[tokio::test]
    async fn a_provider_in_the_works_is_not_sent_twice() {
        let (mut app, rx) = providers_app(&["a"], &["r"]);
        for key in ['u', 'u', 'h'] {
            app.update(press(key, Page::Providers, false).unwrap());
        }
        assert_eq!(app.providers_view.busy[&proxy_ref("a")], Work::Updating);
        // On the rule provider, `h` has nothing to check.
        app.update(Action::Cursor(Move::Down));
        app.update(press('h', Page::Providers, false).unwrap());
        assert_eq!(app.toasts[0].text, "规则集合不支持健康检查");
        app.update(press('u', Page::Providers, false).unwrap());
        assert_eq!(providers_sent(app, rx).await.len(), 2, "one update of each");
    }

    #[tokio::test]
    async fn finished_work_frees_the_provider_and_refetches_its_list() {
        let (mut app, _rx) = providers_app(&["a"], &["r"]);
        app.providers_view
            .busy
            .insert(rule_ref("r"), Work::Updating);
        app.handle_msg(Msg::Providers(ProviderMsg::Done {
            target: rule_ref("r"),
            work: Work::Updating,
            result: Ok(()),
        }));
        assert!(app.providers_view.busy.is_empty());
        assert_eq!(app.toasts[0].text, "已更新 r");
        assert_eq!(
            (app.proxy_provider_fetches, app.rule_provider_fetches),
            (0, 1)
        );

        // A health check changes delays: `/proxies` too, once it is loaded.
        let check = |result| {
            Msg::Providers(ProviderMsg::Done {
                target: proxy_ref("a"),
                work: Work::Checking,
                result,
            })
        };
        app.providers_view
            .busy
            .insert(proxy_ref("a"), Work::Checking);
        app.handle_msg(check(Ok(())));
        assert_eq!((app.proxy_provider_fetches, app.fetches), (1, 0));
        app.proxies = Some(Ok(reply(&[("n", &[])])));
        app.handle_msg(check(Ok(())));
        assert_eq!((app.proxy_provider_fetches, app.fetches), (2, 1));
        assert_eq!(app.toasts.back().unwrap().text, "a 健康检查完成");

        // A failure frees it as well, and only says why.
        app.providers_view
            .busy
            .insert(proxy_ref("a"), Work::Checking);
        app.handle_msg(check(Err(ApiError::Timeout)));
        assert!(app.providers_view.busy.is_empty());
        assert_eq!((app.proxy_provider_fetches, app.fetches), (2, 1));
        let toast = app.toasts.back().unwrap();
        assert_eq!(
            (toast.kind, toast.text.as_str()),
            (ToastKind::Error, "健康检查失败：请求超时")
        );
    }

    #[tokio::test]
    async fn capital_u_under_a_filter_updates_only_what_is_shown() {
        let (mut app, rx) = providers_app(&["hk-1", "us-1", "hk-2"], &["hk-rules", "ads"]);
        app.update(press('U', Page::Providers, false).unwrap());
        assert_eq!(dialog_body(&app), "更新全部 5 个 provider？");
        app.update(Action::Cancel);

        type_filter(&mut app, "HK");
        // One of them is already being updated: it is not sent again.
        app.providers_view
            .busy
            .insert(proxy_ref("hk-2"), Work::Updating);
        app.update(press('U', Page::Providers, false).unwrap());
        assert_eq!(dialog_body(&app), "更新匹配「HK」的 3 个 provider？");
        app.update(Action::Accept);
        assert_eq!(app.providers_view.busy.len(), 3);
        assert_eq!(
            providers_sent(app, rx).await,
            [ProviderSent::Updated(vec![
                proxy_ref("hk-1"),
                rule_ref("hk-rules")
            ])]
        );
    }

    #[tokio::test]
    async fn the_replies_to_capital_u_come_as_one_toast() {
        let (mut app, _rx) = providers_app(&["a", "b"], &["r"]);
        let all = [proxy_ref("a"), proxy_ref("b"), rule_ref("r")];
        for target in &all {
            app.providers_view
                .busy
                .insert(target.clone(), Work::Updating);
        }
        app.handle_msg(Msg::Providers(ProviderMsg::Updated {
            results: vec![
                (all[0].clone(), Ok(())),
                (all[1].clone(), Ok(())),
                (all[2].clone(), Err(http(502))),
            ],
        }));
        assert!(app.providers_view.busy.is_empty());
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(
            app.toasts[0].text,
            "成功 2，失败 1：控制器返回错误（HTTP 502）"
        );
        // Only the list that changed is fetched again.
        assert_eq!(
            (app.proxy_provider_fetches, app.rule_provider_fetches),
            (1, 0)
        );

        app.handle_msg(Msg::Providers(ProviderMsg::Updated {
            results: vec![(all[2].clone(), Ok(()))],
        }));
        assert_eq!(app.toasts[1].text, "已更新 1 个 provider");
        assert_eq!(app.toasts[1].kind, ToastKind::Success);
    }

    #[tokio::test]
    async fn rule_providers_are_only_asked_of_mihomo() {
        // Original Clash: no `/providers/rules` at all.
        let (mut app, rx) = app_without_controller();
        app.version = Some(Ok(kernel(false)));
        app.update(Action::SelectPage(Page::Providers));
        app.update(Action::Refresh);
        let sent = providers_sent(app, rx).await;
        assert!(
            !sent.iter().any(|s| matches!(s, ProviderSent::Rules(_))),
            "{sent:?}"
        );
        assert_eq!(sent.len(), 2);

        // Not known yet: the fetch waits for `/version`, then goes once.
        let (mut app, rx) = app_without_controller();
        app.update(Action::SelectPage(Page::Providers));
        // Away and back before any reply: nothing is asked twice.
        app.update(Action::SelectPage(Page::Logs));
        app.update(Action::SelectPage(Page::Providers));
        assert_eq!(
            (app.proxy_provider_fetches, app.rule_provider_fetches),
            (1, 0)
        );
        app.handle_msg(Msg::Version(Ok(kernel(true))));
        assert_eq!(app.rule_provider_fetches, 1);
        assert_eq!(
            providers_sent(app, rx).await,
            [ProviderSent::Proxies(false), ProviderSent::Rules(false)]
        );
    }

    #[tokio::test]
    async fn providers_load_on_the_first_visit_and_refresh_behind_later_ones() {
        let (mut app, _rx) = app_without_controller();
        app.version = Some(Ok(kernel(true)));
        app.update(Action::SelectPage(Page::Providers));
        assert!(app.is_loading());
        let proxies = |seq, result| {
            Msg::Providers(ProviderMsg::Proxies {
                seq,
                refetch: false,
                result,
            })
        };
        app.handle_msg(proxies(1, Ok(vec![])));
        // One list in: the panel shows, the other list still loading.
        assert!(!app.is_loading());
        app.handle_msg(Msg::Providers(ProviderMsg::Rules {
            seq: 1,
            refetch: false,
            result: Err(http(500)),
        }));
        app.update(Action::SelectPage(Page::Logs));
        app.update(Action::SelectPage(Page::Providers));
        // Behind the list on screen; the failed one again from scratch.
        assert_eq!(
            (app.proxy_provider_fetches, app.rule_provider_fetches),
            (2, 2)
        );
        assert!(app.providers_view.rules.is_none());
        // A late reply to the first fetch does not roll the list back.
        app.handle_msg(proxies(2, Ok(vec![ProxyProvider::default()])));
        app.handle_msg(proxies(1, Ok(vec![])));
        assert!(matches!(&app.providers_view.proxies, Some(Ok(list)) if list.len() == 1));
    }

    #[tokio::test]
    async fn the_cursor_runs_through_both_lists_and_stays_on_its_provider() {
        let (mut app, _rx) = providers_app(&["a", "b"], &["c"]);
        app.update(Action::Cursor(Move::Bottom));
        assert_eq!(
            app.providers_view.selected().unwrap().target(),
            rule_ref("c")
        );
        // A provider added above it: the cursor stays on `c`.
        app.handle_msg(Msg::Providers(ProviderMsg::Proxies {
            seq: 1,
            refetch: true,
            result: Ok(["a", "b", "b2"]
                .iter()
                .map(|name| ProxyProvider {
                    name: (*name).to_owned(),
                    ..ProxyProvider::default()
                })
                .collect()),
        }));
        assert_eq!(app.providers_view.cursor, 3);
        assert_eq!(
            app.providers_view.selected().unwrap().target(),
            rule_ref("c")
        );

        // The rule providers answering first: once the proxy providers fill
        // in above, the cursor is at the top, not dragged down with them.
        let (mut app, _rx) = providers_app(&[], &[]);
        app.providers_view = ProvidersView::default();
        app.handle_msg(Msg::Providers(ProviderMsg::Rules {
            seq: 1,
            refetch: false,
            result: Ok(vec![RuleProvider {
                name: "c".to_owned(),
                ..RuleProvider::default()
            }]),
        }));
        app.handle_msg(Msg::Providers(ProviderMsg::Proxies {
            seq: 1,
            refetch: false,
            result: Ok(vec![ProxyProvider {
                name: "a".to_owned(),
                ..ProxyProvider::default()
            }]),
        }));
        assert_eq!(
            app.providers_view.selected().unwrap().target(),
            proxy_ref("a")
        );
    }

    #[tokio::test]
    async fn the_first_stream_back_reloads_the_pages_that_were_loaded() {
        let outage = |app: &mut App| {
            connect(app, Stream::Traffic);
            disconnect(app, Stream::Traffic);
            connect(app, Stream::Traffic);
        };
        // Never visited: not asked for.
        let (mut app, rx) = app_without_controller();
        app.version = Some(Ok(kernel(true)));
        outage(&mut app);
        assert_eq!(providers_sent(app, rx).await, Vec::new());

        // Rules failed, proxy providers on screen, rule providers failed:
        // the failed ones without `refetch`, so their reply replaces the
        // error; the one on screen behind it.
        let (mut app, rx) = providers_app(&["a"], &[]);
        app.rules_view.rows = Some(Err(http(500)));
        app.providers_view.rules = Some(Err(http(500)));
        outage(&mut app);
        let mut sent = providers_sent(app, rx).await;
        sent.sort_by_key(|s| format!("{s:?}"));
        assert_eq!(
            sent,
            [
                ProviderSent::Proxies(true),
                ProviderSent::RulePage(false),
                ProviderSent::Rules(false),
            ]
        );
    }

    const MIHOMO_CONFIGS: &str = include_str!("../tests/fixtures/mihomo_configs.json");

    /// On the settings page of a kernel (mihomo when `meta`) whose
    /// `/configs` replied `configs`.
    fn settings_app(url: &str, meta: bool, configs: &str) -> (App, mpsc::Receiver<Envelope>) {
        let (mut app, rx) = app_at(url);
        app.page = Page::Settings;
        app.version = Some(Ok(kernel(meta)));
        app.configs = Some(Ok(serde_json::from_str(configs).unwrap()));
        (app, rx)
    }

    fn select(app: &mut App, setting: Setting) {
        app.settings_view.cursor = Setting::ALL.iter().position(|s| *s == setting).unwrap();
    }

    /// Presses `code` as the event loop would.
    fn key_press(app: &mut App, code: KeyCode) {
        let event = Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        if let Some(action) = action_for(&event, app.page, app.typing()) {
            app.update(action);
        }
    }

    fn type_text(app: &mut App, text: &str) {
        for ch in text.chars() {
            key_press(app, KeyCode::Char(ch));
        }
    }

    /// Opens the port under the cursor, unless it is open, and types `text`
    /// over what it held.
    fn retype_port(app: &mut App, text: &str) {
        if app.settings_view.editing.is_none() {
            key_press(app, KeyCode::Enter);
        }
        for _ in 0..5 {
            key_press(app, KeyCode::Backspace);
        }
        type_text(app, text);
    }

    fn port_box(app: &App) -> Option<&str> {
        let (_, input) = app.settings_view.editing.as_ref()?;
        Some(input.value())
    }

    #[test]
    fn a_typed_port_is_a_number_from_0_to_65535_or_nothing() {
        assert_eq!(parse_port(""), Ok(None));
        assert_eq!(parse_port("0"), Ok(Some(0)));
        assert_eq!(parse_port("65535"), Ok(Some(65535)));
        assert_eq!(parse_port("0017890"), Ok(Some(17890)));
        for refused in [
            "65536",
            "70000",
            "99999999999999999999999",
            "12a",
            "-1",
            " 80",
        ] {
            assert!(parse_port(refused).is_err(), "{refused}");
        }
    }

    #[tokio::test]
    async fn the_port_box_takes_digits_and_sends_only_a_new_valid_port() {
        let (mut app, rx) = settings_app("http://127.0.0.1:1", true, MIHOMO_CONFIGS);
        select(&mut app, Setting::Port(Port::Mixed));
        key_press(&mut app, KeyCode::Enter);
        assert_eq!(port_box(&app), Some("17890"));
        // It has the keyboard: `m`, `q` and `7` neither switch modes, quit
        // nor change pages, and letters are not taken.
        retype_port(&mut app, "7a0m0q00");
        assert_eq!(port_box(&app), Some("70000"));
        assert!(!app.should_quit() && app.page == Page::Settings);
        // Out of range: refused, and left open to fix.
        key_press(&mut app, KeyCode::Enter);
        assert_eq!(port_box(&app), Some("70000"));
        assert_eq!(app.toasts.back().unwrap().text, "端口须在 0–65535 之间");
        key_press(&mut app, KeyCode::Esc);
        assert!(port_box(&app).is_none() && !app.typing());
        // Nothing typed, or the port it already is: closed, nothing sent.
        retype_port(&mut app, "");
        key_press(&mut app, KeyCode::Enter);
        retype_port(&mut app, "17890");
        key_press(&mut app, KeyCode::Enter);
        assert!(port_box(&app).is_none());
        assert_eq!(app.toasts.len(), 1);
        // 0 turns it off.
        retype_port(&mut app, "0");
        key_press(&mut app, KeyCode::Enter);
        assert!(app.is_changing(Setting::Port(Port::Mixed)));
        assert_eq!(requests_made(app, rx).await, (0, 0, 0, 1));
    }

    #[tokio::test]
    async fn what_the_kernel_lacks_is_refused_without_a_request() {
        // Original Clash, without `ipv6` and a SOCKS port besides.
        let clash =
            r#"{"port":0,"mixed-port":7890,"allow-lan":false,"mode":"rule","log-level":"info"}"#;
        let (mut app, rx) = settings_app("http://127.0.0.1:1", false, clash);
        for setting in [
            Setting::Ipv6,
            Setting::Port(Port::Socks),
            Setting::Tun,
            Setting::FlushFakeIp,
            Setting::UpdateGeo,
            Setting::Restart,
            Setting::Upgrade,
        ] {
            select(&mut app, setting);
            key_press(&mut app, KeyCode::Enter);
            assert!(app.overlay.is_none() && !app.typing(), "{setting:?}");
            assert_eq!(
                app.toasts.back().unwrap().text,
                "需要 mihomo",
                "{setting:?}"
            );
        }
        // What Clash does have works.
        select(&mut app, Setting::AllowLan);
        key_press(&mut app, KeyCode::Enter);
        assert_eq!(requests_made(app, rx).await, (0, 0, 0, 1));

        // mihomo leaving a value out: unknown rather than off, and not sent.
        let (mut app, rx) = settings_app("http://127.0.0.1:1", true, r#"{"mode":"rule"}"#);
        select(&mut app, Setting::Ipv6);
        key_press(&mut app, KeyCode::Enter);
        assert_eq!(app.toasts.back().unwrap().text, "内核没有提供这一项");
        assert_eq!(requests_made(app, rx).await, (0, 0, 0, 0));
    }

    #[tokio::test]
    async fn each_setting_sends_its_request_and_the_dangerous_ones_ask_first() {
        use serde_json::{Value, json};
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        // allow-lan off, log-level info, ipv6 on, mixed-port 17890, TUN off.
        let (mut app, mut rx) = settings_app(&server.uri(), true, MIHOMO_CONFIGS);
        let expected = [
            (
                Setting::AllowLan,
                "PATCH",
                "/configs",
                json!({"allow-lan": true}),
            ),
            (
                Setting::LogLevel,
                "PATCH",
                "/configs",
                json!({"log-level": "debug"}),
            ),
            (Setting::Ipv6, "PATCH", "/configs", json!({"ipv6": false})),
            (
                Setting::Port(Port::Mixed),
                "PATCH",
                "/configs",
                json!({"mixed-port": 17891}),
            ),
            (
                Setting::Tun,
                "PATCH",
                "/configs",
                json!({"tun": {"enable": true}}),
            ),
            (
                Setting::Reload,
                "PUT",
                "/configs?force=true",
                json!({"path": "", "payload": ""}),
            ),
            (
                Setting::FlushFakeIp,
                "POST",
                "/cache/fakeip/flush",
                Value::Null,
            ),
            (Setting::UpdateGeo, "POST", "/configs/geo", Value::Null),
            (Setting::Restart, "POST", "/restart", Value::Null),
            (Setting::Upgrade, "POST", "/upgrade", Value::Null),
        ];
        for (setting, ..) in &expected {
            select(&mut app, *setting);
            if setting.asks() {
                key_press(&mut app, KeyCode::Enter);
                assert_eq!(app.overlay, Some(Overlay::Confirm), "{setting:?}");
                key_press(&mut app, KeyCode::Char('n'));
                assert!(app.overlay.is_none() && app.pending.is_none());
                key_press(&mut app, KeyCode::Char(' '));
                key_press(&mut app, KeyCode::Char('y'));
            } else if let Setting::Port(_) = setting {
                retype_port(&mut app, "17891");
                key_press(&mut app, KeyCode::Enter);
            } else {
                key_press(&mut app, KeyCode::Char(' '));
            }
            assert!(
                matches!(
                    rx.recv().await.map(|envelope| envelope.msg),
                    Some(Msg::Changed { result: Ok(()), .. })
                ),
                "{setting:?}"
            );
        }

        // In order, one each: `n` sent nothing.
        let requests = server.received_requests().await.unwrap();
        let sent: Vec<(String, String, Value)> = requests
            .iter()
            .map(|request| {
                let query = request.url.query().map(|q| format!("?{q}"));
                let body = if request.body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&request.body).unwrap()
                };
                (
                    request.method.to_string(),
                    format!("{}{}", request.url.path(), query.unwrap_or_default()),
                    body,
                )
            })
            .collect();
        let expected: Vec<(String, String, Value)> = expected
            .into_iter()
            .map(|(_, verb, route, body)| (verb.to_owned(), route.to_owned(), body))
            .collect();
        assert_eq!(sent, expected);
    }

    #[tokio::test]
    async fn a_setting_in_progress_takes_no_second_press_and_old_replies_cannot_undo_it() {
        let (mut app, rx) = settings_app("http://127.0.0.1:1", true, MIHOMO_CONFIGS);
        // The startup fetch, number 1, is slow.
        app.config_fetches = 1;
        select(&mut app, Setting::AllowLan);
        key_press(&mut app, KeyCode::Enter);
        key_press(&mut app, KeyCode::Enter);

        // Patched: the refetch, number 2, is in flight and the row still busy.
        app.handle_msg(Msg::Changed {
            change: Change::Patch(Patch::AllowLan(true)),
            result: Ok(()),
        });
        assert_eq!(app.toasts.back().unwrap().text, "局域网连接已开启");
        key_press(&mut app, KeyCode::Enter);
        assert!(app.is_changing(Setting::AllowLan));

        let lan = |on: bool| Configs {
            allow_lan: Some(on),
            mode: "rule".to_owned(),
            ..Configs::default()
        };
        app.handle_msg(Msg::Configs {
            seq: 2,
            refetch: true,
            result: Ok(lan(true)),
        });
        assert!(!app.is_changing(Setting::AllowLan));
        // The slow startup reply, from before the patch, is dropped.
        app.handle_msg(Msg::Configs {
            seq: 1,
            refetch: false,
            result: Ok(lan(false)),
        });
        assert!(matches!(&app.configs, Some(Ok(c)) if c.allow_lan == Some(true)));

        // Free again: the next press switches it back off. A maintenance
        // action is not sent twice either.
        key_press(&mut app, KeyCode::Enter);
        select(&mut app, Setting::FlushFakeIp);
        key_press(&mut app, KeyCode::Enter);
        key_press(&mut app, KeyCode::Char(' '));
        assert_eq!(requests_made(app, rx).await, (0, 1, 0, 3));
    }

    /// Two profiles on ports nobody listens on, `a` the default.
    const PROFILES: &str = r#"
default = "a"

[profiles.a]
url = "http://127.0.0.1:1"
secret = "a-secret"

[profiles.b]
url = "http://127.0.0.1:2"
secret = "b-secret"
"#;

    /// An app started with `overrides` over [`PROFILES`].
    fn profiles_app(overrides: &crate::config::Overrides) -> (App, mpsc::Receiver<Envelope>) {
        let (tx, rx) = mpsc::channel(64);
        let config: crate::config::Config = toml::from_str(PROFILES).unwrap();
        let settings = config.resolve(overrides).unwrap();
        let profiles = config.profiles(overrides, &settings);
        let app = launch(settings, tx, profiles, Picker::default());
        (app, rx)
    }

    /// `P`, then `Enter` on row `index` of the popup.
    fn switch_to(app: &mut App, index: usize) {
        key_press(app, KeyCode::Char('P'));
        assert_eq!(app.overlay, Some(Overlay::Profiles));
        key_press(app, KeyCode::Char('g'));
        for _ in 0..index {
            key_press(app, KeyCode::Char('j'));
        }
        key_press(app, KeyCode::Enter);
        assert_eq!(app.overlay, None);
    }

    fn toast_texts(app: &App) -> Vec<&str> {
        app.toasts.iter().map(|toast| toast.text.as_str()).collect()
    }

    /// One of every reply and stream event the app takes in.
    fn every_kind_of_msg() -> Vec<Msg> {
        let provider = ProviderRef {
            kind: ProviderKind::Proxy,
            name: "P".to_owned(),
        };
        let connections = ConnectionSnapshot {
            upload_total: 1,
            download_total: 2,
            connections: vec![Connection::default()],
        };
        let stream = |stream, event| Msg::Stream { stream, event };
        vec![
            Msg::Version(Ok(kernel(true))),
            // Each of these would raise the banner or toast.
            Msg::Version(Err(ApiError::Timeout)),
            Msg::Configs {
                seq: 1,
                refetch: false,
                result: Err(http(500)),
            },
            Msg::Configs {
                seq: 1,
                refetch: false,
                result: Ok(Configs::default()),
            },
            fetched(1, false, reply(&[("a", &[50])])),
            Msg::Rules {
                seq: 1,
                refetch: false,
                result: Ok(vec![]),
            },
            Msg::Delay {
                name: "a".to_owned(),
                result: Ok(80),
            },
            Msg::Delay {
                name: "a".to_owned(),
                result: Err(http(500)),
            },
            // A success here refetches the proxies.
            Msg::GroupDelay {
                members: vec!["a".to_owned()],
                result: Ok(HashMap::new()),
            },
            Msg::Selected {
                group: "G".to_owned(),
                node: "a".to_owned(),
                result: Ok(()),
            },
            Msg::ConnectionsClosed {
                results: vec![("c".to_owned(), Ok(()))],
                report: true,
            },
            Msg::AllConnectionsClosed {
                count: 3,
                result: Err(http(500)),
            },
            Msg::Changed {
                change: Change::Patch(Patch::Mode(Mode::Global)),
                result: Ok(()),
            },
            Msg::Providers(ProviderMsg::Done {
                target: provider,
                work: Work::Checking,
                result: Ok(()),
            }),
            stream(Stream::Traffic, StreamEvent::Connected),
            stream(
                Stream::Traffic,
                StreamEvent::Traffic(Traffic { up: 1, down: 2 }),
            ),
            stream(Stream::Connections, StreamEvent::Connected),
            stream(Stream::Connections, StreamEvent::Connections(connections)),
            stream(
                Stream::Logs,
                StreamEvent::Log(Log {
                    kind: LogKind::Level(LogLevel::Info),
                    payload: "old".to_owned(),
                }),
            ),
            // The last stream up going down is an outage, which probes.
            stream(Stream::Traffic, StreamEvent::Disconnected),
            stream(Stream::Connections, StreamEvent::Disconnected),
        ]
    }

    #[tokio::test]
    async fn nothing_sent_for_the_previous_profile_reaches_the_new_one() {
        let (mut app, _rx) = profiles_app(&crate::config::Overrides::default());
        let old = app.outbox.epoch;
        switch_to(&mut app, 1);
        let fetches = (app.fetches, app.config_fetches, app.rule_fetches);
        for msg in every_kind_of_msg() {
            app.receive(Envelope { epoch: old, msg });
        }
        assert!(app.version.is_none() && app.configs.is_none() && app.proxies.is_none());
        assert!(app.rules_view.rows.is_none());
        assert!(app.reconnect.is_none() && !app.links.outage && app.links.up.is_empty());
        assert!(app.traffic.is_none() && app.connections.is_none());
        assert!(app.connections_view.rows.is_none() && app.logs_view.entries.is_empty());
        assert!(app.changing.is_empty());
        assert_eq!(toast_texts(&app), ["已切换到 b"]);
        assert_eq!(
            (app.fetches, app.config_fetches, app.rule_fetches),
            fetches,
            "a stale reply asked for more"
        );

        // The same news from the controller in use does count.
        let epoch = app.outbox.epoch;
        app.receive(Envelope {
            epoch,
            msg: Msg::Version(Err(ApiError::Timeout)),
        });
        assert!(app.reconnect.is_some());
    }

    #[tokio::test]
    async fn a_switch_leaves_nothing_of_the_old_controller_on_screen() {
        let (mut app, _rx) = profiles_app(&crate::config::Overrides::default());
        app.handle_msg(Msg::Version(Err(ApiError::Timeout)));
        app.handle_msg(fetched(1, false, reply(&[("a", &[50])])));
        app.testing.insert("a".to_owned());
        connect(&mut app, Stream::Logs);
        stream_event(
            &mut app,
            Stream::Logs,
            StreamEvent::Log(Log {
                kind: LogKind::Level(LogLevel::Info),
                payload: "old".to_owned(),
            }),
        );
        stream_event(
            &mut app,
            Stream::Traffic,
            StreamEvent::Traffic(Traffic { up: 1, down: 2 }),
        );
        app.page = Page::Logs;
        key_press(&mut app, KeyCode::Char('l'));
        app.page = Page::Proxies;
        key_press(&mut app, KeyCode::Char('s'));
        let (level, sort) = (app.logs_view.level, app.proxies_view.sort);
        assert!(app.reconnect.is_some() && !app.logs_view.entries.is_empty());

        switch_to(&mut app, 1);
        assert_eq!(app.settings.profile, "b");
        assert_eq!(app.profiles.current, 1);
        // The page loads afresh instead of showing the old groups.
        assert!(app.proxies.is_none() && app.is_loading());
        assert!(app.reconnect.is_none(), "the old banner stayed");
        assert!(app.testing.is_empty() && app.traffic.is_none());
        assert!(app.logs_view.entries.is_empty());
        assert_eq!(toast_texts(&app), ["已切换到 b"]);
        // What is the user's stays.
        assert_eq!(app.page, Page::Proxies);
        assert_eq!((app.logs_view.level, app.proxies_view.sort), (level, sort));
    }

    #[tokio::test]
    async fn theme_overrides_survive_a_profile_switch() {
        use ratatui::style::Color;

        let (tx, _rx) = mpsc::channel(64);
        let text =
            format!("{PROFILES}\n[theme_overrides]\naccent = \"#112233\"\nc1 = \"light-red\"\n");
        let config: crate::config::Config = toml::from_str(&text).unwrap();
        let overrides = crate::config::Overrides::default();
        let settings = config.resolve(&overrides).unwrap();
        let profiles = config.profiles(&overrides, &settings);
        let mut app = launch(settings, tx, profiles, Picker::default());

        let colors = |app: &App| (app.theme.accent, app.theme.c1, app.theme.muted);
        let expected = (
            Color::Rgb(0x11, 0x22, 0x33),
            Color::LightRed,
            Color::DarkGray,
        );
        assert_eq!(colors(&app), expected);
        switch_to(&mut app, 1);
        assert_eq!(app.settings.profile, "b");
        assert_eq!(colors(&app), expected);
    }

    #[tokio::test]
    async fn the_theme_picker_previews_restores_and_saves_under_the_overrides() {
        use std::fs;

        use ratatui::style::Color;

        use crate::config::{Config, Overrides, ThemeName};

        let dir = std::env::temp_dir().join(format!("tabby-{}-picker", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let themes_dir = dir.join("themes");
        fs::create_dir_all(&themes_dir).unwrap();
        let mine = "extends = \"nord\"\n[colors]\nfg = \"#010203\"\nbg = \"#040506\"\n";
        fs::write(themes_dir.join("mine.toml"), mine).unwrap();
        fs::write(themes_dir.join("broken.toml"), "[colors]\nfg = \"pink\"\n").unwrap();
        let path = dir.join("config.toml");
        let written = format!("{PROFILES}\n[theme_overrides]\naccent = \"#112233\"\n");
        fs::write(&path, &written).unwrap();

        let (tx, _rx) = mpsc::channel(64);
        let config = Config::load(Some(&path)).unwrap();
        let settings = config.resolve(&Overrides::default()).unwrap();
        let profiles = config.profiles(&Overrides::default(), &settings);
        let picker = Picker {
            dir: Some(themes_dir),
            config: Some(path.clone()),
            ..Picker::default()
        };
        let mut app = launch(settings, tx, profiles, picker);
        let start = app.theme;
        let accent = Color::Rgb(0x11, 0x22, 0x33);
        assert_eq!(start.accent, accent);

        key_press(&mut app, KeyCode::Char('C'));
        assert_eq!(app.overlay, Some(Overlay::Themes));
        let ids: Vec<&str> = app.themes.entries.iter().map(|e| e.id.as_str()).collect();
        let mut expected: Vec<&str> = ThemeName::ALL.map(ThemeName::id).to_vec();
        expected.extend(["broken", "mine"]);
        assert_eq!(ids, expected);
        // On the theme in use.
        assert_eq!(app.themes.cursor, 0);
        let reason = app.themes.entries[expected.len() - 2]
            .theme
            .as_ref()
            .unwrap_err();
        assert!(reason.contains("`fg`"), "{reason}");

        // The cursor previews: the file over what it extends, the overrides
        // over both.
        key_press(&mut app, KeyCode::Char('G'));
        let nord = Theme::from(ThemeName::Nord);
        assert_eq!(
            (
                app.theme.fg,
                app.theme.bg,
                app.theme.success,
                app.theme.accent
            ),
            (
                Color::Rgb(1, 2, 3),
                Color::Rgb(4, 5, 6),
                nord.success,
                accent
            )
        );
        // A broken theme previews the one in use, and is not taken.
        key_press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.theme, start);
        key_press(&mut app, KeyCode::Enter);
        assert_eq!(app.overlay, Some(Overlay::Themes));
        // `Esc` brings back the theme in use and saves nothing.
        key_press(&mut app, KeyCode::Char('j'));
        key_press(&mut app, KeyCode::Esc);
        assert_eq!((app.overlay, app.theme), (None, start));
        assert_eq!(app.settings.theme, "terminal");
        assert_eq!(fs::read_to_string(&path).unwrap(), written);

        // `Enter` keeps it and saves it.
        key_press(&mut app, KeyCode::Char('C'));
        key_press(&mut app, KeyCode::Char('G'));
        key_press(&mut app, KeyCode::Enter);
        assert_eq!(app.overlay, None);
        assert_eq!(app.settings.theme, "mine");
        assert_eq!(app.theme.fg, Color::Rgb(1, 2, 3));
        assert_eq!(toast_texts(&app), ["已切换到主题 mine 并保存"]);
        let saved = Config::load(Some(&path)).unwrap();
        let settings = saved.resolve(&Overrides::default()).unwrap();
        assert_eq!(settings.theme, "mine");
        assert_eq!(settings.theme_overrides, app.settings.theme_overrides);

        // It outlives a profile switch, and the picker opens on it.
        let chosen = app.theme;
        switch_to(&mut app, 1);
        assert_eq!(app.theme, chosen);
        key_press(&mut app, KeyCode::Char('C'));
        assert_eq!(app.themes.entries[app.themes.cursor].id, "mine");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn launch_overrides_hold_for_the_launch_connection_only() {
        let overrides = crate::config::Overrides {
            url: Some("http://127.0.0.1:3".to_owned()),
            secret: Some("cli-secret".to_owned()),
            ..Default::default()
        };
        let (mut app, _rx) = profiles_app(&overrides);
        let connection = |app: &App| (app.settings.url.clone(), app.settings.secret.clone());
        let launch = connection(&app);
        assert_eq!(app.profiles.current, 0);

        // Row 0 is the launch connection, then `a`, then `b`.
        switch_to(&mut app, 2);
        assert_eq!(
            connection(&app),
            ("http://127.0.0.1:2".to_owned(), Some("b-secret".to_owned()))
        );
        switch_to(&mut app, 1);
        assert_eq!(
            connection(&app),
            ("http://127.0.0.1:1".to_owned(), Some("a-secret".to_owned()))
        );
        switch_to(&mut app, 0);
        assert_eq!(connection(&app), launch);
        assert_eq!(app.profiles.current, 0);
        assert_eq!(toast_texts(&app), ["已切换到 启动参数指定"]);
    }

    #[tokio::test]
    async fn picking_the_profile_in_use_does_not_reconnect() {
        let (mut app, rx) = profiles_app(&crate::config::Overrides::default());
        app.handle_msg(fetched(1, false, reply(&[("a", &[50])])));
        let epoch = app.outbox.epoch;
        switch_to(&mut app, 0);
        assert_eq!(app.outbox.epoch, epoch);
        assert!(app.proxies.is_some() && app.toasts.is_empty());
        assert_eq!(requests_made(app, rx).await, (0, 0, 0, 0));
    }

    /// What `app` draws on a `width` x `height` terminal, row by row, the
    /// cell after a wide character left out.
    fn screen_text(app: &App, width: u16, height: u16) -> String {
        use unicode_width::UnicodeWidthStr;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..height {
            let mut x = 0;
            while x < width {
                let symbol = buffer[(x, y)].symbol();
                text.push_str(symbol);
                x += u16::try_from(symbol.width().max(1)).unwrap();
            }
            text.push('\n');
        }
        text
    }

    /// The screen rows of `app` at 80x24 the scrollbar's thumb is on: those
    /// ending in it, on the panel's right border.
    fn thumb_rows(app: &App) -> Vec<usize> {
        screen_text(app, 80, 24)
            .lines()
            .enumerate()
            .filter(|(_, line)| line.ends_with('█'))
            .map(|(row, _)| row)
            .collect()
    }

    /// The screen row of `app` at 80x24 holding `text`.
    fn row_of(app: &App, text: &str) -> usize {
        screen_text(app, 80, 24)
            .lines()
            .position(|line| line.contains(text))
            .unwrap()
    }

    /// Whether screen row `row` of `app` at 80x24 ends with `corner`.
    fn row_ends_with(app: &App, row: usize, corner: char) -> bool {
        screen_text(app, 80, 24)
            .lines()
            .nth(row)
            .is_some_and(|line| line.ends_with(corner))
    }

    #[test]
    fn the_rules_scrollbar_runs_beside_the_rows_not_the_header() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Rules;
        let rules = (0..100)
            .map(|n| rule("DomainSuffix", &format!("{n}.example"), "DIRECT"))
            .collect();
        app.rules_view.apply(false, Ok(rules));
        let header = row_of(&app, "匹配内容");
        assert_eq!(thumb_rows(&app)[0], header + 1);
        app.update(Action::Cursor(Move::Bottom));
        assert!(row_ends_with(
            &app,
            *thumb_rows(&app).last().unwrap() + 1,
            '╯'
        ));
    }

    #[tokio::test]
    async fn a_screen_too_small_only_says_so_and_keys_still_work() {
        let (mut app, _rx) = app_without_controller();
        for (width, height) in [(49, 20), (60, 14)] {
            app.update(Action::Resize(Size::new(width, height)));
            let screen = screen_text(&app, width, height);
            assert!(screen.contains("终端尺寸过小"), "{screen}");
            assert!(screen.contains(&format!("当前 {width}×{height}，至少需要 50×15")));
            // No header, tabs, page or footer.
            for part in ["tabby", "概览", "正在连接控制器", "帮助"] {
                assert!(!screen.contains(part), "{part} in {screen}");
            }
        }
        key_press(&mut app, KeyCode::Char('4'));
        key_press(&mut app, KeyCode::Char('/'));
        key_press(&mut app, KeyCode::Char('x'));
        key_press(&mut app, KeyCode::Enter);
        assert_eq!(app.page, Page::Rules);
        assert_eq!(app.rules_view.filter.value(), "x");
        // Back to a size it draws in, all of it is there.
        app.update(Action::Resize(Size::new(50, 15)));
        let screen = screen_text(&app, 50, 15);
        assert!(!screen.contains("终端尺寸过小"));
        assert!(screen.contains("正在加载规则"), "{screen}");
        assert!(
            screen.contains("帮助") && screen.contains("退出"),
            "{screen}"
        );
        app.update(Action::Resize(Size::new(49, 20)));
        key_press(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    /// The proxies page of [`app_with_proxies`] in a terminal `width`
    /// columns wide.
    fn proxies_page(width: u16) -> App {
        let (mut app, _) = app_with_proxies();
        app.page = Page::Proxies;
        app.size = Size::new(width, 30);
        assert_eq!(app.visible_groups().count(), 4);
        app
    }

    #[test]
    fn a_narrow_proxies_page_picks_its_group_from_a_drop_down() {
        let mut app = proxies_page(99);
        assert_eq!(app.proxies_view.focus, Pane::Groups);
        // `j` and `k` do not move along the picker row.
        key_press(&mut app, KeyCode::Char('k'));
        assert_eq!(
            (app.proxies_view.focus, app.proxies_view.group),
            (Pane::Groups, 0)
        );

        // `Enter` opens the drop-down on the selected group; moving in it
        // selects nothing until `Enter`, `Esc` keeps the group.
        key_press(&mut app, KeyCode::Enter);
        assert_eq!(app.overlay, Some(Overlay::Groups));
        key_press(&mut app, KeyCode::Char('j'));
        key_press(&mut app, KeyCode::Char('j'));
        assert_eq!((app.proxies_view.picker, app.proxies_view.group), (2, 0));
        key_press(&mut app, KeyCode::Esc);
        assert_eq!((app.overlay, app.proxies_view.group), (None, 0));

        key_press(&mut app, KeyCode::Enter);
        assert_eq!(app.proxies_view.picker, 0);
        key_press(&mut app, KeyCode::Char('G'));
        // The drop-down is modal: the help, pages and the grid wait.
        for code in [KeyCode::Char('?'), KeyCode::Char('5'), KeyCode::Char('l')] {
            key_press(&mut app, code);
        }
        assert_eq!(app.overlay, Some(Overlay::Groups));
        assert_eq!(app.page, Page::Proxies);
        key_press(&mut app, KeyCode::Enter);
        assert_eq!(app.overlay, None);
        assert_eq!(
            (app.proxies_view.focus, app.proxies_view.group),
            (Pane::Groups, 3)
        );

        // `l`, `j` and `↓` go into the grid; `h` from its leftmost column
        // comes back to the picker row.
        for code in [KeyCode::Char('l'), KeyCode::Char('j'), KeyCode::Down] {
            key_press(&mut app, code);
            assert_eq!(app.proxies_view.focus, Pane::Nodes);
            key_press(&mut app, KeyCode::Char('g'));
            key_press(&mut app, KeyCode::Char('h'));
            assert_eq!(app.proxies_view.focus, Pane::Groups);
        }
        assert_eq!(app.proxies_view.group, 3);
    }

    #[test]
    fn crossing_100_columns_keeps_the_focus_and_the_group() {
        let mut app = proxies_page(100);
        // Wide, `j` moves along the group list and `Enter` enters the grid.
        key_press(&mut app, KeyCode::Char('j'));
        key_press(&mut app, KeyCode::Enter);
        assert_eq!(
            (app.proxies_view.focus, app.proxies_view.group),
            (Pane::Nodes, 1)
        );
        for width in [99, 100, 99] {
            app.update(Action::Resize(Size::new(width, 30)));
            assert_eq!(
                (app.proxies_view.focus, app.proxies_view.group),
                (Pane::Nodes, 1)
            );
        }
        key_press(&mut app, KeyCode::Char('g'));
        key_press(&mut app, KeyCode::Char('h'));
        app.update(Action::Resize(Size::new(100, 30)));
        assert_eq!(
            (app.proxies_view.focus, app.proxies_view.group),
            (Pane::Groups, 1)
        );

        // Growing past 100 with the drop-down open closes it and keeps the
        // group selected before.
        app.update(Action::Resize(Size::new(99, 30)));
        key_press(&mut app, KeyCode::Enter);
        key_press(&mut app, KeyCode::Char('j'));
        app.update(Action::Resize(Size::new(100, 30)));
        assert_eq!(app.overlay, None);
        assert_eq!(
            (app.proxies_view.focus, app.proxies_view.group),
            (Pane::Groups, 1)
        );
    }

    /// With the `terminal` theme, the cursor is reverse video in the default
    /// colors on every cell of it: a span inverted in a color of its own
    /// becomes a background of that color, and the row a patchwork.
    #[test]
    fn the_reversed_cursor_is_one_pair_of_colors() {
        use ratatui::style::{Color, Modifier};
        // Below the header, whose mode pill is reverse video in the accent.
        let check = |app: &App, at: &str| {
            let Size { width, height } = app.size;
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
            let buffer = terminal.backend().buffer();
            let mut reversed = 0;
            for y in 1..height {
                for x in 0..width {
                    let cell = &buffer[(x, y)];
                    if cell.modifier.contains(Modifier::REVERSED) {
                        reversed += 1;
                        assert_eq!(
                            (cell.fg, cell.bg),
                            (Color::Reset, Color::Reset),
                            "{at}: ({x}, {y}) {:?}",
                            cell.symbol()
                        );
                    }
                }
            }
            assert!(reversed > 0, "{at}: no cursor drawn");
        };

        // The group list: muted type, `→ node` and the delay in the rows.
        let mut app = proxies_page(100);
        check(&app, "group list");
        key_press(&mut app, KeyCode::Char('j'));
        check(&app, "URLTest group");

        let mut app = proxies_page(99);
        check(&app, "picker row");
        key_press(&mut app, KeyCode::Enter);
        key_press(&mut app, KeyCode::Char('j'));
        check(&app, "drop-down");
    }

    #[test]
    fn the_help_scrolls_no_further_than_its_last_row() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Connections;
        // One column of 25 rows, 23 shown.
        app.size = Size::new(69, 25);
        key_press(&mut app, KeyCode::Char('?'));
        let mut press = |code| {
            key_press(&mut app, code);
            app.help_scroll
        };
        assert_eq!(press(KeyCode::Char('k')), 0);
        assert_eq!(press(KeyCode::Char('j')), 1);
        assert_eq!(press(KeyCode::Char('j')), 2);
        assert_eq!(press(KeyCode::Char('j')), 2);
        assert_eq!(press(KeyCode::Char('g')), 0);
        assert_eq!(press(KeyCode::Char('G')), 2);
        // 13 of the rows shown: half a page is 6.
        app.update(Action::Resize(Size::new(50, 15)));
        app.update(Action::Cursor(Move::Bottom));
        assert_eq!(app.help_scroll, 12);
        app.update(Action::Cursor(Move::HalfUp));
        assert_eq!(app.help_scroll, 6);
        // The page behind did not move, and growing the screen brings the
        // rows that now fit back into view.
        assert_eq!(app.connections_view.cursor, 0);
        app.update(Action::Resize(Size::new(69, 27)));
        assert_eq!(app.help_scroll, 0);
        // Two columns fit at 70 x 24: nothing to scroll.
        app.update(Action::Resize(Size::new(70, 24)));
        key_press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.help_scroll, 0);
        // Opened again, it starts at the top.
        app.update(Action::Resize(Size::new(50, 15)));
        key_press(&mut app, KeyCode::Char('G'));
        key_press(&mut app, KeyCode::Char('?'));
        key_press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.help_scroll, 0);
    }

    #[test]
    fn the_help_shows_the_version_on_its_bottom_border_down_to_the_smallest_screen() {
        let (mut app, _rx) = app_without_controller();
        key_press(&mut app, KeyCode::Char('?'));
        let version = format!(" tabby {} ─╯", env!("TABBY_LONG_VERSION"));
        for (width, height) in [(120, 40), (50, 15)] {
            let screen = screen_text(&app, width, height);
            let bottom = screen.lines().rfind(|line| line.contains('╰')).unwrap();
            // The corners stay, with a stretch of border on either side.
            let bottom = bottom.trim();
            assert!(bottom.starts_with("╰──"), "{bottom}");
            assert!(bottom.ends_with(&version), "{bottom}");
        }
    }

    /// Draws `app` at its own size, which registers what the mouse can act
    /// on, as the event loop does before it reads one.
    fn draw(app: &App) -> String {
        screen_text(app, app.size.width, app.size.height)
    }

    /// A moment that is a second later than the one every earlier call
    /// returned: presses made with it are never a double click.
    fn later() -> Instant {
        use std::sync::{
            LazyLock,
            atomic::{AtomicU64, Ordering},
        };
        static START: LazyLock<Instant> = LazyLock::new(Instant::now);
        static SECONDS: AtomicU64 = AtomicU64::new(0);
        *START + Duration::from_secs(SECONDS.fetch_add(1, Ordering::Relaxed))
    }

    fn mouse_at(app: &mut App, kind: MouseEventKind, column: u16, row: u16, now: Instant) {
        let event = MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        if let Some(action) = app.mouse_action(event, now) {
            app.update(action);
        }
    }

    fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
        mouse_at(app, kind, column, row, later());
    }

    fn click(app: &mut App, column: u16, row: u16) {
        mouse(app, MouseEventKind::Down(MouseButton::Left), column, row);
    }

    /// A left press at `now`.
    fn press_at(app: &mut App, column: u16, row: u16, now: Instant) {
        mouse_at(
            app,
            MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            now,
        );
    }

    /// The column `needle` starts at on screen row `row` of `screen`.
    fn column_of(screen: &str, row: usize, needle: &str) -> u16 {
        use unicode_width::UnicodeWidthStr;
        let line = screen.lines().nth(row).unwrap();
        let at = line
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} not in {line}"));
        u16::try_from(line[..at].width()).unwrap()
    }

    /// Rules page of `count` rules at `size`, the first one under the cursor.
    fn rules_page(count: usize, size: Size) -> App {
        let (mut app, _rx) = app_without_controller();
        let payloads: Vec<String> = (0..count).map(|n| n.to_string()).collect();
        let payloads: Vec<&str> = payloads.iter().map(String::as_str).collect();
        app.handle_msg(Msg::Rules {
            seq: 1,
            refetch: false,
            result: Ok(rules_of(&payloads)),
        });
        app.page = Page::Rules;
        app.size = size;
        app
    }

    #[tokio::test]
    async fn a_click_on_a_tab_opens_its_page_with_titles_or_with_numbers_only() {
        let (mut app, _rx) = app_without_controller();
        for width in [120, 80] {
            app.update(Action::Resize(Size::new(width, 30)));
            for (index, page) in Page::ALL.into_iter().enumerate() {
                let label = if width >= 100 {
                    format!("{} {}", index + 1, crate::ui::page_title(page))
                } else {
                    (index + 1).to_string()
                };
                // Some other page first, so that a click that did nothing
                // shows.
                app.page = Page::ALL[(index + 3) % 7];
                let screen = draw(&app);
                let x = column_of(&screen, 1, &label);
                click(&mut app, x, 1);
                assert_eq!(app.page, page, "{label} at {width}");
                // The row under the labels is the divider; it belongs to
                // the tab above it.
                app.page = Page::ALL[(index + 3) % 7];
                draw(&app);
                click(&mut app, x, 2);
                assert_eq!(app.page, page, "divider under {label} at {width}");
            }
        }
        // Bare numbers are three columns apart: the blank column on either
        // side belongs to the number next to it.
        app.update(Action::Resize(Size::new(80, 30)));
        app.page = Page::Overview;
        draw(&app);
        click(&mut app, 3 * 2 + 1, 1);
        assert_eq!(app.page, Page::Providers);
        click(&mut app, 3 * 3 - 1, 1);
        assert_eq!(app.page, Page::Rules);
    }

    #[tokio::test]
    async fn a_click_ends_a_filter_keeping_the_text_and_drops_a_port_being_typed() {
        let mut app = rules_page(5, Size::new(120, 30));
        key_press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "ab");
        assert!(app.typing());
        let screen = draw(&app);
        // On the page's list: the click lands on it, and that is all.
        click(&mut app, 5, 10);
        assert!(!app.typing() && app.page == Page::Rules);
        assert_eq!(app.rules_view.filter.value(), "ab");
        key_press(&mut app, KeyCode::Char('/'));
        let x = column_of(&screen, 1, "5 连接");
        click(&mut app, x, 1);
        assert!(!app.typing());
        assert_eq!(app.page, Page::Connections);
        assert_eq!(app.rules_view.filter.value(), "ab");

        let (mut app, rx) = settings_app("http://127.0.0.1:1", true, MIHOMO_CONFIGS);
        select(&mut app, Setting::Port(Port::Mixed));
        retype_port(&mut app, "9999");
        assert_eq!(port_box(&app), Some("9999"));
        let screen = draw(&app);
        let x = column_of(&screen, 1, "2 代理");
        click(&mut app, x, 1);
        // Left unsent, as `Esc` leaves it.
        assert!(port_box(&app).is_none() && app.page == Page::Proxies);
        assert!(!app.is_changing(Setting::Port(Port::Mixed)));
        assert_eq!(requests_made(app, rx).await, (0, 0, 0, 0));
    }

    #[tokio::test]
    async fn a_click_outside_a_popup_closes_it_and_never_reaches_the_page_below() {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Rules;
        let screen = draw(&app);
        let tab = column_of(&screen, 1, "5 连接");

        // Help: the tab under the backdrop does not switch pages.
        key_press(&mut app, KeyCode::Char('?'));
        draw(&app);
        click(&mut app, tab, 1);
        assert!(app.overlay.is_none());
        assert_eq!(app.page, Page::Rules);
        // Inside the popup, nothing happens to it.
        key_press(&mut app, KeyCode::Char('?'));
        draw(&app);
        click(&mut app, 60, 20);
        assert_eq!(app.overlay, Some(Overlay::Help));
        click(&mut app, 0, 0);
        assert!(app.overlay.is_none());

        // Themes: closing by a click puts the theme in use back.
        let before = app.theme;
        key_press(&mut app, KeyCode::Char('C'));
        key_press(&mut app, KeyCode::Char('j'));
        assert_ne!(app.theme, before);
        draw(&app);
        click(&mut app, 0, 0);
        assert!(app.overlay.is_none());
        assert_eq!(app.theme, before);

        // Profiles.
        key_press(&mut app, KeyCode::Char('P'));
        draw(&app);
        click(&mut app, 0, 0);
        assert!(app.overlay.is_none());

        // The drop-down of the narrow proxies page: the group under its
        // cursor is not taken.
        let mut app = proxies_page(99);
        key_press(&mut app, KeyCode::Enter);
        assert_eq!(app.overlay, Some(Overlay::Groups));
        key_press(&mut app, KeyCode::Char('j'));
        draw(&app);
        click(&mut app, 98, 28);
        assert!(app.overlay.is_none());
        assert_eq!(app.proxies_view.group, 0);
    }

    #[tokio::test]
    async fn the_mouse_is_ignored_on_a_screen_too_small_to_draw() {
        let mut app = rules_page(40, Size::new(49, 20));
        let event = |kind| MouseEvent {
            kind,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        draw(&app);
        let now = Instant::now();
        assert_eq!(
            app.mouse_action(event(MouseEventKind::Down(MouseButton::Left)), now),
            None
        );
        assert_eq!(
            app.mouse_action(event(MouseEventKind::ScrollDown), now),
            None
        );
        // What an earlier, larger frame registered is forgotten.
        app.update(Action::Resize(Size::new(120, 30)));
        draw(&app);
        assert!(
            app.mouse_action(event(MouseEventKind::Down(MouseButton::Left)), now)
                .is_some()
        );
        app.update(Action::Resize(Size::new(49, 30)));
        draw(&app);
        assert_eq!(
            app.mouse_action(event(MouseEventKind::Down(MouseButton::Left)), now),
            None
        );
    }

    #[tokio::test]
    async fn the_wheel_moves_the_list_under_the_pointer_a_row_at_a_time() {
        let mut app = rules_page(40, Size::new(120, 30));
        draw(&app);
        mouse(&mut app, MouseEventKind::ScrollDown, 10, 10);
        mouse(&mut app, MouseEventKind::ScrollDown, 10, 10);
        mouse(&mut app, MouseEventKind::ScrollDown, 10, 10);
        assert_eq!(app.rules_view.cursor, 3);
        mouse(&mut app, MouseEventKind::ScrollUp, 10, 10);
        assert_eq!(app.rules_view.cursor, 2);
        // The tab row and the footer are not lists.
        mouse(&mut app, MouseEventKind::ScrollDown, 10, 1);
        mouse(&mut app, MouseEventKind::ScrollDown, 10, 29);
        assert_eq!(app.rules_view.cursor, 2);
        // Neither is the page of a kernel that has no list to scroll.
        app.page = Page::Overview;
        draw(&app);
        mouse(&mut app, MouseEventKind::ScrollDown, 10, 10);
        assert_eq!(app.rules_view.cursor, 2);

        // A popup takes the wheel wherever it is: the help scrolls, the page
        // does not.
        let mut app = rules_page(40, Size::new(50, 15));
        key_press(&mut app, KeyCode::Char('?'));
        draw(&app);
        mouse(&mut app, MouseEventKind::ScrollDown, 0, 0);
        mouse(&mut app, MouseEventKind::ScrollDown, 25, 7);
        assert_eq!(app.help_scroll, 2);
        assert_eq!(app.rules_view.cursor, 0);
        mouse(&mut app, MouseEventKind::ScrollUp, 25, 7);
        assert_eq!(app.help_scroll, 1);
    }

    #[tokio::test]
    async fn the_wheel_moves_the_pane_it_is_over_and_gives_it_the_focus() {
        let mut app = proxies_page(120);
        draw(&app);
        // The group list is the first 28 columns, the grid the rest.
        assert_eq!(app.proxies_view.focus, Pane::Groups);
        mouse(&mut app, MouseEventKind::ScrollDown, 10, 10);
        assert_eq!(
            (app.proxies_view.group, app.proxies_view.focus),
            (1, Pane::Groups)
        );
        // The group of eight nodes, three cards to a row.
        let eight = app
            .visible_groups()
            .position(|group| group.name == "节点选择")
            .unwrap();
        app.proxies_view.group = eight;
        let cursor = app.node_cursor();
        mouse(&mut app, MouseEventKind::ScrollDown, 60, 10);
        assert_eq!(app.proxies_view.focus, Pane::Nodes);
        assert_eq!(app.node_cursor(), cursor + 3);
        let group = app.proxies_view.group;
        mouse(&mut app, MouseEventKind::ScrollDown, 10, 10);
        assert_eq!(app.proxies_view.focus, Pane::Groups);
        assert_eq!(app.proxies_view.group, group + 1);

        // Narrow, the picker row is one line: nothing to scroll there.
        let mut app = proxies_page(99);
        draw(&app);
        mouse(&mut app, MouseEventKind::ScrollDown, 10, 3);
        assert_eq!(app.proxies_view.group, 0);
    }

    /// The first place `needle` shows on `screen` in the columns `cols`, as
    /// (column, row).
    fn locate(screen: &str, needle: &str, cols: std::ops::Range<u16>) -> (u16, u16) {
        use unicode_width::UnicodeWidthStr;
        for (row, line) in screen.lines().enumerate() {
            for (at, _) in line.match_indices(needle) {
                let column = u16::try_from(line[..at].width()).unwrap();
                if cols.contains(&column) {
                    return (column, u16::try_from(row).unwrap());
                }
            }
        }
        panic!("{needle} not in columns {cols:?} of\n{screen}");
    }

    /// Two presses 100 ms apart.
    fn double_click(app: &mut App, column: u16, row: u16) {
        let first = later();
        press_at(app, column, row, first);
        press_at(app, column, row, first + Duration::from_millis(100));
    }

    /// The proxies page of the fixture at `size`, with the channel the
    /// requests it makes report on.
    fn proxies_page_with_requests(size: Size) -> (App, mpsc::Receiver<Envelope>) {
        let proxies: Proxies =
            serde_json::from_str(include_str!("../tests/fixtures/mihomo_proxies.json")).unwrap();
        let (mut app, rx) = app_without_controller();
        app.proxies = Some(Ok(proxies));
        app.page = Page::Proxies;
        app.size = size;
        (app, rx)
    }

    /// The (group, node) of every selection the app asked the kernel for.
    async fn selections(app: App, mut rx: mpsc::Receiver<Envelope>) -> Vec<(String, String)> {
        drop(app);
        let mut selected = vec![];
        while let Some(Envelope { msg, .. }) = rx.recv().await {
            if let Msg::Selected { group, node, .. } = msg {
                selected.push((group, node));
            }
        }
        selected
    }

    #[tokio::test]
    async fn a_click_picks_a_group_and_a_card_and_a_second_one_acts_like_enter() {
        let (mut app, rx) = proxies_page_with_requests(Size::new(100, 30));
        let screen = draw(&app);
        // The group list.
        let (x, y) = locate(&screen, "负载均衡", 0..28);
        click(&mut app, x, y);
        let balance = app
            .visible_groups()
            .position(|group| group.name == "负载均衡")
            .unwrap();
        assert_eq!(
            (app.proxies_view.group, app.proxies_view.focus),
            (balance, Pane::Groups)
        );
        // Two quick ones go into the node area, as `Enter` does.
        double_click(&mut app, x, y);
        assert_eq!(app.proxies_view.focus, Pane::Nodes);

        // A group of eight nodes: a click puts the cursor on a card and
        // the focus on the grid; two select the node.
        let (x, y) = locate(&draw(&app), "节点选择", 0..28);
        click(&mut app, x, y);
        assert_eq!(app.proxies_view.focus, Pane::Groups);
        let screen = draw(&app);
        let (x, y) = locate(&screen, "dead", 28..100);
        click(&mut app, x, y);
        assert_eq!(app.proxies_view.focus, Pane::Nodes);
        assert_eq!(app.visible_nodes()[app.node_cursor()], "dead");
        double_click(&mut app, x, y);
        assert_eq!(
            selections(app, rx).await,
            [("节点选择".to_owned(), "dead".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_click_on_a_card_of_a_narrow_page_puts_the_cursor_on_it() {
        let (mut app, _rx) = proxies_page_with_requests(Size::new(60, 20));
        let screen = draw(&app);
        let (x, y) = locate(&screen, "香港 01", 0..60);
        click(&mut app, x, y);
        assert_eq!(app.proxies_view.focus, Pane::Nodes);
        assert_eq!(app.visible_nodes()[app.node_cursor()], "香港 01");
    }

    #[tokio::test]
    async fn the_group_picker_opens_on_a_click_and_picks_on_two() {
        for size in [Size::new(99, 30), Size::new(60, 20)] {
            let (mut app, _rx) = proxies_page_with_requests(size);
            draw(&app);
            // One click on the picker row opens the drop-down.
            click(&mut app, 2, 3);
            assert_eq!(app.overlay, Some(Overlay::Groups), "{size:?}");
            let (x, y) = locate(&draw(&app), "负载均衡", 0..size.width);
            let balance = app
                .visible_groups()
                .position(|group| group.name == "负载均衡")
                .unwrap();
            // A click on an item only moves its cursor.
            click(&mut app, x, y);
            assert_eq!(app.proxies_view.picker, balance, "{size:?}");
            assert_eq!(app.overlay, Some(Overlay::Groups));
            assert_eq!(app.proxies_view.group, 0);
            // Two choose it.
            double_click(&mut app, x, y);
            assert_eq!(app.overlay, None);
            assert_eq!(app.proxies_view.group, balance, "{size:?}");
        }
    }

    #[tokio::test]
    async fn a_click_on_a_row_of_the_table_puts_the_cursor_on_that_rule() {
        for size in [Size::new(100, 30), Size::new(60, 20)] {
            let mut app = rules_page(40, size);
            draw(&app);
            // Below the tabs, the panel's border and the table's header.
            click(&mut app, 10, 5 + 5);
            assert_eq!(app.rules_view.cursor, 5, "{size:?}");
            // Scrolled to the end, the first row in view is not rule 0; a
            // click on the header is on no rule.
            key_press(&mut app, KeyCode::Char('G'));
            draw(&app);
            let rows = usize::from(size.height) - 8;
            click(&mut app, 10, 4);
            assert_eq!(app.rules_view.cursor, 39);
            click(&mut app, 10, 5);
            assert_eq!(app.rules_view.cursor, 40 - rows, "{size:?}");
        }
    }

    #[tokio::test]
    async fn the_connection_clicked_is_the_one_selected_even_after_the_table_reorders() {
        for size in [Size::new(100, 30), Size::new(60, 20)] {
            let (mut app, _rx) = app_without_controller();
            app.page = Page::Connections;
            app.size = size;
            feed(
                &mut app,
                vec![
                    flow("a", "1", 0, 0),
                    flow("b", "2", 0, 0),
                    flow("c", "3", 0, 0),
                ],
            );
            // Newest first.
            assert_eq!(table_ids(&app), ["c", "b", "a"]);
            draw(&app);
            // A newer one arrives before the click does: the second row is
            // now `c`, but `b` is what was on screen where the click is.
            feed(
                &mut app,
                vec![
                    flow("a", "1", 0, 0),
                    flow("b", "2", 0, 0),
                    flow("c", "3", 0, 0),
                    flow("d", "4", 0, 0),
                ],
            );
            click(&mut app, 10, 5 + 1);
            assert_eq!(
                app.connections_view
                    .selected_connection()
                    .unwrap()
                    .connection
                    .id,
                "b"
            );
            // Gone by then: ignored, and what was selected stays.
            feed(&mut app, vec![flow("c", "3", 0, 0), flow("d", "4", 0, 0)]);
            let before = app.connections_view.cursor;
            // `a`, the third row of the frame the click was made on.
            click(&mut app, 10, 5 + 2);
            assert_eq!(app.connections_view.cursor, before, "{size:?}");
            // Two clicks open the details, as `Enter` does.
            draw(&app);
            double_click(&mut app, 10, 5);
            assert_eq!(app.overlay, Some(Overlay::Connection));
            assert_eq!(
                app.connections_view
                    .detail
                    .as_ref()
                    .unwrap()
                    .row
                    .connection
                    .id,
                "d"
            );
        }
    }

    #[tokio::test]
    async fn a_click_picks_a_provider_and_a_setting_and_two_toggle_the_setting() {
        let (mut app, _rx) = providers_app(&["p1", "p2"], &["r1"]);
        let screen = draw(&app);
        let (x, y) = locate(&screen, "r1", 0..40);
        click(&mut app, x, y);
        assert_eq!(app.providers_view.cursor, 2);
        let (x, y) = locate(&screen, "p2", 0..40);
        click(&mut app, x, y);
        assert_eq!(app.providers_view.cursor, 1);
        // `Enter` does nothing on a provider, and so do two clicks.
        double_click(&mut app, x, y);
        assert_eq!(app.providers_view.cursor, 1);
        assert!(app.overlay.is_none());

        for size in [Size::new(100, 30), Size::new(60, 20)] {
            let (mut app, rx) = settings_app("http://127.0.0.1:1", true, MIHOMO_CONFIGS);
            app.size = size;
            let (x, y) = locate(&draw(&app), "IPv6", 0..size.width);
            click(&mut app, x, y);
            assert_eq!(app.selected_setting(), Setting::Ipv6);
            assert!(!app.is_changing(Setting::Ipv6));
            double_click(&mut app, x, y);
            assert!(app.is_changing(Setting::Ipv6), "{size:?}");
            assert_eq!(requests_made(app, rx).await, (0, 0, 0, 1));
        }

        // What asks first still asks: two clicks open the dialog, they do
        // not restart the kernel.
        let (mut app, rx) = settings_app("http://127.0.0.1:1", true, MIHOMO_CONFIGS);
        let (x, y) = locate(&draw(&app), "重启内核", 0..100);
        double_click(&mut app, x, y);
        assert_eq!(app.overlay, Some(Overlay::Confirm));
        assert!(!app.is_changing(Setting::Restart));
        assert_eq!(requests_made(app, rx).await, (0, 0, 0, 0));
    }

    #[tokio::test]
    async fn two_presses_are_a_double_click_only_on_one_row_and_within_400_ms() {
        let (mut app, rx) = settings_app("http://127.0.0.1:1", true, MIHOMO_CONFIGS);
        let screen = draw(&app);
        let (x, y) = locate(&screen, "IPv6", 0..100);
        let (other_x, other_y) = locate(&screen, "局域网连接", 0..100);
        let start = later();
        let at = |millis| start + Duration::from_millis(millis);
        press_at(&mut app, x, y, at(0));
        // Too late: another pick.
        press_at(&mut app, x, y, at(401));
        assert!(!app.is_changing(Setting::Ipv6));
        // Another row in between: not the same one twice.
        press_at(&mut app, other_x, other_y, at(450));
        press_at(&mut app, x, y, at(500));
        assert!(!app.is_changing(Setting::Ipv6));
        // In time, on the same row.
        press_at(&mut app, x, y, at(900));
        assert!(app.is_changing(Setting::Ipv6));
        assert_eq!(app.selected_setting(), Setting::Ipv6);
        // The third press of a streak is a first one: no second change (it
        // would be refused while the first is in progress, so look at what
        // is sent).
        press_at(&mut app, x, y, at(950));
        assert_eq!(requests_made(app, rx).await, (0, 0, 0, 1));

        // Tabs only switch pages, two clicks or not: `Enter` on the page
        // they open would act on its first row.
        let (mut app, rx) = settings_app("http://127.0.0.1:1", true, MIHOMO_CONFIGS);
        let (x, y) = locate(&draw(&app), "7 设置", 0..100);
        double_click(&mut app, x, y);
        assert_eq!(app.page, Page::Settings);
        assert!(!app.is_changing(Setting::Mode));
        assert_eq!(requests_made(app, rx).await, (0, 0, 0, 0));
    }

    #[tokio::test]
    async fn the_profile_and_theme_popups_take_clicks_on_their_rows() {
        let (mut app, _rx) = profiles_app(&crate::config::Overrides::default());
        key_press(&mut app, KeyCode::Char('P'));
        let (x, y) = locate(&draw(&app), "127.0.0.1:2", 0..120);
        click(&mut app, x, y);
        assert_eq!(app.profile_cursor, 1);
        assert_eq!(app.overlay, Some(Overlay::Profiles));
        double_click(&mut app, x, y);
        assert_eq!(app.overlay, None);
        assert_eq!(app.settings.profile, "b");

        let (mut app, _rx) = app_without_controller();
        let before = app.theme;
        key_press(&mut app, KeyCode::Char('C'));
        let (x, y) = locate(&draw(&app), "nord", 0..120);
        // One click previews it.
        click(&mut app, x, y);
        let nord = app
            .themes
            .entries
            .iter()
            .position(|entry| entry.id == "nord")
            .unwrap();
        assert_eq!(app.themes.cursor, nord);
        assert_ne!(app.theme, before);
        assert_eq!(app.overlay, Some(Overlay::Themes));
        assert_eq!(app.settings.theme, "terminal");
        // Two take it.
        draw(&app);
        double_click(&mut app, x, y);
        assert_eq!(app.overlay, None);
        assert_eq!(app.settings.theme, "nord");
    }

    /// The connections page of `size` with some connections in it.
    fn connections_page(size: Size) -> App {
        let (mut app, _rx) = app_without_controller();
        app.page = Page::Connections;
        app.size = size;
        feed(
            &mut app,
            vec![
                flow("a", "1", 5, 0),
                flow("b", "2", 9, 0),
                flow("c", "3", 1, 0),
            ],
        );
        app
    }

    #[tokio::test]
    async fn a_click_on_a_heading_sorts_by_it_and_a_second_one_turns_the_order() {
        let mut app = connections_page(Size::new(120, 30));
        let sort = |app: &App| {
            (
                app.connections_view.sort.column,
                app.connections_view.sort.ascending,
            )
        };
        assert_eq!(sort(&app), (SortColumn::Duration, true));
        let screen = draw(&app);
        let (x, y) = locate(&screen, "上行", 0..120);
        click(&mut app, x, y);
        // Traffic: the most first.
        assert_eq!(sort(&app), (SortColumn::Up, false));
        assert!(draw(&app).contains("上行 ▼"));
        click(&mut app, x, y);
        assert_eq!(sort(&app), (SortColumn::Up, true));
        assert!(draw(&app).contains("上行 ▲"));
        // Names: A to Z first.
        let (x, y) = locate(&screen, "主机", 0..120);
        click(&mut app, x, y);
        assert_eq!(sort(&app), (SortColumn::Host, true));
        // The route has nothing to sort by.
        let (x, y) = locate(&screen, "规则 → 链路", 0..120);
        click(&mut app, x, y);
        assert_eq!(sort(&app), (SortColumn::Host, true));

        // The rows follow: by size, the biggest first, then the smallest.
        let (x, y) = locate(&screen, "总量", 0..120);
        click(&mut app, x, y);
        assert_eq!(sort(&app), (SortColumn::Total, false));
        assert_eq!(table_ids(&app), ["b", "a", "c"]);
        click(&mut app, x, y);
        assert_eq!(table_ids(&app), ["c", "a", "b"]);
    }

    #[tokio::test]
    async fn a_compact_table_has_no_heading_for_the_columns_it_drops() {
        let mut app = connections_page(Size::new(60, 20));
        let screen = draw(&app);
        for hidden in ["进程", "总量"] {
            assert!(!screen.contains(hidden), "{screen}");
        }
        let (x, y) = locate(&screen, "时长", 0..60);
        click(&mut app, x, y);
        // The one that stays: shortest first is what it opens with, so
        // this is the other way round.
        assert_eq!(
            (
                app.connections_view.sort.column,
                app.connections_view.sort.ascending
            ),
            (SortColumn::Duration, false)
        );
        // A click on a heading takes the keyboard from the filter.
        key_press(&mut app, KeyCode::Char('/'));
        assert!(app.typing());
        draw(&app);
        let (x, y) = locate(&screen, "下行", 0..60);
        click(&mut app, x, y);
        assert!(!app.typing());
        assert_eq!(app.connections_view.sort.column, SortColumn::Down);
    }

    #[tokio::test]
    async fn a_click_on_a_mode_asks_the_kernel_for_it_and_the_one_in_use_asks_nothing() {
        let (mut app, rx) = settings_app("http://127.0.0.1:1", true, r#"{"mode":"rule"}"#);
        app.page = Page::Overview;
        let screen = draw(&app);
        // The mode in use.
        let (x, y) = locate(&screen, "规则", 0..120);
        click(&mut app, x, y);
        assert!(!app.is_changing(Setting::Mode));
        let (x, y) = locate(&screen, "全局", 0..120);
        click(&mut app, x, y);
        assert!(app.is_changing(Setting::Mode));
        // Until the kernel agrees the mode on screen is the old one, and a
        // second click waits for the first.
        click(&mut app, x, y);
        assert!(app.is_changing(Setting::Mode));
        assert_eq!(requests_made(app, rx).await, (0, 0, 0, 1));
    }

    fn managed_choice(core: &ManagedCore) -> crate::config::Choice {
        crate::config::Choice {
            origin: Origin::Managed,
            name: "core".to_owned(),
            url: core.url(),
            secret: Some(core.secret().to_owned()),
        }
    }

    /// An app on the managed `core`, as `main` starts one with
    /// `default = "core"`.
    fn managed_app(core: Arc<ManagedCore>) -> (App, mpsc::Receiver<Envelope>) {
        let (tx, rx) = mpsc::channel(64);
        let overrides = crate::config::Overrides {
            url: Some(core.url()),
            secret: Some(core.secret().to_owned()),
            ..Default::default()
        };
        let settings = crate::config::Config::default()
            .resolve(&overrides)
            .unwrap();
        let profiles = Profiles {
            choices: vec![managed_choice(&core)],
            current: 0,
            path: None,
            core: Some(core),
        };
        (launch(settings, tx, profiles, Picker::default()), rx)
    }

    /// Applies what the background work reports until `done`.
    async fn pump_until(
        app: &mut App,
        rx: &mut mpsc::Receiver<Envelope>,
        done: impl Fn(&App) -> bool,
    ) {
        while !done(app) {
            let envelope = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("news in time")
                .expect("the app is alive");
            app.receive(envelope);
        }
    }

    fn core_up(app: &App) -> bool {
        matches!(app.core_phase, Some(CorePhase::Running))
    }

    /// The box at the foot of the subscriptions page asks for a secret key.
    fn asking_age_key(app: &App) -> bool {
        app.subscription_prompt()
            .is_some_and(|prompt| prompt.secret)
    }

    #[tokio::test]
    async fn the_pages_wait_for_the_managed_kernel_and_load_once_it_is_up() {
        let dir = testing::scratch("app-start");
        let port = testing::free_port();
        let recorder = testing::systemd(port, Duration::from_millis(100));
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (mut app, mut rx) = managed_app(core);

        app.start();

        // Nothing is asked of a controller that is not there yet, whatever
        // page is shown, and the screen says what is going on.
        assert!(matches!(app.core_phase, Some(CorePhase::Starting(_))));
        for page in [Page::Overview, Page::Proxies, Page::Settings, Page::Rules] {
            app.update(Action::SelectPage(page));
            assert!(draw(&app).contains("正在启动内核"), "{page:?}");
        }
        assert_eq!(app.rule_fetches, 0);

        pump_until(&mut app, &mut rx, core_up).await;
        // The page that was waiting loads now.
        assert_eq!(app.rule_fetches, 1);
        pump_until(&mut app, &mut rx, |app| app.version.is_some()).await;
        assert!(draw(&app).contains("mihomo v1.19.33 · 托管"));
        assert_eq!(recorder.runs("systemd-run").len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_kernel_that_does_not_start_says_why_and_r_tries_again() {
        let dir = testing::scratch("app-fail");
        let port = testing::free_port();
        // Not there, failed (twice: the wait and the stop that follows), then
        // not there again for the second try, then running.
        let states =
            std::sync::Mutex::new(vec!["active", "inactive", "failed", "failed", "inactive"]);
        let recorder = testing::Recorder::new(move |cmd| {
            match (cmd.program.as_str(), cmd.args.get(1).map(String::as_str)) {
                ("systemctl", Some("is-active")) => {
                    let state = states.lock().unwrap().pop().unwrap_or("active");
                    testing::answer(if state == "active" { 0 } else { 3 }, state)
                }
                ("systemd-run", _) => {
                    // The first start leaves nothing listening; the second works.
                    if states.lock().unwrap().len() <= 1 {
                        testing::serve(port, testing::Reply::Mihomo, Duration::ZERO);
                    }
                    testing::answer(0, "")
                }
                ("journalctl", _) => testing::answer(0, "Parse config error: boom\n"),
                _ => testing::answer(0, ""),
            }
        });
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (mut app, mut rx) = managed_app(core);

        app.start();
        pump_until(&mut app, &mut rx, |app| {
            matches!(app.core_phase, Some(CorePhase::Failed(_)))
        })
        .await;
        let screen = draw(&app);
        assert!(screen.contains("内核启动失败"), "{screen}");
        assert!(screen.contains("Parse config error: boom"), "{screen}");
        assert_eq!(app.connection(), super::Connection::Disconnected);

        // `P` still works while the kernel is down, and `r` tries again.
        key_press(&mut app, KeyCode::Char('r'));
        assert!(matches!(app.core_phase, Some(CorePhase::Starting(_))));
        pump_until(&mut app, &mut rx, core_up).await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn stopping_the_kernel_leaves_a_page_that_says_so_and_r_attaches_again() {
        let dir = testing::scratch("app-stop");
        let port = testing::free_port();
        let recorder = testing::systemd(port, Duration::ZERO);
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (mut app, mut rx) = managed_app(core);
        app.start();
        pump_until(&mut app, &mut rx, core_up).await;
        pump_until(&mut app, &mut rx, |app| app.configs.is_some()).await;

        app.update(Action::SelectPage(Page::Settings));
        let row = app
            .settings()
            .iter()
            .position(|setting| *setting == Setting::StopCore)
            .unwrap();
        app.settings_view.cursor = row;
        key_press(&mut app, KeyCode::Enter);
        assert_eq!(app.pending, Some(Confirm::StopCore));
        key_press(&mut app, KeyCode::Char('y'));
        assert!(app.is_changing(Setting::StopCore));

        pump_until(&mut app, &mut rx, |app| {
            matches!(app.core_phase, Some(CorePhase::Stopped))
        })
        .await;
        let screen = draw(&app);
        assert!(
            screen.contains("内核未运行") && screen.contains("r 启动"),
            "{screen}"
        );
        assert_eq!(app.connection(), super::Connection::Disconnected);
        assert_eq!(
            recorder.runs("systemctl").last().unwrap(),
            &format!("--user stop {}", testing::UNIT)
        );

        // The kernel in the test answers again: `r` attaches to it.
        key_press(&mut app, KeyCode::Char('r'));
        pump_until(&mut app, &mut rx, core_up).await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_managed_kernel_that_went_away_is_said_so_and_r_starts_it() {
        let dir = testing::scratch("app-exited");
        let port = testing::free_port();
        let recorder = testing::systemd(port, Duration::ZERO);
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (mut app, mut rx) = managed_app(core);
        app.core_phase = Some(CorePhase::Running);

        app.apply_version(Err(ApiError::Connect("refused".to_owned())));
        pump_until(&mut app, &mut rx, |app| {
            app.reconnect.is_some_and(|reconnect| reconnect.exited)
        })
        .await;
        // Retrying on a timer cannot help.
        assert_eq!(app.reconnect.unwrap().retry_at, None);
        assert!(draw(&app).contains("内核已退出"));

        key_press(&mut app, KeyCode::Char('r'));
        assert!(matches!(app.core_phase, Some(CorePhase::Starting(_))));
        pump_until(&mut app, &mut rx, core_up).await;
        assert_eq!(recorder.runs("systemd-run").len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn an_external_controller_that_goes_quiet_is_not_asked_about_a_kernel() {
        let (mut app, mut rx) = app_without_controller();
        app.apply_version(Err(ApiError::Connect("refused".to_owned())));
        assert!(app.reconnect.is_some_and(|reconnect| !reconnect.exited));
        drop(app);
        // Only the startup requests were ever sent: no news from a core.
        while let Some(Envelope { msg, .. }) = rx.recv().await {
            assert!(!matches!(msg, Msg::Core(_)), "{msg:?}");
        }
    }

    #[tokio::test]
    async fn only_a_managed_connection_lists_the_managed_cores_settings() {
        let (external, _rx) = app_without_controller();
        let expected: Vec<Setting> = Setting::ALL
            .into_iter()
            .filter(|setting| {
                !matches!(
                    setting,
                    Setting::CoreBinary
                        | Setting::UpgradeCore
                        | Setting::LoginStart
                        | Setting::SystemProxy
                        | Setting::Dns
                        | Setting::SubscriptionDownload
                        | Setting::StopCore
                )
            })
            .collect();
        assert_eq!(external.settings(), expected);

        let dir = testing::scratch("app-rows");
        let recorder = testing::systemd(testing::free_port(), Duration::ZERO);
        let core = Arc::new(testing::core_at(
            &dir,
            testing::free_port(),
            &recorder,
            true,
        ));
        let (managed, _rx) = managed_app(core);
        // The kernel's own `/upgrade` gives way to the managed core's upgrade.
        let expected: Vec<Setting> = Setting::ALL
            .into_iter()
            .filter(|setting| *setting != Setting::Upgrade)
            .collect();
        assert_eq!(managed.settings(), expected);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_managed_core_is_a_row_of_the_profile_popup_and_leaving_it_stops_nothing() {
        let dir = testing::scratch("app-switch");
        let port = testing::free_port();
        let recorder = testing::systemd(port, Duration::ZERO);
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (tx, mut rx) = mpsc::channel(64);
        let overrides = crate::config::Overrides {
            url: Some("http://127.0.0.1:1".to_owned()),
            ..Default::default()
        };
        let settings = crate::config::Config::default()
            .resolve(&overrides)
            .unwrap();
        let profiles = Profiles {
            choices: vec![
                crate::config::Choice {
                    origin: Origin::Overrides,
                    name: "default".to_owned(),
                    url: "http://127.0.0.1:1".to_owned(),
                    secret: None,
                },
                managed_choice(&core),
            ],
            current: 0,
            path: None,
            core: Some(core),
        };
        let mut app = launch(settings, tx, profiles, Picker::default());
        app.start();
        assert!(app.core_phase.is_none());

        key_press(&mut app, KeyCode::Char('P'));
        assert!(draw(&app).contains("托管内核"));
        key_press(&mut app, KeyCode::Esc);
        switch_to(&mut app, 1);
        assert!(matches!(app.core_phase, Some(CorePhase::Starting(_))));
        pump_until(&mut app, &mut rx, core_up).await;

        switch_to(&mut app, 0);
        assert!(app.core_phase.is_none());
        assert_eq!(
            recorder
                .runs("systemctl")
                .iter()
                .filter(|run| run.contains("stop"))
                .count(),
            0
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A user manager that starts the unit when asked, after which the
    /// kernel answers on `port`; with `running`, it already does. `-v` of
    /// the downloaded `mihomo.new` says v1.2.3, `-v` of the kernel in use
    /// says `installed`.
    fn updating_systemd(
        port: u16,
        running: bool,
        installed: &'static str,
    ) -> Arc<testing::Recorder> {
        use std::sync::atomic::{AtomicBool, Ordering};
        if running {
            testing::serve(port, testing::Reply::Mihomo, Duration::ZERO);
            std::thread::sleep(Duration::from_millis(100));
        }
        let started = AtomicBool::new(running);
        testing::Recorder::new(move |cmd| {
            if cmd.args == ["-v"] {
                let said = if cmd.program.ends_with("mihomo.new") {
                    "Mihomo Meta v1.2.3 linux amd64\n"
                } else {
                    installed
                };
                return testing::answer(0, said);
            }
            match (cmd.program.as_str(), cmd.args.get(1).map(String::as_str)) {
                ("systemd-run", _) => {
                    testing::serve(port, testing::Reply::Mihomo, Duration::ZERO);
                    started.store(true, Ordering::SeqCst);
                    testing::answer(0, "")
                }
                ("systemctl", Some("is-active")) => {
                    if started.load(Ordering::SeqCst) {
                        testing::answer(0, "active\n")
                    } else {
                        testing::answer(3, "inactive\n")
                    }
                }
                _ => testing::answer(0, ""),
            }
        })
    }

    /// A release server whose download matches its digest.
    async fn honest_server() -> wiremock::MockServer {
        let packed = testing::gzip(testing::KERNEL);
        testing::release_server(Some(format!("sha256:{}", testing::sha256(&packed))), packed).await
    }

    #[tokio::test]
    async fn a_missing_kernel_is_downloaded_after_the_question_and_then_started() {
        let dir = testing::scratch("app-download");
        let port = testing::free_port();
        let server = honest_server().await;
        let recorder = updating_systemd(port, false, "");
        let core = Arc::new(testing::core_downloading(
            &dir,
            port,
            &recorder,
            &server.uri(),
        ));
        let (mut app, mut rx) = managed_app(core);

        app.start();
        pump_until(&mut app, &mut rx, |app| app.pending.is_some()).await;
        // The question is over the page that says there is no kernel.
        let screen = draw(&app);
        assert!(matches!(app.core_phase, Some(CorePhase::Failed(_))));
        assert!(screen.contains("下载 mihomo v1.2.3？"), "{screen}");
        assert!(!dir.join("core/bin/mihomo").exists());

        key_press(&mut app, KeyCode::Char('y'));
        assert!(matches!(
            app.core_phase,
            Some(CorePhase::Starting(Step::Downloading(_)))
        ));
        pump_until(&mut app, &mut rx, core_up).await;
        assert_eq!(
            std::fs::read(dir.join("core/bin/mihomo")).unwrap(),
            testing::KERNEL
        );
        assert_eq!(recorder.runs("systemd-run").len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn declining_the_download_leaves_a_page_that_says_so_and_r_asks_again() {
        let dir = testing::scratch("app-decline");
        let port = testing::free_port();
        let server = honest_server().await;
        let recorder = updating_systemd(port, false, "");
        let core = Arc::new(testing::core_downloading(
            &dir,
            port,
            &recorder,
            &server.uri(),
        ));
        let (mut app, mut rx) = managed_app(core);
        app.start();
        pump_until(&mut app, &mut rx, |app| app.pending.is_some()).await;

        key_press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.overlay, None);
        let screen = draw(&app);
        assert!(screen.contains("没有找到 mihomo"), "{screen}");
        assert!(screen.contains("tabby 还没有下载内核"), "{screen}");
        // Only the release was looked up; the file was never asked for.
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests
                .iter()
                .all(|request| !request.url.path().starts_with("/dl"))
        );

        key_press(&mut app, KeyCode::Char('r'));
        pump_until(&mut app, &mut rx, |app| app.pending.is_some()).await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_refused_release_lookup_is_the_page_and_says_why() {
        use wiremock::{
            Mock, ResponseTemplate,
            matchers::{method, path},
        };
        let dir = testing::scratch("app-refused");
        let port = testing::free_port();
        let server = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/MetaCubeX/mihomo/releases/latest"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_json(serde_json::json!({"message": "API rate limit exceeded"})),
            )
            .mount(&server)
            .await;
        let recorder = updating_systemd(port, false, "");
        let core = Arc::new(testing::core_downloading(
            &dir,
            port,
            &recorder,
            &server.uri(),
        ));
        let (mut app, mut rx) = managed_app(core);

        app.start();
        pump_until(&mut app, &mut rx, |app| {
            matches!(app.core_phase, Some(CorePhase::Failed(_)))
        })
        .await;

        let screen = draw(&app);
        assert!(screen.contains("无法获取内核版本"), "{screen}");
        assert!(screen.contains("API rate limit exceeded"), "{screen}");
        assert_eq!(app.pending, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// On the settings page of a managed app whose kernel runs, the cursor
    /// on `setting`.
    async fn managed_settings_on(
        core: Arc<ManagedCore>,
        setting: Setting,
    ) -> (App, mpsc::Receiver<Envelope>) {
        let (mut app, mut rx) = managed_app(core);
        app.start();
        pump_until(&mut app, &mut rx, core_up).await;
        pump_until(&mut app, &mut rx, |app| app.configs.is_some()).await;
        app.update(Action::SelectPage(Page::Settings));
        app.settings_view.cursor = app
            .settings()
            .iter()
            .position(|each| *each == setting)
            .unwrap();
        (app, rx)
    }

    fn toasted(app: &App, text: &str) -> bool {
        app.toasts.iter().any(|toast| toast.text.contains(text))
    }

    #[tokio::test]
    async fn an_upgrade_downloads_replaces_and_restarts_the_kernel() {
        let dir = testing::scratch("app-upgrade");
        let port = testing::free_port();
        let server = honest_server().await;
        let recorder = updating_systemd(port, true, "Mihomo Meta v1.2.2 linux amd64\n");
        let kept = dir.join("core/bin/mihomo");
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        std::fs::write(&kept, "old").unwrap();
        let core = Arc::new(testing::core_downloading(
            &dir,
            port,
            &recorder,
            &server.uri(),
        ));
        let (mut app, mut rx) = managed_settings_on(core, Setting::UpgradeCore).await;

        key_press(&mut app, KeyCode::Enter);
        assert!(app.is_changing(Setting::UpgradeCore));
        pump_until(&mut app, &mut rx, |app| app.pending.is_some()).await;
        assert!(draw(&app).contains("升级内核到 v1.2.3？"));
        // Nothing changes before the answer.
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "old");

        key_press(&mut app, KeyCode::Char('y'));
        pump_until(&mut app, &mut rx, |app| {
            !app.is_changing(Setting::UpgradeCore)
        })
        .await;

        assert!(toasted(&app, "已升级到 v1.2.3"), "{:?}", app.toasts);
        assert_eq!(std::fs::read(&kept).unwrap(), testing::KERNEL);
        assert_eq!(
            recorder.runs("systemctl").last().unwrap(),
            &format!("--user restart {}", testing::UNIT)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_newest_kernel_is_told_so_and_nothing_is_downloaded() {
        let dir = testing::scratch("app-current");
        let port = testing::free_port();
        let server = honest_server().await;
        let recorder = updating_systemd(port, true, "Mihomo Meta v1.2.3 linux amd64\n");
        let kept = dir.join("core/bin/mihomo");
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        std::fs::write(&kept, "kernel").unwrap();
        let core = Arc::new(testing::core_downloading(
            &dir,
            port,
            &recorder,
            &server.uri(),
        ));
        let (mut app, mut rx) = managed_settings_on(core, Setting::UpgradeCore).await;

        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, |app| {
            !app.is_changing(Setting::UpgradeCore)
        })
        .await;

        assert!(toasted(&app, "已是最新（v1.2.3）"), "{:?}", app.toasts);
        assert_eq!(app.pending, None);
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "kernel");
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests
                .iter()
                .all(|request| !request.url.path().starts_with("/dl"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_kernel_of_the_users_own_is_not_upgraded_and_the_row_says_so() {
        let dir = testing::scratch("app-own-kernel");
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (mut app, _rx) = managed_settings_on(core, Setting::UpgradeCore).await;

        assert!(draw(&app).contains("由你自己管理"));
        key_press(&mut app, KeyCode::Enter);

        assert!(toasted(&app, "由你自己管理"), "{:?}", app.toasts);
        assert!(!app.is_changing(Setting::UpgradeCore));
        assert_eq!(app.pending, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    const SUB_YAML: &str = "proxies:\n  - {name: a, type: ss, server: 1.2.3.4, port: 1, cipher: aes-128-gcm, password: p}\nrules:\n  - MATCH,DIRECT\n";

    /// A subscription server for `/sub`: `body`, named 机场 with 1 GB of
    /// 10 GB used.
    async fn subscription_server(status: u16, body: &str) -> wiremock::MockServer {
        use wiremock::{
            Mock, ResponseTemplate,
            matchers::{method, path},
        };
        let server = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/sub"))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_string(body)
                    .insert_header(
                        "content-disposition",
                        "attachment; filename*=UTF-8''%E6%9C%BA%E5%9C%BA.yaml",
                    )
                    .insert_header(
                        "subscription-userinfo",
                        "upload=0; download=1073741824; total=10737418240",
                    ),
            )
            .mount(&server)
            .await;
        server
    }

    /// A managed app on the subscriptions page with the kernel running and
    /// two subscriptions imported (`机场` from `server`, active; `local`
    /// from a file), and the list read.
    async fn subscriptions_app(
        name: &str,
        server: &wiremock::MockServer,
    ) -> (App, mpsc::Receiver<Envelope>, std::path::PathBuf, u16) {
        let dir = testing::scratch(name);
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        core.import_url(&format!("{}/sub?token=SECRET", server.uri()), None, |_| {})
            .await
            .unwrap();
        let file = dir.join("local.yaml");
        std::fs::write(&file, SUB_YAML).unwrap();
        core.import_file(&file, None).await.unwrap();
        let (mut app, mut rx) = managed_app(core);
        app.start();
        pump_until(&mut app, &mut rx, core_up).await;
        pump_until(&mut app, &mut rx, |app| {
            app.subscriptions_view.list.is_some()
        })
        .await;
        key_press(&mut app, KeyCode::Char('8'));
        assert_eq!(app.page, Page::Subscriptions);
        (app, rx, dir, port)
    }

    #[tokio::test]
    async fn an_external_controller_has_no_subscriptions_and_says_who_manages_them() {
        let (mut app, _rx) = app_without_controller();
        key_press(&mut app, KeyCode::Char('8'));
        assert_eq!(app.page, Page::Subscriptions);

        let screen = draw(&app);
        assert!(
            screen.contains("连接的是外部控制器，订阅由它自己管理"),
            "{screen}"
        );
        assert!(screen.contains("P 切换到托管内核"), "{screen}");
        // Its keys have nothing to act on.
        for key in ['a', 'u', 'U', 'd'] {
            key_press(&mut app, KeyCode::Char(key));
        }
        key_press(&mut app, KeyCode::Enter);
        assert!(!app.typing() && app.pending.is_none() && app.toasts.is_empty());
        // The eighth tab is there, with its number alone at 60 columns too.
        app.update(Action::Resize(Size::new(60, 20)));
        assert!(draw(&app).contains('8'));
    }

    #[tokio::test]
    async fn the_list_marks_the_one_in_use_and_shows_each_ones_kind_age_and_usage() {
        let server = subscription_server(200, SUB_YAML).await;
        let (app, _rx, dir, _) = subscriptions_app("subs-list", &server).await;

        let screen = draw(&app);

        let rows: Vec<&str> = screen
            .lines()
            .filter(|line| line.contains("刚刚"))
            .collect();
        assert_eq!(rows.len(), 2, "{screen}");
        assert!(
            rows[0].contains("✓ 机场") && rows[0].contains("URL"),
            "{}",
            rows[0]
        );
        assert!(rows[0].contains("1.0 GB / 10.0 GB"), "{}", rows[0]);
        assert!(
            rows[1].contains("local") && rows[1].contains("文件"),
            "{}",
            rows[1]
        );
        assert!(!rows[1].contains('✓'), "{}", rows[1]);
        assert!(screen.contains("订阅  2"), "{screen}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn an_address_typed_into_the_box_is_imported_and_the_new_row_is_under_the_cursor() {
        let server = subscription_server(200, SUB_YAML).await;
        let (mut app, mut rx, dir, _) = subscriptions_app("subs-add", &server).await;

        key_press(&mut app, KeyCode::Char('a'));
        assert!(app.typing() && app.subscription_prompt().is_some());
        // Keys that mean something elsewhere are text in the box.
        type_text(&mut app, &format!("{}/sub?x=1", server.uri()));
        assert_eq!(app.page, Page::Subscriptions);
        key_press(&mut app, KeyCode::Enter);
        assert!(!app.typing() && app.subscriptions_view.importing);
        assert!(draw(&app).contains("正在导入"));

        pump_until(&mut app, &mut rx, |app| !app.subscriptions_view.importing).await;

        assert!(toasted(&app, "已导入「机场」"), "{:?}", app.toasts);
        let view = &app.subscriptions_view;
        assert_eq!(view.visible().len(), 3);
        assert_eq!(view.selected().unwrap().name, "机场");
        // The first was already in use: the new one is not.
        assert_ne!(view.active(), view.selected().map(|sub| sub.id.as_str()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_failed_import_says_why_and_a_path_is_taken_for_a_file() {
        let server = subscription_server(200, SUB_YAML).await;
        let (mut app, mut rx, dir, _) = subscriptions_app("subs-add-fail", &server).await;

        key_press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, &format!("{}/missing.yaml", dir.display()));
        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, |app| !app.subscriptions_view.importing).await;
        assert!(
            toasted(&app, "导入失败：原文件已不存在"),
            "{:?}",
            app.toasts
        );

        // Esc drops what was typed; an empty box imports nothing.
        key_press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, "http://x");
        key_press(&mut app, KeyCode::Esc);
        assert!(app.subscriptions_view.adding.is_none() && !app.typing());
        key_press(&mut app, KeyCode::Char('a'));
        key_press(&mut app, KeyCode::Enter);
        assert!(!app.subscriptions_view.importing);
        assert_eq!(app.subscriptions_view.visible().len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn enter_switches_the_subscription_and_the_kernel_is_asked_to_read_its_config_again() {
        let server = subscription_server(200, SUB_YAML).await;
        let (mut app, mut rx, dir, port) = subscriptions_app("subs-switch", &server).await;
        let before = testing::requests_to(port).len();

        // On the one in use there is nothing to do.
        key_press(&mut app, KeyCode::Enter);
        assert!(toasted(&app, "「机场」已在使用"));
        assert!(app.subscriptions_view.busy.is_empty());

        key_press(&mut app, KeyCode::Char('j'));
        key_press(&mut app, KeyCode::Enter);
        assert!(!app.subscriptions_view.busy.is_empty());
        pump_until(&mut app, &mut rx, |app| {
            app.subscriptions_view.busy.is_empty()
        })
        .await;
        assert!(toasted(&app, "已切换到「local」"), "{:?}", app.toasts);
        // Wait for the reload's reply, which refreshes the pages behind it.
        pump_until(&mut app, &mut rx, |_| {
            testing::requests_to(port)[before..]
                .iter()
                .any(|request| request.starts_with("PUT /configs"))
        })
        .await;

        let config = std::fs::read_to_string(dir.join("core/home/config.yaml")).unwrap();
        assert!(config.starts_with(SUB_YAML), "{config}");
        assert!(draw(&app).contains("✓ local"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_download_under_way_shows_how_much_came_on_its_row_until_it_ends() {
        let server = subscription_server(200, SUB_YAML).await;
        let (mut app, _rx, dir, _) = subscriptions_app("subs-progress", &server).await;
        let id = app.subscriptions_view.selected().unwrap().id.clone();
        app.subscriptions_view.busy.insert(id.clone());
        let progress = |attempt, received, done| {
            Msg::Subscription(SubEvent::Progress {
                fetching: Fetching::Subscription(id.clone()),
                progress: crate::core::Progress {
                    attempt,
                    answered: true,
                    received,
                    total: Some(16 * 1024),
                    done,
                },
            })
        };
        app.handle_msg(progress(1, 8 * 1024, false));
        let row = |app: &App| {
            draw(app)
                .lines()
                .find(|line| line.contains("机场") && line.contains("URL"))
                .unwrap()
                .to_owned()
        };
        assert!(row(&app).contains("8.0 KB/16.0 KB"), "{}", row(&app));
        // A second try says so.
        app.handle_msg(progress(2, 1024, false));
        assert!(
            row(&app).contains("第 2 次 · 1.0 KB/16.0 KB"),
            "{}",
            row(&app)
        );
        // All there: the kernel checks it now.
        app.handle_msg(progress(2, 16 * 1024, true));
        assert!(
            row(&app).contains("第 2 次 · 16.0 KB · 检查中…"),
            "{}",
            row(&app)
        );

        app.handle_msg(Msg::Subscription(SubEvent::Done {
            work: subscriptions::Work::Update(id),
            result: Err(crate::core::CoreError::Fetch("no data for 20s".to_owned())),
        }));
        // The row shows its usage again, and the failure is said.
        assert!(row(&app).contains("1.0 GB / 10.0 GB"), "{}", row(&app));
        assert!(app.subscriptions_view.transfers.is_empty());
        assert!(
            toasted(&app, "更新「机场」失败：no data for 20s"),
            "{:?}",
            app.toasts
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_subscription_in_use_cannot_be_deleted_and_another_goes_after_a_question() {
        let server = subscription_server(200, SUB_YAML).await;
        let (mut app, mut rx, dir, _) = subscriptions_app("subs-delete", &server).await;

        key_press(&mut app, KeyCode::Char('d'));
        assert!(toasted(&app, "先切换到其他订阅"));
        assert!(app.pending.is_none());

        key_press(&mut app, KeyCode::Char('j'));
        key_press(&mut app, KeyCode::Char('d'));
        assert!(draw(&app).contains("删除订阅「local」及其文件？"));
        // Declined, nothing goes.
        key_press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.subscriptions_view.visible().len(), 2);
        key_press(&mut app, KeyCode::Char('d'));
        key_press(&mut app, KeyCode::Char('y'));
        pump_until(&mut app, &mut rx, |app| {
            app.subscriptions_view.visible().len() == 1
        })
        .await;
        assert!(toasted(&app, "已删除「local」"));
        assert_eq!(app.subscriptions_view.visible()[0].name, "机场");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_failed_update_is_a_toast_and_the_row_stops_spinning() {
        let server = subscription_server(200, SUB_YAML).await;
        let (mut app, mut rx, dir, _) = subscriptions_app("subs-update", &server).await;

        server.reset().await;
        key_press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.subscriptions_view.busy.len(), 1);
        // A second press while it runs is not a second update.
        key_press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.subscriptions_view.busy.len(), 1);
        pump_until(&mut app, &mut rx, |app| {
            app.subscriptions_view.busy.is_empty()
        })
        .await;

        assert!(toasted(&app, "更新「机场」失败"), "{:?}", app.toasts);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_filter_narrows_the_list_and_a_bulk_update_asks_about_what_is_shown() {
        let server = subscription_server(200, SUB_YAML).await;
        let (mut app, _rx, dir, _) = subscriptions_app("subs-filter", &server).await;

        key_press(&mut app, KeyCode::Char('U'));
        assert!(draw(&app).contains("更新全部 2 个订阅？"));
        key_press(&mut app, KeyCode::Char('n'));

        key_press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "LOC");
        key_press(&mut app, KeyCode::Enter);
        assert_eq!(app.subscriptions_view.visible().len(), 1);
        let screen = draw(&app);
        assert!(
            screen.contains("1/2 · 过滤「LOC」") && !screen.contains("机场"),
            "{screen}"
        );
        key_press(&mut app, KeyCode::Char('U'));
        assert!(draw(&app).contains("更新匹配「LOC」的 1 个订阅？"));
        key_press(&mut app, KeyCode::Char('n'));

        type_filter_none(&mut app);
        assert_eq!(app.subscriptions_view.visible().len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `Esc` on the page: the filter goes, the keyboard is the page's.
    fn type_filter_none(app: &mut App) {
        key_press(app, KeyCode::Esc);
    }

    #[test]
    fn the_subscription_keys_are_the_pages_own() {
        for (key, expected) in [
            ('a', Action::AddSubscription),
            ('u', Action::UpdateSubscription),
            ('U', Action::UpdateSubscriptions),
            ('d', Action::DeleteSubscription),
            ('e', Action::RenameSubscription),
            ('i', Action::EditInterval),
            ('K', Action::ChangeAgeKey),
            ('/', Action::StartFilter),
        ] {
            assert_eq!(
                press(key, Page::Subscriptions, false),
                Some(expected),
                "{key}"
            );
        }
        // Elsewhere they are what they were, or nothing.
        assert_eq!(press('a', Page::Rules, false), None);
        assert_eq!(press('d', Page::Rules, false), None);
        assert_eq!(
            press('u', Page::Providers, false),
            Some(Action::UpdateProvider)
        );
        assert_eq!(
            press('8', Page::Rules, false),
            Some(Action::SelectPage(Page::Subscriptions))
        );
        // And in the box they are text.
        assert_eq!(
            press('a', Page::Subscriptions, true),
            Some(Action::FilterInput(KeyEvent::new(
                KeyCode::Char('a'),
                KeyModifiers::NONE
            )))
        );
    }

    #[tokio::test]
    async fn a_subscription_is_renamed_and_given_an_interval_in_the_box_at_the_foot() {
        let server = subscription_server(200, SUB_YAML).await;
        let (mut app, mut rx, dir, _) = subscriptions_app("subs-edit", &server).await;
        let typed = |app: &App| {
            app.subscriptions_view
                .adding
                .as_ref()
                .unwrap()
                .value()
                .to_owned()
        };

        // The name comes up in the box, to be changed.
        key_press(&mut app, KeyCode::Char('e'));
        assert!(app.typing());
        assert_eq!(typed(&app), "机场");
        for _ in 0..2 {
            key_press(&mut app, KeyCode::Backspace);
        }
        type_text(&mut app, "家里");
        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, |app| toasted(app, "已改名为「家里」")).await;
        assert_eq!(app.subscriptions_view.selected().unwrap().name, "家里");

        // Hours are digits; one that is not a number of hours keeps the box
        // open with what was typed.
        key_press(&mut app, KeyCode::Char('i'));
        assert_eq!(typed(&app), "24");
        type_text(&mut app, "ab");
        assert_eq!(typed(&app), "24");
        for _ in 0..2 {
            key_press(&mut app, KeyCode::Backspace);
        }
        type_text(&mut app, "0");
        key_press(&mut app, KeyCode::Enter);
        assert!(toasted(&app, "间隔须是 1–8760 小时"), "{:?}", app.toasts);
        assert!(app.typing());
        assert_eq!(typed(&app), "0");
        key_press(&mut app, KeyCode::Backspace);
        type_text(&mut app, "6");
        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, |app| toasted(app, "每 6 小时自动更新")).await;
        assert_eq!(app.subscriptions_view.selected().unwrap().interval_hours, 6);

        // A file is read by hand, so it has no interval; a subscription
        // that is not encrypted has no key to change.
        key_press(&mut app, KeyCode::Char('j'));
        key_press(&mut app, KeyCode::Char('i'));
        assert!(!app.typing() && toasted(&app, "文件订阅不自动更新"));
        key_press(&mut app, KeyCode::Char('k'));
        key_press(&mut app, KeyCode::Char('K'));
        assert!(!app.typing() && toasted(&app, "这份订阅没有加密"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Waits until `reached` holds of the stand-in controller's requests.
    async fn requested(
        app: &mut App,
        rx: &mut mpsc::Receiver<Envelope>,
        port: u16,
        reached: impl Fn(&[String]) -> bool,
    ) {
        pump_until(app, rx, |_| reached(&testing::requests_to(port))).await;
    }

    fn settings_toml(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("core/settings.toml")).unwrap_or_default()
    }

    #[tokio::test]
    async fn what_is_changed_on_a_managed_kernel_is_kept_for_its_next_start() {
        let dir = testing::scratch("app-persist");
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (mut app, mut rx) = managed_settings_on(Arc::clone(&core), Setting::AllowLan).await;

        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, |app| !app.is_changing(Setting::AllowLan)).await;
        // `m`: the next mode, as the header and the overview have it.
        app.update(Action::SelectPage(Page::Overview));
        key_press(&mut app, KeyCode::Char('m'));
        pump_until(&mut app, &mut rx, |app| !app.is_changing(Setting::Mode)).await;
        // The Mixed port, typed.
        app.update(Action::SelectPage(Page::Settings));
        app.settings_view.cursor = app
            .settings()
            .iter()
            .position(|setting| *setting == Setting::Port(Port::Mixed))
            .unwrap();
        key_press(&mut app, KeyCode::Enter);
        for _ in 0..5 {
            key_press(&mut app, KeyCode::Backspace);
        }
        type_text(&mut app, "18080");
        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, |app| {
            !app.is_changing(Setting::Port(Port::Mixed))
        })
        .await;

        let saved = core.settings();
        assert!(saved.allow_lan);
        assert_eq!(saved.mixed_port, 18080);
        assert_eq!(saved.mode, "global");
        let text = settings_toml(&dir);
        assert!(
            text.contains("mixed-port = 18080") && text.contains("allow-lan = true"),
            "{text}"
        );

        // A kernel started afresh over the same files runs with them.
        drop(core);
        let again = crate::core::testing::core_at(&dir, testing::free_port(), &recorder, true);
        assert_eq!(
            (again.settings().mixed_port, again.settings().mode.as_str()),
            (18080, "global")
        );
        again.regenerate().await.unwrap();
        let config_now = std::fs::read_to_string(dir.join("core/home/config.yaml")).unwrap();
        assert!(
            config_now.contains("\"mixed-port\": 18080")
                && config_now.contains("\"allow-lan\": true"),
            "{config_now}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_subscription_download_row_steps_through_its_ways_and_asks_for_a_proxy() {
        let dir = testing::scratch("app-download-way");
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (mut app, _rx) =
            managed_settings_on(Arc::clone(&core), Setting::SubscriptionDownload).await;
        let row = |app: &App| {
            draw(app)
                .lines()
                .find(|line| line.contains("订阅下载"))
                .unwrap()
                .to_owned()
        };
        assert!(row(&app).contains("直连"), "{}", row(&app));

        key_press(&mut app, KeyCode::Enter);
        assert_eq!(core.settings().subscription_download.via, Route::Core);
        assert!(
            row(&app).contains("经托管内核  127.0.0.1:7890"),
            "{}",
            row(&app)
        );

        // Next is a proxy: the box asks which, with the usual one in it.
        key_press(&mut app, KeyCode::Enter);
        assert!(app.typing());
        assert!(row(&app).contains("http://127.0.0.1:7890"), "{}", row(&app));
        assert_eq!(core.settings().subscription_download.via, Route::Core);
        // Not an address: refused, the box stays.
        for _ in 0.."http://127.0.0.1:7890".len() {
            key_press(&mut app, KeyCode::Backspace);
        }
        type_text(&mut app, "127.0.0.1:7890");
        key_press(&mut app, KeyCode::Enter);
        assert!(
            app.typing() && toasted(&app, "代理须是"),
            "{:?}",
            app.toasts
        );
        // Letters are typed, not taken as keys of the page.
        for _ in 0.."127.0.0.1:7890".len() {
            key_press(&mut app, KeyCode::Backspace);
        }
        type_text(&mut app, "socks5://me:pw@127.0.0.1:1080");
        key_press(&mut app, KeyCode::Enter);
        assert!(!app.typing());
        let download = core.settings().subscription_download;
        assert_eq!(
            (download.via, download.proxy.as_str()),
            (Route::Proxy, "socks5://me:pw@127.0.0.1:1080")
        );
        // The password is not shown.
        assert!(
            row(&app).contains("经代理  socks5://127.0.0.1:1080"),
            "{}",
            row(&app)
        );
        assert!(settings_toml(&dir).contains("via = \"proxy\""));

        // Then directly again; the proxy is kept for next time.
        key_press(&mut app, KeyCode::Enter);
        let download = core.settings().subscription_download;
        assert_eq!(download.via, Route::Direct);
        key_press(&mut app, KeyCode::Enter);
        key_press(&mut app, KeyCode::Enter);
        assert!(
            row(&app).contains("socks5://me:pw@127.0.0.1:1080"),
            "{}",
            row(&app)
        );
        // Esc leaves it as it was.
        key_press(&mut app, KeyCode::Esc);
        assert!(!app.typing());
        assert_eq!(core.settings().subscription_download.via, Route::Core);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_dns_popup_keeps_what_the_kernel_accepts_and_the_kernel_reads_it_when_it_closes() {
        let dir = testing::scratch("app-dns");
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (mut app, mut rx) = managed_settings_on(Arc::clone(&core), Setting::Dns).await;
        assert!(draw(&app).contains("DNS 覆盖"));

        key_press(&mut app, KeyCode::Enter);
        assert_eq!(app.overlay, Some(Overlay::Dns));
        let screen = draw(&app);
        assert!(
            screen.contains("主 DNS") && screen.contains("nameserver"),
            "{screen}"
        );

        // A switch goes round: the subscription's own, on, off.
        key_press(&mut app, KeyCode::Char('j'));
        key_press(&mut app, KeyCode::Enter);
        assert!(app.dns_view.checking);
        pump_until(&mut app, &mut rx, |app| !app.dns_view.checking).await;
        assert_eq!(core.settings().dns["ipv6"], toml::Value::Boolean(true));

        // A list is typed, commas or spaces between its items.
        for _ in 0..6 {
            key_press(&mut app, KeyCode::Char('j'));
        }
        key_press(&mut app, KeyCode::Enter);
        assert!(app.typing());
        type_text(&mut app, "1.1.1.1, tls://8.8.8.8");
        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, |app| !app.dns_view.checking).await;
        let screen = draw(&app);
        assert!(screen.contains("1.1.1.1, tls://8.8.8.8"), "{screen}");
        // The config is written from the settings by the core, which the
        // core's own tests look at.
        assert!(settings_toml(&dir).contains("tls://8.8.8.8"));

        // `Esc` while typing drops the typing, and not the popup.
        key_press(&mut app, KeyCode::Enter);
        assert!(app.typing());
        key_press(&mut app, KeyCode::Esc);
        assert!(!app.typing() && app.overlay == Some(Overlay::Dns));

        // Closed, the kernel is asked to read the file again, once.
        let before = testing::requests_to(port).len();
        key_press(&mut app, KeyCode::Esc);
        assert_eq!(app.overlay, None);
        requested(&mut app, &mut rx, port, |requests| {
            requests[before..]
                .iter()
                .any(|request| request.starts_with("PUT /configs"))
        })
        .await;
        assert!(draw(&app).contains("2 项已覆盖"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_dns_value_the_kernel_refuses_is_said_in_its_words() {
        let (mut app, _rx) = app_without_controller();
        app.dns_view.checking = true;

        app.on_dns(
            "nameserver",
            Err(crate::core::CoreError::Invalid("bad.example".to_owned())),
        );

        assert!(!app.dns_view.checking);
        assert!(
            toasted(&app, "内核不接受 nameserver：bad.example"),
            "{:?}",
            app.toasts
        );
    }

    #[tokio::test]
    async fn a_managed_kernel_has_its_mixed_port_alone_and_the_others_say_so() {
        let dir = testing::scratch("app-ports");
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (mut app, _rx) = managed_settings_on(core, Setting::Port(Port::Http)).await;

        assert!(draw(&app).contains("托管内核只用 Mixed 端口"));
        key_press(&mut app, KeyCode::Enter);
        assert!(app.settings_view.editing.is_none());
        assert!(toasted(&app, "托管内核只用 Mixed 端口"), "{:?}", app.toasts);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn reload_on_a_managed_kernel_writes_the_config_again_before_the_kernel_reads_it() {
        let dir = testing::scratch("app-regenerate");
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let file = dir.join("a.yaml");
        std::fs::write(&file, SUB_YAML).unwrap();
        core.import_file(&file, None).await.unwrap();
        let (mut app, mut rx) = managed_settings_on(Arc::clone(&core), Setting::Reload).await;
        let config = dir.join("core/home/config.yaml");
        std::fs::write(&config, "stale").unwrap();
        let before = testing::requests_to(port).len();

        key_press(&mut app, KeyCode::Enter);
        assert!(draw(&app).contains("用当前订阅重新生成配置？"));
        key_press(&mut app, KeyCode::Char('y'));
        requested(&mut app, &mut rx, port, |requests| {
            requests[before..]
                .iter()
                .any(|request| request.starts_with("PUT /configs"))
        })
        .await;

        assert!(
            std::fs::read_to_string(&config)
                .unwrap()
                .starts_with(SUB_YAML)
        );
        pump_until(&mut app, &mut rx, |app| !app.is_changing(Setting::Reload)).await;
        assert!(toasted(&app, "已重载配置"), "{:?}", app.toasts);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn an_external_controller_keeps_asking_the_kernel_to_reload_and_has_every_port() {
        let (mut app, _rx) = settings_app("http://127.0.0.1:1", true, MIHOMO_CONFIGS);
        select(&mut app, Setting::Reload);
        key_press(&mut app, KeyCode::Enter);
        assert!(draw(&app).contains("重新加载配置文件？"));
        key_press(&mut app, KeyCode::Char('n'));
        select(&mut app, Setting::Port(Port::Http));
        key_press(&mut app, KeyCode::Enter);
        assert!(app.settings_view.editing.is_some());
    }

    /// A machine whose kernel answers on `port`, and whose `getcap` says it
    /// may manage the network once `setcap` ran: through `pkexec` (which
    /// exits `pkexec`) or `sudo` (`sudo`); either has to exit 0 for that.
    /// `capable` is whether it already may.
    fn tun_machine(
        port: u16,
        pkexec: i32,
        sudo: i32,
        capable: Arc<std::sync::atomic::AtomicBool>,
    ) -> Arc<testing::Recorder> {
        use std::sync::atomic::Ordering;
        testing::serve(port, testing::Reply::Mihomo, Duration::ZERO);
        testing::Recorder::new(move |cmd| match cmd.program.as_str() {
            "getcap" if capable.load(Ordering::SeqCst) => {
                testing::answer(0, "/x cap_net_admin,cap_net_bind_service=ep\n")
            }
            "pkexec" | "sudo" => {
                let code = if cmd.program == "pkexec" {
                    pkexec
                } else {
                    sudo
                };
                if code == 0 {
                    capable.store(true, Ordering::SeqCst);
                }
                testing::answer(code, "")
            }
            "systemctl" if cmd.args.get(1).map(String::as_str) == Some("is-active") => {
                testing::answer(0, "active\n")
            }
            _ => testing::answer(0, ""),
        })
    }

    /// The managed settings page of `core`, on the TUN row.
    async fn on_tun_row(core: &Arc<ManagedCore>) -> (App, mpsc::Receiver<Envelope>) {
        let (mut app, mut rx) = managed_settings_on(Arc::clone(core), Setting::Tun).await;
        // What `getcap` said at the start is in.
        pump_until(&mut app, &mut rx, |app| app.tun_authorized.is_some()).await;
        (app, rx)
    }

    #[tokio::test]
    async fn tun_asks_for_the_capabilities_first_and_is_turned_on_once_the_kernel_has_them() {
        use std::sync::atomic::AtomicBool;
        let dir = testing::scratch("app-tun-auth");
        let port = testing::free_port();
        let recorder = tun_machine(port, 0, 1, Arc::new(AtomicBool::new(false)));
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (mut app, mut rx) = on_tun_row(&core).await;
        assert_eq!(app.tun_authorized, Some(false));
        let before = testing::requests_to(port).len();

        key_press(&mut app, KeyCode::Enter);
        let screen = draw(&app);
        assert!(screen.contains("开启 TUN 需要授权"), "{screen}");
        assert!(
            screen.contains("CAP_NET_ADMIN 和 CAP_NET_BIND_SERVICE"),
            "{screen}"
        );
        // Nothing is asked of the system before the answer.
        assert_eq!(recorder.runs("pkexec"), Vec::<String>::new());

        key_press(&mut app, KeyCode::Char('y'));
        assert!(app.is_changing(Setting::Tun));
        requested(&mut app, &mut rx, port, |requests| {
            requests[before..]
                .iter()
                .any(|request| request.starts_with("PATCH /configs"))
        })
        .await;

        // The capability was set on the kernel alone, through the desktop's
        // dialog; the kernel was started again to have it, and TUN is on.
        assert_eq!(
            recorder.runs("pkexec"),
            [format!(
                "/usr/bin/setcap cap_net_admin,cap_net_bind_service=+ep {}",
                dir.join("mihomo").display()
            )]
        );
        assert_eq!(recorder.runs("sudo"), Vec::<String>::new());
        assert_eq!(
            recorder.runs("systemctl").last().unwrap(),
            &format!("--user restart {}", testing::UNIT)
        );
        assert_eq!(app.tun_authorized, Some(true));
        pump_until(&mut app, &mut rx, |app| !app.is_changing(Setting::Tun)).await;
        assert!(
            settings_toml(&dir).contains("enable = true"),
            "{}",
            settings_toml(&dir)
        );
        assert!(app.take_suspend().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_kernel_that_may_already_manage_the_network_is_only_asked_the_usual_question() {
        use std::sync::atomic::AtomicBool;
        let dir = testing::scratch("app-tun-authorized");
        let port = testing::free_port();
        let recorder = tun_machine(port, 0, 1, Arc::new(AtomicBool::new(true)));
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (mut app, mut rx) = on_tun_row(&core).await;
        assert_eq!(app.tun_authorized, Some(true));

        key_press(&mut app, KeyCode::Enter);
        assert!(draw(&app).contains("开启 TUN？内核会接管本机流量"));
        key_press(&mut app, KeyCode::Char('y'));
        pump_until(&mut app, &mut rx, |app| !app.is_changing(Setting::Tun)).await;
        assert_eq!(recorder.runs("pkexec"), Vec::<String>::new());
        assert_eq!(
            recorder
                .runs("systemctl")
                .iter()
                .filter(|run| run.contains("restart"))
                .count(),
            0
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn saying_no_to_the_dialog_leaves_tun_off_and_the_kernel_alone() {
        use std::sync::atomic::AtomicBool;
        let dir = testing::scratch("app-tun-cancel");
        let port = testing::free_port();
        let recorder = tun_machine(port, 126, 1, Arc::new(AtomicBool::new(false)));
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (mut app, mut rx) = on_tun_row(&core).await;
        let before = testing::requests_to(port).len();

        key_press(&mut app, KeyCode::Enter);
        key_press(&mut app, KeyCode::Char('y'));
        pump_until(&mut app, &mut rx, |app| !app.is_changing(Setting::Tun)).await;

        assert!(
            toasted(&app, "已取消授权，TUN 保持关闭"),
            "{:?}",
            app.toasts
        );
        assert_eq!(recorder.runs("sudo"), Vec::<String>::new());
        assert_eq!(
            recorder
                .runs("systemctl")
                .iter()
                .filter(|run| run.contains("restart"))
                .count(),
            0
        );
        assert!(
            testing::requests_to(port)[before..]
                .iter()
                .all(|request| !request.starts_with("PATCH"))
        );
        assert!(app.take_suspend().is_none());
        assert!(!settings_toml(&dir).contains("enable = true"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn without_a_polkit_agent_the_terminal_is_asked_for_and_what_sudo_says_decides() {
        use std::sync::atomic::AtomicBool;
        for (sudo, ok) in [(0, true), (1, false)] {
            let dir = testing::scratch("app-tun-sudo");
            let port = testing::free_port();
            // 127: pkexec found no one to ask.
            let recorder = tun_machine(port, 127, sudo, Arc::new(AtomicBool::new(false)));
            let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
            let (mut app, mut rx) = on_tun_row(&core).await;
            let before = testing::requests_to(port).len();

            key_press(&mut app, KeyCode::Enter);
            key_press(&mut app, KeyCode::Char('y'));
            pump_until(&mut app, &mut rx, |app| app.suspend.is_some()).await;

            // The main loop is asked for the terminal; until it has run `sudo`
            // nothing else happens.
            let lent = app.take_suspend().expect("the terminal is asked for");
            assert_eq!(recorder.runs("sudo"), Vec::<String>::new());
            let result = lent.authorize_terminal().await;
            assert_eq!(
                recorder.runs("sudo"),
                [format!(
                    "setcap cap_net_admin,cap_net_bind_service=+ep {}",
                    dir.join("mihomo").display()
                )]
            );
            app.finish_terminal(result);
            if ok {
                requested(&mut app, &mut rx, port, |requests| {
                    requests[before..]
                        .iter()
                        .any(|request| request.starts_with("PATCH /configs"))
                })
                .await;
                assert_eq!(app.tun_authorized, Some(true));
            } else {
                assert!(toasted(&app, "授权没有完成"), "{:?}", app.toasts);
                assert!(!app.is_changing(Setting::Tun));
                assert!(
                    testing::requests_to(port)[before..]
                        .iter()
                        .all(|request| !request.starts_with("PATCH"))
                );
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[tokio::test]
    async fn a_kernel_that_lost_its_capabilities_starts_without_tun_and_says_to_authorize_again() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = testing::scratch("app-tun-lost");
        let port = testing::free_port();
        let capable = Arc::new(AtomicBool::new(true));
        let recorder = tun_machine(port, 0, 1, Arc::clone(&capable));
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        core.persist(crate::core::Persist::Tun(true)).unwrap();
        let (mut app, mut rx) = managed_settings_on(Arc::clone(&core), Setting::Restart).await;
        assert!(core.settings().tun.enable);

        // The kernel is replaced (an upgrade, a package manager): no file
        // capability on the new one. Then it is started again.
        capable.store(false, Ordering::SeqCst);
        key_press(&mut app, KeyCode::Enter);
        key_press(&mut app, KeyCode::Char('y'));
        pump_until(&mut app, &mut rx, |app| !app.is_changing(Setting::Restart)).await;

        assert!(
            toasted(&app, "内核已更新，TUN 需要重新授权"),
            "{:?}",
            app.toasts
        );
        assert!(!core.settings().tun.enable);
        assert!(settings_toml(&dir).contains("enable = false"));
        let config = std::fs::read_to_string(dir.join("core/home/config.yaml")).unwrap();
        assert!(config.contains("\"enable\":false"), "{config}");
        pump_until(&mut app, &mut rx, |app| app.tun_authorized == Some(false)).await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_kernel_on_a_nosuid_mount_is_told_so_before_anything_is_asked() {
        use std::sync::atomic::AtomicBool;
        let dir = testing::scratch("app-tun-nosuid");
        let port = testing::free_port();
        let recorder = tun_machine(port, 0, 0, Arc::new(AtomicBool::new(false)));
        let core = Arc::new(testing::core_on_nosuid_mount(&dir, port, &recorder));
        let (mut app, _rx) = on_tun_row(&core).await;

        key_press(&mut app, KeyCode::Enter);

        assert!(app.pending.is_none() && app.overlay.is_none());
        assert!(
            toasted(
                &app,
                "以 nosuid 挂载，无法授权，请在 [core].binary 指定其他位置"
            ),
            "{:?}",
            app.toasts
        );
        assert_eq!(recorder.runs("pkexec"), Vec::<String>::new());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_toast_too_long_for_a_line_goes_over_more_and_ends_in_an_ellipsis_past_three() {
        let (mut app, _rx) = app_without_controller();
        app.toast(
            ToastKind::Error,
            "内核所在分区（/tmp）以 nosuid 挂载，无法授权，请在 [core].binary 指定其他位置"
                .to_owned(),
        );
        let screen = draw(&app);
        assert!(
            screen.contains("[core].binary") && screen.contains("其他位置"),
            "{screen}"
        );

        app.toasts.clear();
        app.toast(ToastKind::Error, "很长的话".repeat(60));
        let screen = draw(&app);
        let rows = screen
            .lines()
            .filter(|line| line.contains("很长的话"))
            .count();
        assert_eq!(rows, 3, "{screen}");
        assert!(screen.contains('…'), "{screen}");
    }

    /// Makes every subscription of the managed core in `dir` look as if it
    /// was last updated `hours` ago.
    fn age_subscriptions(dir: &std::path::Path, hours: i64) {
        let path = dir.join("core/subscriptions.toml");
        let mut store = crate::core::Store::load(&path).unwrap();
        let then = jiff::Timestamp::now().as_second() - hours * 3600;
        for sub in &mut store.subscriptions {
            sub.updated_at = then;
        }
        store.save(&path).unwrap();
    }

    async fn requests_for(server: &wiremock::MockServer) -> usize {
        server.received_requests().await.unwrap().len()
    }

    #[tokio::test]
    async fn a_subscription_whose_interval_has_passed_is_updated_in_the_background_without_a_word()
    {
        let server = subscription_server(200, SUB_YAML).await;
        let dir = testing::scratch("subs-auto");
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        core.import_url(&format!("{}/sub?token=SECRET", server.uri()), None, |_| {})
            .await
            .unwrap();
        let file = dir.join("local.yaml");
        std::fs::write(&file, SUB_YAML).unwrap();
        core.import_file(&file, None).await.unwrap();
        age_subscriptions(&dir, 25);
        let imported = requests_for(&server).await;
        let (mut app, mut rx) = managed_app(core);
        app.start();
        pump_until(&mut app, &mut rx, |app| {
            app.subscriptions_view.list.is_some()
        })
        .await;
        let before = testing::requests_to(port).len();

        let now = Instant::now();
        app.on_tick(now);
        // The URL one, and not the file.
        assert_eq!(app.subscriptions_view.busy.len(), 1);
        pump_until(&mut app, &mut rx, |app| {
            app.subscriptions_view.busy.is_empty()
        })
        .await;

        assert_eq!(requests_for(&server).await, imported + 1);
        // Nothing is said about it (a toast of the connection to the stand-in
        // kernel may be on screen, which is not what is asked).
        assert!(
            !toasted(&app, "更新") && !toasted(&app, "订阅"),
            "{:?}",
            app.toasts
        );
        let refreshed = app.subscriptions_view.visible()[0].updated_at;
        assert!(refreshed > jiff::Timestamp::now().as_second() - 60);
        // It was the one in use: the kernel is asked to read it again.
        requested(&mut app, &mut rx, port, |requests| {
            requests[before..]
                .iter()
                .any(|request| request.starts_with("PUT /configs"))
        })
        .await;

        // Looked at again only a minute on, and then there is nothing due.
        app.on_tick(now + Duration::from_secs(30));
        app.on_tick(now + Duration::from_secs(61));
        assert!(app.subscriptions_view.busy.is_empty());
        assert_eq!(requests_for(&server).await, imported + 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_failed_automatic_update_says_so_and_is_not_tried_again_for_half_an_hour() {
        let server = subscription_server(200, SUB_YAML).await;
        let dir = testing::scratch("subs-auto-fail");
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        core.import_url(&format!("{}/sub?token=SECRET", server.uri()), None, |_| {})
            .await
            .unwrap();
        age_subscriptions(&dir, 25);
        server.reset().await;
        let (mut app, mut rx) = managed_app(core);
        app.start();
        pump_until(&mut app, &mut rx, |app| {
            app.subscriptions_view.list.is_some()
        })
        .await;

        let now = Instant::now();
        app.on_tick(now);
        pump_until(&mut app, &mut rx, |app| {
            app.subscriptions_view.busy.is_empty()
        })
        .await;
        assert!(toasted(&app, "更新「机场」失败"), "{:?}", app.toasts);
        assert_eq!(requests_for(&server).await, 1);

        // Every minute it is looked at, and left alone, until half an hour.
        app.on_tick(now + Duration::from_secs(61));
        app.on_tick(now + Duration::from_mins(29));
        assert!(app.subscriptions_view.busy.is_empty());
        assert_eq!(requests_for(&server).await, 1);
        app.on_tick(now + Duration::from_mins(31));
        assert_eq!(app.subscriptions_view.busy.len(), 1);
        pump_until(&mut app, &mut rx, |app| {
            app.subscriptions_view.busy.is_empty()
        })
        .await;
        assert_eq!(requests_for(&server).await, 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn t_turns_a_subscription_off_for_the_timer_and_the_row_says_so() {
        let server = subscription_server(200, SUB_YAML).await;
        let (mut app, mut rx, dir, _) = subscriptions_app("subs-auto-toggle", &server).await;
        age_subscriptions(&dir, 25);
        let screen = draw(&app);
        assert!(
            screen
                .lines()
                .any(|line| line.contains("机场") && line.contains("24h")),
            "{screen}"
        );
        // A file is read by hand, and says it is not on the timer.
        assert!(
            screen
                .lines()
                .any(|line| line.contains("local") && line.contains('—')),
            "{screen}"
        );

        key_press(&mut app, KeyCode::Char('t'));
        pump_until(&mut app, &mut rx, |app| {
            !app.subscriptions_view.visible()[0].auto_update
        })
        .await;
        assert!(toasted(&app, "「机场」自动更新已关闭"), "{:?}", app.toasts);
        let screen = draw(&app);
        assert!(
            screen
                .lines()
                .any(|line| line.contains("机场") && line.contains('关')),
            "{screen}"
        );
        // Off, it is never due, however old.
        let imported = requests_for(&server).await;
        let store = crate::core::Store::load(&dir.join("core/subscriptions.toml")).unwrap();
        assert!(!store.subscriptions[0].auto_update);
        app.subscriptions_view.list = Some(Ok(store));
        app.on_tick(Instant::now());
        assert!(app.subscriptions_view.busy.is_empty());
        assert_eq!(requests_for(&server).await, imported);

        // On again, and the file's row cannot be switched.
        key_press(&mut app, KeyCode::Char('t'));
        pump_until(&mut app, &mut rx, |app| {
            app.subscriptions_view.visible()[0].auto_update
        })
        .await;
        key_press(&mut app, KeyCode::Char('j'));
        key_press(&mut app, KeyCode::Char('t'));
        assert!(toasted(&app, "文件订阅不自动更新"), "{:?}", app.toasts);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_login_start_row_writes_the_unit_and_takes_it_away_again() {
        let dir = testing::scratch("app-login");
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (mut app, mut rx) = managed_settings_on(core, Setting::LoginStart).await;
        pump_until(&mut app, &mut rx, |app| app.login_start.is_some()).await;
        assert_eq!(
            app.login_start,
            Some(LoginStart {
                supported: true,
                enabled: false
            })
        );
        let screen = draw(&app);
        assert!(
            screen.lines().any(|line| line.contains("登录时启动")),
            "{screen}"
        );
        let unit = dir.join(format!("units/{}.service", testing::UNIT));

        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, |app| {
            !app.is_changing(Setting::LoginStart)
        })
        .await;
        assert!(unit.exists());
        assert!(app.login_start.is_some_and(|state| state.enabled));
        assert!(toasted(&app, "内核会在登录时启动"), "{:?}", app.toasts);

        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, |app| {
            !app.is_changing(Setting::LoginStart)
        })
        .await;
        assert!(!unit.exists());
        assert!(app.login_start.is_some_and(|state| !state.enabled));
        assert!(toasted(&app, "内核不再在登录时启动"), "{:?}", app.toasts);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_login_start_row_is_greyed_without_a_user_manager_and_writes_nothing() {
        let dir = testing::scratch("app-login-grey");
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = Arc::new(testing::core_at(&dir, port, &recorder, true));
        let (mut app, mut rx) = managed_settings_on(core, Setting::LoginStart).await;
        pump_until(&mut app, &mut rx, |app| app.login_start.is_some()).await;
        app.login_start = Some(LoginStart {
            supported: false,
            enabled: false,
        });
        let screen = draw(&app);
        assert!(
            screen
                .lines()
                .any(|line| line.contains("登录时启动") && line.contains("需要 systemd")),
            "{screen}"
        );

        key_press(&mut app, KeyCode::Enter);

        assert!(toasted(&app, "登录时启动需要 systemd"), "{:?}", app.toasts);
        assert!(!app.is_changing(Setting::LoginStart));
        assert!(!dir.join("units").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_system_proxy_row_points_the_desktop_at_the_mixed_port_and_back() {
        let dir = testing::scratch("app-sysproxy");
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = testing::on_desktop(testing::core_at(&dir, port, &recorder, true), "GNOME");
        let (mut app, mut rx) = managed_settings_on(Arc::new(core), Setting::SystemProxy).await;
        pump_until(&mut app, &mut rx, |app| app.system_proxy.is_some()).await;
        assert!(
            draw(&app)
                .lines()
                .any(|line| line.contains("系统代理") && line.contains('关'))
        );

        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, |app| {
            !app.is_changing(Setting::SystemProxy)
        })
        .await;

        // The Mixed port the kernel reports, not a number of tabby's own.
        let gsettings = recorder.runs("gsettings");
        assert!(
            gsettings.contains(&"set org.gnome.system.proxy.http port 17890".to_owned()),
            "{gsettings:?}"
        );
        assert!(gsettings.iter().any(|run| run.ends_with("mode 'manual'")));
        assert!(app.system_proxy.is_some_and(|state| state.enabled));
        assert!(toasted(&app, "系统代理已指向内核"), "{:?}", app.toasts);
        assert!(
            draw(&app)
                .lines()
                .any(|line| line.contains("系统代理") && line.contains('开'))
        );

        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, |app| {
            !app.is_changing(Setting::SystemProxy)
        })
        .await;
        assert!(app.system_proxy.is_some_and(|state| !state.enabled));
        assert!(toasted(&app, "系统代理已恢复"), "{:?}", app.toasts);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_system_proxy_row_is_greyed_on_a_desktop_without_one() {
        let dir = testing::scratch("app-sysproxy-grey");
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = testing::on_desktop(testing::core_at(&dir, port, &recorder, true), "Hyprland");
        let (mut app, mut rx) = managed_settings_on(Arc::new(core), Setting::SystemProxy).await;
        pump_until(&mut app, &mut rx, |app| app.system_proxy.is_some()).await;
        let screen = draw(&app);
        assert!(
            screen
                .lines()
                .any(|line| line.contains("系统代理") && line.contains("当前桌面不支持系统代理")),
            "{screen}"
        );

        key_press(&mut app, KeyCode::Enter);

        assert!(toasted(&app, "当前桌面不支持系统代理"), "{:?}", app.toasts);
        assert_eq!(recorder.runs("gsettings"), Vec::<String>::new());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn stopping_the_kernel_puts_the_system_proxy_back_first() {
        let dir = testing::scratch("app-sysproxy-stop");
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = Arc::new(testing::on_desktop(
            testing::core_at(&dir, port, &recorder, true),
            "KDE",
        ));
        core.set_system_proxy(true, 17890).await.unwrap();
        let (mut app, mut rx) = managed_settings_on(Arc::clone(&core), Setting::StopCore).await;
        pump_until(&mut app, &mut rx, |app| {
            app.system_proxy.is_some_and(|state| state.enabled)
        })
        .await;

        key_press(&mut app, KeyCode::Enter);
        key_press(&mut app, KeyCode::Char('y'));
        pump_until(&mut app, &mut rx, |app| {
            matches!(app.core_phase, Some(CorePhase::Stopped))
        })
        .await;

        assert!(!core.system_proxy().enabled);
        let calls = recorder.calls();
        let last_write = calls
            .iter()
            .rposition(|cmd| cmd.program == "kwriteconfig6")
            .unwrap();
        let stop = calls
            .iter()
            .position(|cmd| {
                cmd.program == "systemctl" && cmd.args.get(1).map(String::as_str) == Some("stop")
            })
            .unwrap();
        assert!(
            last_write < stop,
            "the proxy is put back before the kernel goes"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_new_mixed_port_takes_the_system_proxy_along() {
        let dir = testing::scratch("app-sysproxy-port");
        let port = testing::free_port();
        let recorder = updating_systemd(port, true, "");
        let core = Arc::new(testing::on_desktop(
            testing::core_at(&dir, port, &recorder, true),
            "GNOME",
        ));
        core.set_system_proxy(true, 17890).await.unwrap();
        let (mut app, mut rx) = managed_settings_on(core, Setting::SystemProxy).await;
        pump_until(&mut app, &mut rx, |app| {
            app.system_proxy.is_some_and(|state| state.enabled)
        })
        .await;

        app.persist_patch(Patch::Port(Port::Mixed, 18000));
        pump_until(&mut app, &mut rx, |_| {
            recorder
                .runs("gsettings")
                .contains(&"set org.gnome.system.proxy.http port 18000".to_owned())
        })
        .await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_proxy_left_on_by_a_kernel_that_went_away_is_said_when_tabby_starts() {
        let dir = testing::scratch("app-sysproxy-dangling");
        let port = testing::free_port();
        let recorder = updating_systemd(port, false, "");
        let core = Arc::new(testing::on_desktop(
            testing::core_at(&dir, port, &recorder, true),
            "GNOME",
        ));
        core.set_system_proxy(true, 17890).await.unwrap();
        let (mut app, mut rx) = managed_app(core);

        app.start();
        pump_until(&mut app, &mut rx, |app| toasted(app, "系统代理没有恢复")).await;

        assert!(toasted(&app, "127.0.0.1:17890"), "{:?}", app.toasts);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn an_encrypted_subscription_asks_for_its_key_hidden_and_goes_on_with_it() {
        use age::secrecy::ExposeSecret;
        use wiremock::{
            Mock, ResponseTemplate,
            matchers::{method, path},
        };
        let plain = subscription_server(200, SUB_YAML).await;
        let (mut app, mut rx, dir, _) = subscriptions_app("subs-age", &plain).await;
        let identity = age::x25519::Identity::generate();
        let key = identity.to_string().expose_secret().to_owned();
        let encrypted = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/sub"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(
                    age::encrypt(&identity.to_public(), SUB_YAML.as_bytes()).unwrap(),
                ),
            )
            .mount(&encrypted)
            .await;

        key_press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, &format!("{}/sub", encrypted.uri()));
        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, asking_age_key).await;
        assert!(!app.subscriptions_view.importing);
        assert!(draw(&app).contains("订阅已加密：输入 age 私钥"));

        // A key that opens nothing says so and asks again; it is never shown.
        let wrong = age::x25519::Identity::generate();
        type_text(&mut app, wrong.to_string().expose_secret());
        let screen = draw(&app);
        assert!(!screen.contains("AGE-SECRET-KEY"), "{screen}");
        assert!(screen.contains("*****"), "{screen}");
        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, asking_age_key).await;
        assert!(
            toasted(&app, "导入失败：age 密钥打不开这份订阅"),
            "{:?}",
            app.toasts
        );
        assert_eq!(app.subscriptions_view.visible().len(), 2);

        type_text(&mut app, &key);
        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, |app| !app.subscriptions_view.importing).await;

        assert!(!asking_age_key(&app));
        assert_eq!(app.subscriptions_view.visible().len(), 3);
        assert_eq!(
            app.subscriptions_view
                .selected()
                .unwrap()
                .age_key
                .as_deref(),
            Some(key.as_str())
        );

        // Esc at the question drops the import.
        key_press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, &format!("{}/sub", encrypted.uri()));
        key_press(&mut app, KeyCode::Enter);
        pump_until(&mut app, &mut rx, asking_age_key).await;
        key_press(&mut app, KeyCode::Esc);
        assert!(!asking_age_key(&app) && !app.typing());
        assert_eq!(app.subscriptions_view.visible().len(), 3);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
