//! The managed core's side of [`App`] (plan §3, §5): bringing the kernel
//! up before the pages have a controller to talk to, and what the screen
//! shows meanwhile.

use std::sync::Arc;

use super::{App, Confirm, Msg, Outbox, Overlay, Reconnect, Setting, Stage, ToastKind};
use crate::{
    core::{CoreError, ManagedCore, Step},
    ui::text::core_reason,
};

/// Where the managed kernel is, as far as the pages are concerned.
#[derive(Debug)]
pub enum CorePhase {
    /// Not asked to start yet; [`App::start`] does.
    Idle,
    /// Being brought up, at `step`.
    Starting(Step),
    /// Its controller answers: the pages work as they do on any controller.
    Running,
    /// It could not be brought up.
    Failed(CoreError),
    /// Stopped from the settings page.
    Stopped,
}

/// News from the work the app spawned on the managed core.
#[derive(Debug)]
pub enum CoreEvent {
    /// What [`ManagedCore::ensure_running`] is doing now.
    Step(Step),
    /// [`ManagedCore::ensure_running`] ended.
    Started(Result<(), CoreError>),
    /// The `停止内核` row ended.
    Stopped(Result<(), CoreError>),
    /// The `重启内核` row ended.
    Restarted(Result<(), CoreError>),
    /// Whether the kernel process still exists, asked after the controller
    /// went quiet.
    Alive(bool),
}

impl App {
    /// Whether the connection is the managed core.
    pub fn is_managed(&self) -> bool {
        self.core_phase.is_some()
    }

    /// Whether there is a controller to ask: any external one, or the
    /// managed core once it is up.
    pub fn controller_up(&self) -> bool {
        matches!(self.core_phase, None | Some(CorePhase::Running))
    }

    /// The managed core, while the connection is it.
    fn managed_core(&self) -> Option<Arc<ManagedCore>> {
        self.core_phase.as_ref()?;
        self.profiles.core.clone()
    }

    /// Runs `work` on the managed core in the background; what it returns
    /// is reported as a [`CoreEvent`].
    fn spawn_core<F, Fut>(&self, work: F)
    where
        F: FnOnce(Arc<ManagedCore>, Outbox) -> Fut,
        Fut: Future<Output = CoreEvent> + Send + 'static,
    {
        let Some(core) = self.managed_core() else {
            return;
        };
        let outbox = self.outbox.clone();
        let event = work(core, outbox.clone());
        tokio::spawn(async move {
            outbox.send(Msg::Core(event.await)).await;
        });
    }

    /// Starts bringing the kernel up, or attaches to the one already
    /// running.
    pub(super) fn begin_core(&mut self) {
        self.core_phase = Some(CorePhase::Starting(Step::Starting));
        self.dirty = true;
        self.spawn_core(|core, outbox| async move {
            // A step the full channel cannot take is only a hint lost.
            let result = core
                .ensure_running(|step| outbox.try_send(Msg::Core(CoreEvent::Step(step))))
                .await;
            CoreEvent::Started(result)
        });
    }

    /// `r` on a managed connection that needs more than a refresh: starts
    /// the kernel again where the last attempt ended, or where it went away.
    /// `true` when it took the key.
    pub(super) fn retry_core(&mut self) -> bool {
        let exited = self.reconnect.is_some_and(|reconnect| reconnect.exited);
        match &self.core_phase {
            None => false,
            // Already on its way.
            Some(CorePhase::Idle | CorePhase::Starting(_)) => true,
            Some(CorePhase::Running) if !exited => false,
            Some(CorePhase::Running | CorePhase::Failed(_) | CorePhase::Stopped) => {
                self.begin_core();
                true
            }
        }
    }

    /// Asks whether the kernel process is still there; the banner says so
    /// when it is not (plan §5.3).
    pub(super) fn check_core_alive(&self) {
        self.spawn_core(|core, _| async move { CoreEvent::Alive(core.alive().await) });
    }

    pub(super) fn on_core(&mut self, event: CoreEvent) {
        match event {
            CoreEvent::Step(step) => {
                if let Some(CorePhase::Starting(shown)) = &mut self.core_phase {
                    *shown = step;
                }
            }
            CoreEvent::Started(Ok(())) => {
                self.core_phase = Some(CorePhase::Running);
                self.reconnect = None;
                self.start();
                self.load_page(self.page);
            }
            CoreEvent::Started(Err(err)) => {
                tracing::warn!("the managed kernel did not start: {err}");
                self.core_phase = Some(CorePhase::Failed(err));
            }
            CoreEvent::Stopped(result) => {
                self.changing.remove(&Setting::StopCore);
                match result {
                    Ok(()) => {
                        let index = self.profiles.current;
                        if let Ok(page) = self.rebuild(index) {
                            self.page = page;
                        }
                        self.core_phase = Some(CorePhase::Stopped);
                        self.toast(ToastKind::Success, "内核已停止".to_owned());
                    }
                    Err(err) => {
                        let reason = core_reason(&err);
                        self.toast(ToastKind::Error, format!("停止内核失败：{reason}"));
                    }
                }
            }
            CoreEvent::Restarted(result) => {
                self.changing.remove(&Setting::Restart);
                match result {
                    Ok(()) => self.toast(ToastKind::Success, "内核已重启".to_owned()),
                    Err(err) => {
                        let reason = core_reason(&err);
                        self.toast(ToastKind::Error, format!("重启失败：{reason}"));
                    }
                }
            }
            CoreEvent::Alive(alive) => {
                if !alive && let Some(reconnect) = &mut self.reconnect {
                    *reconnect = Reconnect {
                        retry_at: None,
                        exited: true,
                        ..*reconnect
                    };
                }
            }
        }
    }

    /// `停止内核`, after the question was answered with `y`.
    pub(super) fn stop_core(&mut self) {
        if self.changing.contains_key(&Setting::StopCore) {
            return;
        }
        self.changing.insert(Setting::StopCore, Stage::Sending);
        self.spawn_core(|core, _| async move { CoreEvent::Stopped(core.stop().await) });
    }

    /// `重启内核` on the managed core: through its supervisor, so that the
    /// kernel's config is written anew and its exit and output stay in the
    /// journal, rather than `POST /restart`.
    pub(super) fn restart_core(&mut self) -> bool {
        if self.managed_core().is_none() {
            return false;
        }
        self.spawn_core(|core, _| async move { CoreEvent::Restarted(core.restart().await) });
        true
    }

    /// The question `停止内核` asks.
    pub(super) fn ask_stop_core(&mut self) {
        self.pending = Some(Confirm::StopCore);
        self.overlay = Some(Overlay::Confirm);
        self.dirty = true;
    }

    /// The rows of the settings page, top to bottom: the managed core's own
    /// only on a managed connection.
    pub fn settings(&self) -> Vec<Setting> {
        Setting::ALL
            .into_iter()
            .filter(|setting| self.is_managed() || !setting.managed_only())
            .collect()
    }

    /// The setting under the settings page's cursor.
    pub fn selected_setting(&self) -> Setting {
        let rows = self.settings();
        rows[self.settings_view.cursor.min(rows.len() - 1)]
    }

    /// Whether the page draws the managed kernel's own state instead of its
    /// data.
    pub fn core_pane(&self) -> Option<&CorePhase> {
        self.core_phase
            .as_ref()
            .filter(|phase| !matches!(phase, CorePhase::Running))
    }
}
