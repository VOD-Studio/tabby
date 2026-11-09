//! The subscriptions page (plan §10): the list of a managed core's
//! subscriptions, and what its keys do.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

use jiff::Timestamp;
use tui_input::Input;

use super::{App, Confirm, CorePhase, Msg, Outbox, Overlay, ToastKind, managed::failure};
use crate::{
    api::error::ApiError,
    core::{CoreError, Fault, Kind, ManagedCore, Outcome, Progress, Store, Subscription},
    ui::{scroll::Scroll, text::reason},
};

/// How often the subscriptions are looked at for any that came due.
const AUTO_CHECK: Duration = Duration::from_secs(60);

/// How long after a failed automatic update the next try waits.
const AUTO_RETRY: Duration = Duration::from_mins(30);

/// How far back the speed of a download is measured.
const RATE_WINDOW: Duration = Duration::from_secs(2);

/// Less time than this since a download began says nothing about its
/// speed yet.
const RATE_MIN: Duration = Duration::from_millis(500);

/// What is being done to subscriptions, for the answer to say what it was.
#[derive(Debug)]
pub enum Work {
    /// Importing what was typed: `source`, an address or a path.
    Import(String),
    /// Reading subscription `id` again.
    Update(String),
    /// Making subscription `id` the one in use.
    Switch(String),
    /// Reading subscription `id` again because its interval has passed.
    AutoUpdate(String),
    /// Turning the automatic update of a subscription on (or off).
    Toggle(bool),
    Remove,
    /// Giving a subscription another name.
    Rename,
    /// Setting the hours between the automatic updates of a subscription.
    Interval(u32),
    /// Reading subscription `id` again with the age key it was given.
    Rekey(String),
}

impl Work {
    /// What it is, in the log.
    fn label(&self) -> &'static str {
        match self {
            Self::Import(_) => "import",
            Self::Update(_) => "update",
            Self::Switch(_) => "switch",
            Self::AutoUpdate(_) => "auto-update",
            Self::Toggle(_) => "auto-update toggle",
            Self::Remove => "remove",
            Self::Rename => "rename",
            Self::Interval(_) => "interval",
            Self::Rekey(_) => "age key change",
        }
    }
}

/// What the box at the foot of the page is asked for while it is open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Ask {
    /// The address or path of a subscription to import.
    #[default]
    Import,
    /// The age key to open the encrypted subscription at the address or
    /// path with.
    ImportKey(String),
    /// The new name of subscription `id`.
    Name(String),
    /// The hours between the automatic updates of subscription `id`.
    Interval(String),
    /// The age key of subscription `id` is to be another.
    Key(String),
}

/// How the box at the foot of the screen looks while it asks `Ask`.
#[derive(Debug, Clone, Copy)]
pub struct Prompt {
    pub glyph: &'static str,
    /// What the keys do, after the text.
    pub hint: &'static str,
    /// Said where nothing is typed yet.
    pub placeholder: &'static str,
    /// What is typed is a secret, and shown as `*`.
    pub secret: bool,
}

impl Ask {
    pub fn prompt(&self) -> Prompt {
        let (glyph, hint, placeholder, secret) = match self {
            Self::Import => ("+ ", "  Enter 导入   Esc 取消", "订阅地址或文件路径", false),
            Self::ImportKey(_) => (
                "* ",
                "  Enter 解密导入   Esc 取消",
                "订阅已加密：输入 age 私钥",
                true,
            ),
            Self::Name(_) => ("> ", "  Enter 改名   Esc 取消", "订阅名称", false),
            Self::Interval(_) => (
                "> ",
                "  Enter 确定   Esc 取消",
                "自动更新间隔（小时）",
                false,
            ),
            Self::Key(_) => ("* ", "  Enter 更换   Esc 取消", "输入新的 age 私钥", true),
        };
        Prompt {
            glyph,
            hint,
            placeholder,
            secret,
        }
    }
}

/// What is being downloaded.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Fetching {
    /// The subscription being imported.
    Import,
    /// Subscription `id`, read again.
    Subscription(String),
}

/// A download under way, as far as it said.
#[derive(Debug, Clone)]
pub struct Transfer {
    /// Which try it is, from 1.
    pub attempt: u32,
    /// The server answered this try.
    pub answered: bool,
    pub received: u64,
    pub total: Option<u64>,
    /// Every byte came; the kernel is checking it.
    pub done: bool,
    /// When this try said how far it was since the server answered, and
    /// what it said: those of the last [`RATE_WINDOW`], and the one before
    /// them, where the window starts.
    samples: VecDeque<(Instant, u64)>,
}

impl Transfer {
    fn new(progress: Progress, now: Instant) -> Self {
        let mut transfer = Self {
            attempt: progress.attempt,
            answered: false,
            received: 0,
            total: None,
            done: false,
            samples: VecDeque::new(),
        };
        transfer.record(progress, now);
        transfer
    }

    /// Takes in what the download said at `now`. Another try starts the
    /// count again; the wait for an answer does not count toward the
    /// speed.
    fn record(&mut self, progress: Progress, now: Instant) {
        if progress.attempt != self.attempt {
            self.samples.clear();
        }
        self.attempt = progress.attempt;
        self.answered = progress.answered;
        self.received = progress.received;
        self.total = progress.total;
        self.done = progress.done;
        if !progress.answered {
            return;
        }
        self.samples.push_back((now, progress.received));
        let start = now.checked_sub(RATE_WINDOW);
        while self
            .samples
            .get(1)
            .is_some_and(|(at, _)| start.is_some_and(|start| *at <= start))
        {
            self.samples.pop_front();
        }
    }

    /// Bytes a second over the last [`RATE_WINDOW`] as of `now`; falls to 0
    /// while nothing comes. `None` until the try has run [`RATE_MIN`] since
    /// the server answered, and once every byte came.
    pub fn rate(&self, now: Instant) -> Option<u64> {
        if self.done {
            return None;
        }
        let start = now.checked_sub(RATE_WINDOW);
        let first = *self.samples.front()?;
        // What had come when the window starts: the last report before
        // then. A try younger than the window counts from its beginning.
        let (from, base) = self
            .samples
            .iter()
            .rev()
            .find(|(at, _)| start.is_some_and(|start| *at <= start))
            .map_or(first, |&(_, bytes)| (start.unwrap_or(first.0), bytes));
        let elapsed = now.saturating_duration_since(from);
        if elapsed < RATE_MIN {
            return None;
        }
        let bytes = u128::from(self.received.saturating_sub(base));
        Some(u64::try_from(bytes * 1000 / elapsed.as_millis()).unwrap_or(u64::MAX))
    }
}

/// News from the work started on the subscriptions.
#[derive(Debug)]
pub enum SubEvent {
    /// The list, as the files have it.
    Loaded(Result<Store, CoreError>),
    /// How far a download is.
    Progress {
        fetching: Fetching,
        progress: Progress,
    },
    Done {
        work: Work,
        result: Result<Outcome, CoreError>,
    },
    /// The kernel was asked to read its config file again.
    Reloaded(Result<(), ApiError>),
}

/// List, cursor, filter and input of the subscriptions page.
#[derive(Debug, Default)]
pub struct SubscriptionsView {
    /// The list; `None` until it is read.
    pub list: Option<Result<Store, CoreError>>,
    /// Index into the subscriptions that pass the filter.
    pub cursor: usize,
    pub scroll: Scroll,
    /// Case-insensitive substring a name must contain.
    pub filter: Input,
    /// The filter box has the keyboard.
    pub typing: bool,
    /// What is being typed in the box at the foot of the screen, while one
    /// is open; it has the keyboard. What it is for is [`Self::ask`].
    pub adding: Option<Input>,
    /// What the open box asks for.
    pub ask: Ask,
    /// Subscriptions being updated or switched to.
    pub busy: HashSet<String>,
    /// An import is under way.
    pub importing: bool,
    /// The downloads under way.
    pub transfers: HashMap<Fetching, Transfer>,
    /// When the subscriptions are looked at next for any that came due.
    next_auto: Option<Instant>,
    /// When an automatic update of each was last tried.
    auto_tried: HashMap<String, Instant>,
}

impl SubscriptionsView {
    fn store(&self) -> Option<&Store> {
        self.list.as_ref()?.as_ref().ok()
    }

    /// The subscriptions that pass the filter, in the order of the list.
    pub fn visible(&self) -> Vec<&Subscription> {
        let Some(store) = self.store() else {
            return vec![];
        };
        let needle = self.filter.value().to_lowercase();
        store
            .subscriptions
            .iter()
            .filter(|sub| needle.is_empty() || sub.name.to_lowercase().contains(&needle))
            .collect()
    }

    /// The subscription in use, if any is.
    pub fn active(&self) -> Option<&str> {
        self.store()?.active.as_deref()
    }

    /// Whether anything is going on that the page spins for.
    pub fn working(&self) -> bool {
        self.importing || !self.busy.is_empty()
    }

    /// The subscription under the cursor.
    pub fn selected(&self) -> Option<&Subscription> {
        let visible = self.visible();
        visible
            .get(self.cursor.min(visible.len().checked_sub(1)?))
            .copied()
    }

    /// Takes in a new list: the cursor stays on its subscription, or at its
    /// place when that one is gone.
    fn replace(&mut self, store: Store) {
        let anchor = self.selected().map(|sub| (sub.id.clone(), self.cursor));
        self.list = Some(Ok(store));
        let visible = self.visible();
        self.cursor = anchor
            .map_or(0, |(id, at)| {
                visible.iter().position(|sub| sub.id == id).unwrap_or(at)
            })
            .min(visible.len().saturating_sub(1));
    }

    /// Changes the filter; the cursor goes to the first match.
    pub(super) fn edit_filter(&mut self, edit: impl FnOnce(&mut Input)) {
        edit(&mut self.filter);
        self.cursor = 0;
    }

    /// Drops the filter and gives the keyboard back, the cursor staying on
    /// its subscription.
    pub(super) fn clear_filter(&mut self) {
        let anchor = self.selected().map(|sub| sub.id.clone());
        self.filter.reset();
        self.typing = false;
        let visible = self.visible();
        self.cursor = anchor
            .and_then(|id| visible.iter().position(|sub| sub.id == id))
            .unwrap_or(0);
    }
}

/// `text` as the file it names, `~` standing for the home directory.
fn path_of(text: &str) -> std::path::PathBuf {
    match (text.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => std::path::Path::new(&home).join(rest),
        _ => std::path::PathBuf::from(text),
    }
}

/// `source` as the log may have it: an address carries its owner's token
/// in its path or query, so only its scheme, host and port are kept. A path
/// is kept whole.
fn loggable(source: &str) -> String {
    if !(source.starts_with("http://") || source.starts_with("https://")) {
        return source.to_owned();
    }
    match reqwest::Url::parse(source) {
        Ok(url) => {
            let host = url.host_str().unwrap_or_default();
            match url.port() {
                Some(port) => format!("{}://{host}:{port}", url.scheme()),
                None => format!("{}://{host}", url.scheme()),
            }
        }
        Err(_) => "(an address that does not parse)".to_owned(),
    }
}

/// What tells the page through `outbox` how far the download of
/// `fetching` is. A report that does not fit is dropped: the next one
/// says more.
fn report(outbox: Outbox, fetching: Fetching) -> impl Fn(Progress) {
    move |progress| {
        outbox.try_send(Msg::Subscription(SubEvent::Progress {
            fetching: fetching.clone(),
            progress,
        }));
    }
}

/// The hours typed as the interval between automatic updates: a whole
/// number of hours, up to a year.
fn parse_interval(text: &str) -> Result<u32, &'static str> {
    const WHY: &str = "间隔须是 1–8760 小时";
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(WHY);
    }
    // Four digits at most: no overflow whatever is typed.
    let digits = text.trim_start_matches('0');
    if digits.len() > 4 {
        return Err(WHY);
    }
    match digits.parse::<u32>() {
        Ok(hours @ 1..=8760) => Ok(hours),
        _ => Err(WHY),
    }
}

impl App {
    /// How the box at the foot of the screen looks, while it asks the
    /// subscriptions page for something rather than filters.
    pub fn subscription_prompt(&self) -> Option<Prompt> {
        let view = &self.subscriptions_view;
        (self.page == super::Page::Subscriptions && view.adding.is_some())
            .then(|| view.ask.prompt())
    }

    /// The managed core, while the pages are talking to it.
    fn subscriptions_core(&self) -> Option<std::sync::Arc<ManagedCore>> {
        match self.core_phase {
            Some(CorePhase::Running) => self.managed_core(),
            _ => None,
        }
    }

    /// Runs `work` on the managed core in the background; what it returns
    /// is reported as a [`SubEvent`]. `work` gets the outbox too, to say
    /// how far a download is meanwhile.
    fn spawn_subscription<F, Fut>(&self, work: F)
    where
        F: FnOnce(std::sync::Arc<ManagedCore>, Outbox) -> Fut,
        Fut: Future<Output = SubEvent> + Send + 'static,
    {
        let Some(core) = self.subscriptions_core() else {
            return;
        };
        let outbox = self.outbox.clone();
        let event = work(core, outbox.clone());
        tokio::spawn(async move {
            outbox.send(Msg::Subscription(event.await)).await;
        });
    }

    /// Reads the list from its file.
    pub(super) fn load_subscriptions(&self) {
        self.spawn_subscription(|core, _| async move { SubEvent::Loaded(core.subscriptions()) });
    }

    /// Opens the box at the foot of the screen to ask for `ask`, `text`
    /// already in it.
    fn open_box(&mut self, ask: Ask, text: &str) {
        let view = &mut self.subscriptions_view;
        view.typing = false;
        view.ask = ask;
        view.adding = Some(Input::new(text.to_owned()));
        self.dirty = true;
    }

    /// `a`: opens the box an address or a path is typed in.
    pub(super) fn add_subscription(&mut self) {
        if self.subscriptions_core().is_none() || self.subscriptions_view.adding.is_some() {
            return;
        }
        self.open_box(Ask::Import, "");
    }

    /// `e`: opens the box the name of the subscription under the cursor is
    /// changed in.
    pub(super) fn rename_subscription(&mut self) {
        if self.subscriptions_core().is_none() || self.subscriptions_view.adding.is_some() {
            return;
        }
        let Some(sub) = self.subscriptions_view.selected() else {
            return;
        };
        let (id, name) = (sub.id.clone(), sub.name.clone());
        self.open_box(Ask::Name(id), &name);
    }

    /// `i`: opens the box the hours between the automatic updates of the
    /// subscription under the cursor are typed in.
    pub(super) fn edit_interval(&mut self) {
        if self.subscriptions_core().is_none() || self.subscriptions_view.adding.is_some() {
            return;
        }
        let Some(sub) = self.subscriptions_view.selected() else {
            return;
        };
        if sub.kind == Kind::File {
            self.toast(
                ToastKind::Error,
                "文件订阅不自动更新，按 u 重新读取".to_owned(),
            );
            return;
        }
        let (id, hours) = (sub.id.clone(), sub.interval_hours.max(1).to_string());
        self.open_box(Ask::Interval(id), &hours);
    }

    /// `K`: opens the box the subscription under the cursor is given
    /// another age key in. Only an encrypted subscription has one.
    pub(super) fn change_age_key(&mut self) {
        if self.subscriptions_core().is_none() || self.subscriptions_view.adding.is_some() {
            return;
        }
        let Some(sub) = self.subscriptions_view.selected() else {
            return;
        };
        if sub.age_key.is_none() {
            self.toast(ToastKind::Error, "这份订阅没有加密".to_owned());
            return;
        }
        let id = sub.id.clone();
        self.open_box(Ask::Key(id), "");
    }

    /// `Enter` in the box: does what the box asked for with what was typed.
    /// A typed interval that is not one leaves the box open.
    pub(super) fn submit_subscription(&mut self) {
        let Some(input) = self.subscriptions_view.adding.take() else {
            return;
        };
        self.dirty = true;
        let text = input.value().trim().to_owned();
        let ask = std::mem::take(&mut self.subscriptions_view.ask);
        if text.is_empty() {
            return;
        }
        match ask {
            Ask::Import => self.start_import(text, None),
            Ask::ImportKey(source) => self.start_import(source, Some(text)),
            Ask::Name(id) => self.spawn_subscription(move |core, _| async move {
                SubEvent::Done {
                    result: core.rename(&id, &text).await,
                    work: Work::Rename,
                }
            }),
            Ask::Interval(id) => match parse_interval(&text) {
                Ok(hours) => self.spawn_subscription(move |core, _| async move {
                    SubEvent::Done {
                        result: core.set_interval(&id, hours).await,
                        work: Work::Interval(hours),
                    }
                }),
                Err(why) => {
                    let view = &mut self.subscriptions_view;
                    view.ask = Ask::Interval(id);
                    view.adding = Some(input);
                    self.toast(ToastKind::Error, why.to_owned());
                }
            },
            Ask::Key(id) => {
                if !self.subscriptions_view.busy.insert(id.clone()) {
                    return;
                }
                self.spawn_subscription(move |core, outbox| async move {
                    let progress = report(outbox, Fetching::Subscription(id.clone()));
                    SubEvent::Done {
                        result: core.set_age_key(&id, &text, progress).await,
                        work: Work::Rekey(id),
                    }
                });
            }
        }
    }

    /// Imports `source`, an address or a file, opening it with `key` when
    /// there is one.
    fn start_import(&mut self, source: String, key: Option<String>) {
        if self.subscriptions_view.importing {
            self.toast(ToastKind::Error, "正在导入另一个订阅".to_owned());
            return;
        }
        self.subscriptions_view.importing = true;
        let is_url = source.starts_with("http://") || source.starts_with("https://");
        self.spawn_subscription(move |core, outbox| async move {
            let result = if is_url {
                let progress = report(outbox, Fetching::Import);
                core.import_url(&source, key.as_deref(), progress).await
            } else {
                core.import_file(&path_of(&source), key.as_deref()).await
            };
            SubEvent::Done {
                work: Work::Import(source),
                result,
            }
        });
    }

    /// The import of `source` failed for want of a key that opens it: the
    /// box asks for one, and the import goes on with it. `true` when it
    /// does.
    fn ask_age_key(&mut self, source: &str, err: &CoreError) -> bool {
        match err {
            CoreError::AgeKeyNeeded => {}
            CoreError::Age(Fault::NotAKey | Fault::WrongKey) => {
                self.toast(ToastKind::Error, failure("导入失败", err));
            }
            _ => return false,
        }
        self.open_box(Ask::ImportKey(source.to_owned()), "");
        true
    }

    /// `Enter`: makes the subscription under the cursor the one in use.
    pub(super) fn switch_subscription(&mut self) {
        let Some(sub) = self.subscriptions_view.selected() else {
            return;
        };
        let (id, name) = (sub.id.clone(), sub.name.clone());
        if self.subscriptions_view.active() == Some(id.as_str()) {
            self.toast(ToastKind::Success, format!("「{name}」已在使用"));
            return;
        }
        if !self.subscriptions_view.busy.insert(id.clone()) {
            return;
        }
        self.dirty = true;
        self.spawn_subscription(move |core, _| async move {
            SubEvent::Done {
                result: core.switch(&id).await,
                work: Work::Switch(id),
            }
        });
    }

    /// `u`: reads the subscription under the cursor again.
    pub(super) fn update_subscription(&mut self) {
        let Some(sub) = self.subscriptions_view.selected() else {
            return;
        };
        let id = sub.id.clone();
        self.start_update(id);
    }

    fn start_update(&mut self, id: String) {
        self.begin_update(id, false);
    }

    /// Starts reading subscription `id` again, `auto` when nobody asked.
    fn begin_update(&mut self, id: String, auto: bool) {
        if !self.subscriptions_view.busy.insert(id.clone()) {
            return;
        }
        self.dirty = true;
        self.spawn_subscription(move |core, outbox| async move {
            let progress = report(outbox, Fetching::Subscription(id.clone()));
            let result = core.update(&id, progress).await;
            let work = if auto {
                Work::AutoUpdate(id)
            } else {
                Work::Update(id)
            };
            SubEvent::Done { result, work }
        });
    }

    /// `t`: turns the automatic update of the subscription under the cursor
    /// on or off.
    pub(super) fn toggle_auto_update(&mut self) {
        let Some(sub) = self.subscriptions_view.selected() else {
            return;
        };
        if sub.kind == Kind::File {
            self.toast(
                ToastKind::Error,
                "文件订阅不自动更新，按 u 重新读取".to_owned(),
            );
            return;
        }
        let (id, on) = (sub.id.clone(), !sub.auto_update);
        self.spawn_subscription(move |core, _| async move {
            SubEvent::Done {
                result: core.set_auto_update(&id, on).await,
                work: Work::Toggle(on),
            }
        });
    }

    /// Every [`AUTO_CHECK`]: updates the subscriptions that came due. One
    /// that failed is left alone for [`AUTO_RETRY`], so that a server that
    /// is down is not asked every minute, with a toast each time.
    pub(super) fn auto_update_due(&mut self, now: Instant) {
        if self.subscriptions_core().is_none() {
            return;
        }
        let view = &mut self.subscriptions_view;
        if view.next_auto.is_some_and(|at| now < at) {
            return;
        }
        let Some(store) = view.store() else {
            // Not read yet; look again at the next tick.
            return;
        };
        let due: Vec<String> = store
            .due(Timestamp::now().as_second())
            .into_iter()
            .map(|sub| sub.id.clone())
            .filter(|id| {
                view.auto_tried
                    .get(id)
                    .is_none_or(|tried| now.saturating_duration_since(*tried) >= AUTO_RETRY)
            })
            .collect();
        view.next_auto = Some(now + AUTO_CHECK);
        for id in due {
            self.subscriptions_view.auto_tried.insert(id.clone(), now);
            self.begin_update(id, true);
        }
    }

    /// `U`: asks whether to update every subscription the page shows.
    pub(super) fn ask_update_subscriptions(&mut self) {
        let ids: Vec<String> = self
            .subscriptions_view
            .visible()
            .iter()
            .map(|sub| sub.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        self.pending = Some(Confirm::UpdateSubscriptions {
            filter: self.subscriptions_view.filter.value().to_owned(),
            ids,
        });
        self.overlay = Some(Overlay::Confirm);
        self.dirty = true;
    }

    pub(super) fn update_subscriptions(&mut self, ids: Vec<String>) {
        for id in ids {
            self.start_update(id);
        }
    }

    /// `d`: asks whether to delete the subscription under the cursor; the
    /// one in use cannot be.
    pub(super) fn ask_delete_subscription(&mut self) {
        let Some(sub) = self.subscriptions_view.selected() else {
            return;
        };
        if self.subscriptions_view.active() == Some(sub.id.as_str()) {
            self.toast(ToastKind::Error, "先切换到其他订阅".to_owned());
            return;
        }
        self.pending = Some(Confirm::DeleteSubscription {
            id: sub.id.clone(),
            name: sub.name.clone(),
        });
        self.overlay = Some(Overlay::Confirm);
        self.dirty = true;
    }

    pub(super) fn delete_subscription(&mut self, id: String) {
        self.spawn_subscription(move |core, _| async move {
            SubEvent::Done {
                result: core.remove(&id).await,
                work: Work::Remove,
            }
        });
    }

    /// Asks the kernel to read its config file again, after tabby wrote it
    /// anew, and refreshes what the pages show of it.
    fn reload_kernel(&self) {
        self.spawn(|client| async move {
            Msg::Subscription(SubEvent::Reloaded(client.reload_config().await))
        });
    }

    pub(super) fn on_subscription(&mut self, event: SubEvent) {
        match event {
            SubEvent::Loaded(Ok(store)) => self.subscriptions_view.replace(store),
            SubEvent::Loaded(Err(err)) => self.subscriptions_view.list = Some(Err(err)),
            SubEvent::Progress { fetching, progress } => {
                let now = Instant::now();
                self.subscriptions_view
                    .transfers
                    .entry(fetching)
                    .and_modify(|transfer| transfer.record(progress, now))
                    .or_insert_with(|| Transfer::new(progress, now));
            }
            SubEvent::Done { work, result } => self.finish_subscription(&work, result),
            SubEvent::Reloaded(Ok(())) => {
                self.fetch_configs(true);
                if matches!(self.proxies, Some(Ok(_))) {
                    self.fetch_proxies(true);
                }
                self.reload_visited();
            }
            SubEvent::Reloaded(Err(err)) => {
                self.toast(ToastKind::Error, format!("重载配置失败：{}", reason(&err)));
            }
        }
    }

    fn finish_subscription(&mut self, work: &Work, result: Result<Outcome, CoreError>) {
        let view = &mut self.subscriptions_view;
        match work {
            Work::Import(_) => {
                view.importing = false;
                view.transfers.remove(&Fetching::Import);
            }
            Work::Update(id) | Work::AutoUpdate(id) | Work::Switch(id) | Work::Rekey(id) => {
                view.busy.remove(id);
                view.transfers.remove(&Fetching::Subscription(id.clone()));
            }
            Work::Remove | Work::Toggle(_) | Work::Rename | Work::Interval(_) => {}
        }
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(err) => {
                // Asking for a key is no failure; the rest outlives its toast
                // in the log.
                if !matches!(err, CoreError::AgeKeyNeeded) {
                    self.log_failure(work, &err);
                }
                if let Work::Import(source) = work
                    && self.ask_age_key(source, &err)
                {
                    return;
                }
                let what = match work {
                    Work::Import(_) => "导入失败".to_owned(),
                    Work::Update(id) | Work::AutoUpdate(id) | Work::Switch(id) => {
                        let name = self
                            .subscriptions_view
                            .store()
                            .and_then(|store| store.get(id))
                            .map_or("订阅", |sub| sub.name.as_str());
                        let verb = if matches!(work, Work::Switch(_)) {
                            "切换"
                        } else {
                            "更新"
                        };
                        format!("{verb}「{name}」失败")
                    }
                    Work::Remove => "删除失败".to_owned(),
                    Work::Toggle(_) | Work::Interval(_) => "设置失败".to_owned(),
                    Work::Rename => "改名失败".to_owned(),
                    Work::Rekey(_) => "更换密钥失败".to_owned(),
                };
                self.toast(ToastKind::Error, failure(&what, &err));
                return;
            }
        };
        let name = outcome.name;
        self.subscriptions_view.replace(outcome.store);
        let text = match work {
            Work::Import(_) => {
                // The new subscription is the last of the list.
                let last = self.subscriptions_view.visible().len().saturating_sub(1);
                self.subscriptions_view.cursor = last;
                Some(format!("已导入「{name}」"))
            }
            Work::Update(_) => Some(format!("已更新「{name}」")),
            // Nobody asked: nothing to say unless it failed.
            Work::AutoUpdate(_) => None,
            Work::Switch(_) if outcome.reload => Some(format!("已切换到「{name}」")),
            Work::Switch(_) => Some(format!("「{name}」已在使用")),
            Work::Remove => Some(format!("已删除「{name}」")),
            Work::Toggle(on) => Some(format!(
                "「{name}」自动更新已{}",
                if *on { "开启" } else { "关闭" }
            )),
            Work::Rename => Some(format!("已改名为「{name}」")),
            Work::Interval(hours) => Some(format!("「{name}」每 {hours} 小时自动更新")),
            Work::Rekey(_) => Some(format!("已用新密钥更新「{name}」")),
        };
        if let Some(text) = text {
            self.toast(ToastKind::Success, text);
        }
        if outcome.reload {
            self.reload_kernel();
        }
    }

    /// Writes down what failed and why, where it can be looked up after
    /// the toast is gone.
    fn log_failure(&self, work: &Work, err: &CoreError) {
        let subject = match work {
            Work::Import(source) => loggable(source),
            Work::Update(id) | Work::AutoUpdate(id) | Work::Switch(id) | Work::Rekey(id) => {
                let name = self
                    .subscriptions_view
                    .store()
                    .and_then(|store| store.get(id))
                    .map_or("", |sub| sub.name.as_str());
                format!("{name} ({id})")
            }
            Work::Remove | Work::Toggle(_) | Work::Rename | Work::Interval(_) => String::new(),
        };
        tracing::warn!(
            work = work.label(),
            subscription = %subject,
            "subscription work failed: {err}"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Progress, Transfer, loggable};

    fn said(attempt: u32, received: u64) -> Progress {
        Progress {
            attempt,
            answered: true,
            received,
            total: None,
            done: false,
        }
    }

    #[test]
    fn the_speed_is_what_came_over_the_last_two_seconds_and_falls_to_nothing_in_a_stall() {
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        // Asked; the server takes three seconds to answer, which is not a
        // speed of nothing.
        let asked = Progress {
            answered: false,
            ..said(1, 0)
        };
        let mut transfer = Transfer::new(asked, at(0));
        assert_eq!(transfer.rate(at(3_000)), None);
        let at = |millis: u64| start + Duration::from_millis(3_000 + millis);
        transfer.record(said(1, 0), at(0));
        assert!(transfer.answered);
        // Too early to say.
        transfer.record(said(1, 4_000), at(300));
        assert_eq!(transfer.rate(at(300)), None);

        transfer.record(said(1, 10_000), at(1_000));
        transfer.record(said(1, 20_000), at(2_000));
        assert_eq!(transfer.rate(at(2_000)), Some(10_000));
        // Faster now: only the last two seconds count.
        transfer.record(said(1, 60_000), at(3_000));
        assert_eq!(transfer.rate(at(3_000)), Some(25_000));
        // Nothing more comes: the speed falls, then is nothing.
        assert_eq!(transfer.rate(at(4_000)), Some(20_000));
        assert_eq!(transfer.rate(at(5_000)), Some(0));

        // Another try counts from its own start.
        transfer.record(said(2, 0), at(6_000));
        transfer.record(said(2, 1_000), at(6_200));
        assert_eq!((transfer.attempt, transfer.received), (2, 1_000));
        assert_eq!(transfer.rate(at(6_200)), None);
        assert_eq!(transfer.rate(at(7_000)), Some(1_000));
    }

    #[test]
    fn an_address_is_logged_without_the_token_in_its_path_or_query() {
        assert_eq!(
            loggable("https://sub.example.test/q/93dd2ffe5fc44203?token=SECRET#frag"),
            "https://sub.example.test"
        );
        assert_eq!(
            loggable("http://127.0.0.1:8080/sub?token=SECRET"),
            "http://127.0.0.1:8080"
        );
        // A path is the user's own and has no token: kept as typed.
        assert_eq!(loggable("~/subs/home.yaml"), "~/subs/home.yaml");
        assert!(!loggable("https://[bad/SECRET").contains("SECRET"));
    }
}
