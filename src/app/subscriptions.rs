//! The subscriptions page (plan §10): the list of a managed core's
//! subscriptions, and what its keys do.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use jiff::Timestamp;
use tui_input::Input;

use super::{App, Confirm, CorePhase, Msg, Overlay, ToastKind, managed::failure};
use crate::{
    api::error::ApiError,
    core::{CoreError, Fault, Kind, ManagedCore, Outcome, Store, Subscription},
    ui::{scroll::Scroll, text::reason},
};

/// How often the subscriptions are looked at for any that came due.
const AUTO_CHECK: Duration = Duration::from_secs(60);

/// How long after a failed automatic update the next try waits.
const AUTO_RETRY: Duration = Duration::from_mins(30);

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
}

/// News from the work started on the subscriptions.
#[derive(Debug)]
pub enum SubEvent {
    /// The list, as the files have it.
    Loaded(Result<Store, CoreError>),
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
    /// The address or path being typed to import a subscription, while one
    /// is; it has the keyboard.
    pub adding: Option<Input>,
    /// The address or path of an encrypted subscription, while the box asks
    /// for the age key to open it with.
    pub key_for: Option<String>,
    /// Subscriptions being updated or switched to.
    pub busy: HashSet<String>,
    /// An import is under way.
    pub importing: bool,
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

impl App {
    /// Whether the box at the foot of the screen takes the address or path of
    /// a subscription to import, rather than a filter.
    pub fn adding_subscription(&self) -> bool {
        self.page == super::Page::Subscriptions && self.subscriptions_view.adding.is_some()
    }

    /// Whether the box at the foot of the screen takes an age key.
    pub fn asking_age_key(&self) -> bool {
        self.adding_subscription() && self.subscriptions_view.key_for.is_some()
    }

    /// The managed core, while the pages are talking to it.
    fn subscriptions_core(&self) -> Option<std::sync::Arc<ManagedCore>> {
        match self.core_phase {
            Some(CorePhase::Running) => self.managed_core(),
            _ => None,
        }
    }

    /// Runs `work` on the managed core in the background; what it returns
    /// is reported as a [`SubEvent`].
    fn spawn_subscription<F, Fut>(&self, work: F)
    where
        F: FnOnce(std::sync::Arc<ManagedCore>) -> Fut,
        Fut: Future<Output = SubEvent> + Send + 'static,
    {
        let Some(core) = self.subscriptions_core() else {
            return;
        };
        let outbox = self.outbox.clone();
        let event = work(core);
        tokio::spawn(async move {
            outbox.send(Msg::Subscription(event.await)).await;
        });
    }

    /// Reads the list from its file.
    pub(super) fn load_subscriptions(&self) {
        self.spawn_subscription(|core| async move { SubEvent::Loaded(core.subscriptions()) });
    }

    /// `a`: opens the box an address or a path is typed in.
    pub(super) fn add_subscription(&mut self) {
        if self.subscriptions_core().is_none() || self.subscriptions_view.adding.is_some() {
            return;
        }
        self.subscriptions_view.typing = false;
        self.subscriptions_view.adding = Some(Input::default());
        self.dirty = true;
    }

    /// `Enter` in the box: imports what was typed, an address or a file.
    pub(super) fn submit_subscription(&mut self) {
        let Some(input) = self.subscriptions_view.adding.take() else {
            return;
        };
        self.dirty = true;
        let text = input.value().trim().to_owned();
        let key_for = self.subscriptions_view.key_for.take();
        if text.is_empty() {
            return;
        }
        if self.subscriptions_view.importing {
            self.toast(ToastKind::Error, "正在导入另一个订阅".to_owned());
            return;
        }
        self.subscriptions_view.importing = true;
        // What was typed is the key when the box was asking for one.
        let (source, key) = match key_for {
            Some(source) => (source, Some(text)),
            None => (text, None),
        };
        let is_url = source.starts_with("http://") || source.starts_with("https://");
        self.spawn_subscription(move |core| async move {
            let result = if is_url {
                core.import_url(&source, key.as_deref()).await
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
        let view = &mut self.subscriptions_view;
        view.key_for = Some(source.to_owned());
        view.adding = Some(Input::default());
        view.typing = false;
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
        self.spawn_subscription(move |core| async move {
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
        self.spawn_subscription(move |core| async move {
            let result = core.update(&id).await;
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
        self.spawn_subscription(move |core| async move {
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
        self.spawn_subscription(move |core| async move {
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
            Work::Import(_) => view.importing = false,
            Work::Update(id) | Work::AutoUpdate(id) | Work::Switch(id) => {
                view.busy.remove(id);
            }
            Work::Remove | Work::Toggle(_) => {}
        }
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(err) => {
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
                    Work::Toggle(_) => "设置失败".to_owned(),
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
        };
        if let Some(text) = text {
            self.toast(ToastKind::Success, text);
        }
        if outcome.reload {
            self.reload_kernel();
        }
    }
}
