//! The managed core's side of [`App`] (plan §3, §5): bringing the kernel
//! up before the pages have a controller to talk to, and what the screen
//! shows meanwhile.

use std::sync::Arc;

use super::{
    App, Change, Confirm, Msg, Outbox, Overlay, Patch, Port, Reconnect, Setting, Stage, ToastKind,
};
use crate::{
    core::{CoreError, Graphical, ManagedCore, Persist, Release, Step, Upgrade},
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

/// Whether the kernel starts at login, and whether it can.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginStart {
    /// There is a systemd user manager to keep the unit.
    pub supported: bool,
    /// The unit is there.
    pub enabled: bool,
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
    /// The newest release, looked up because there is no kernel yet.
    Release(Result<Release, CoreError>),
    /// The kernel was downloaded and put in place.
    Installed(Result<(), CoreError>),
    /// What `升级内核` found out: whether a newer kernel is out.
    UpgradeChecked(Result<Upgrade, CoreError>),
    /// `升级内核` ended: the version now installed.
    Upgraded(Result<String, CoreError>),
    /// The config file was made again, for `重载配置`.
    Regenerated(Result<(), CoreError>),
    /// Whether the kernel's file has the capabilities, as `getcap` says.
    Authority(bool),
    /// What asking for them through the desktop's dialog came to.
    Setcap(Result<Graphical, CoreError>),
    /// The capabilities are set and the kernel started again, or not.
    Authorized(Result<(), CoreError>),
    /// Whether the kernel starts at login, as the unit file says.
    LoginStart(LoginStart),
    /// The `登录时启动` row ended: whether it is on now.
    LoginStarted(Result<bool, CoreError>),
}

/// A toast about what failed: `what`, then what the managed core said.
pub(super) fn failure(what: &str, err: &CoreError) -> String {
    format!("{what}：{}", core_reason(err))
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
    pub(super) fn managed_core(&self) -> Option<Arc<ManagedCore>> {
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
                self.load_subscriptions();
                self.say_tun_dropped();
                self.check_authority();
                self.check_login_start();
            }
            // Nothing downloaded yet: look up what there is to download, and
            // ask before downloading it.
            CoreEvent::Started(Err(CoreError::NoBinary { path: None })) => {
                self.core_phase = Some(CorePhase::Starting(Step::CheckingRelease));
                self.spawn_core(|core, _| async move {
                    CoreEvent::Release(core.latest_release().await)
                });
            }
            CoreEvent::Started(Err(err))
            | CoreEvent::Release(Err(err))
            | CoreEvent::Installed(Err(err)) => {
                tracing::warn!("the managed kernel did not start: {err}");
                self.core_phase = Some(CorePhase::Failed(err));
            }
            CoreEvent::Release(Ok(release)) => {
                // Until the question is answered the page says there is no
                // kernel; `r` asks again after a `n`.
                self.core_phase = Some(CorePhase::Failed(CoreError::NoBinary { path: None }));
                self.ask_install(release, false);
            }
            CoreEvent::Installed(Ok(())) => self.begin_core(),
            CoreEvent::UpgradeChecked(result) => {
                self.changing.remove(&Setting::UpgradeCore);
                match result {
                    Ok(Upgrade::Current(version)) => {
                        self.toast(ToastKind::Success, format!("已是最新（{version}）"));
                    }
                    Ok(Upgrade::Available(release)) => self.ask_install(release, true),
                    Err(err) => {
                        let reason = core_reason(&err);
                        self.toast(ToastKind::Error, format!("检查更新失败：{reason}"));
                    }
                }
            }
            CoreEvent::Authority(_) | CoreEvent::Setcap(_) | CoreEvent::Authorized(_) => {
                self.on_authorization(event);
            }
            CoreEvent::LoginStart(state) => self.login_start = Some(state),
            CoreEvent::LoginStarted(result) => {
                self.changing.remove(&Setting::LoginStart);
                match result {
                    Ok(on) => {
                        if let Some(state) = &mut self.login_start {
                            state.enabled = on;
                        }
                        let text = if on {
                            "内核会在登录时启动"
                        } else {
                            "内核不再在登录时启动"
                        };
                        self.toast(ToastKind::Success, text.to_owned());
                    }
                    Err(err) => self.toast(ToastKind::Error, failure("设置失败", &err)),
                }
            }
            CoreEvent::Regenerated(Ok(())) => {
                // The reload itself is the one every other kernel has.
                self.changing.remove(&Setting::Reload);
                self.send_change(Change::Reload);
            }
            CoreEvent::Regenerated(Err(err)) => {
                self.changing.remove(&Setting::Reload);
                self.toast(ToastKind::Error, failure("重载配置失败", &err));
            }
            CoreEvent::Upgraded(result) => {
                self.changing.remove(&Setting::UpgradeCore);
                match result {
                    Ok(tag) => {
                        self.toast(ToastKind::Success, format!("已升级到 {tag}，内核已重启"));
                        // The new file has no capabilities.
                        self.say_tun_dropped();
                        self.check_authority();
                    }
                    Err(err) => {
                        let reason = core_reason(&err);
                        self.toast(ToastKind::Error, format!("升级失败：{reason}"));
                    }
                }
            }
            CoreEvent::Stopped(_) | CoreEvent::Restarted(_) | CoreEvent::Alive(_) => {
                self.on_process(event);
            }
        }
    }

    /// What became of the kernel's process: stopped, started again, or gone
    /// without being asked.
    fn on_process(&mut self, event: CoreEvent) {
        match event {
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
                self.say_tun_dropped();
                self.check_authority();
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
            _ => {}
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

    /// `开启 TUN` on a managed core: the kernel needs the capabilities
    /// first, which asks the user (plan §8). A kernel on a `nosuid`
    /// mount cannot have them, which is said before anything is asked.
    pub(super) fn ask_enable_tun(&mut self, change: Change) {
        let Some(core) = self.managed_core() else {
            return;
        };
        let authorized = self.tun_authorized == Some(true);
        if !authorized && let Some(mount) = core.nosuid_mount() {
            self.toast(
                ToastKind::Error,
                format!(
                    "内核所在分区（{}）以 nosuid 挂载，无法授权，请在 [core].binary 指定其他位置",
                    mount.display()
                ),
            );
            return;
        }
        self.pending = Some(if authorized {
            Confirm::Change(change)
        } else {
            Confirm::AuthorizeTun
        });
        self.overlay = Some(Overlay::Confirm);
        self.dirty = true;
    }

    /// `y` to the question about the capabilities: asks for them through the
    /// desktop's dialog first.
    pub(super) fn begin_authorization(&mut self) {
        if self.changing.contains_key(&Setting::Tun) {
            return;
        }
        self.changing.insert(Setting::Tun, Stage::Sending);
        self.dirty = true;
        self.spawn_core(
            |core, _| async move { CoreEvent::Setcap(core.authorize_graphical().await) },
        );
    }

    /// The capabilities are set: the kernel starts again to have them.
    fn finish_authorization(&mut self) {
        self.spawn_core(|core, _| async move {
            CoreEvent::Authorized(core.finish_authorization().await)
        });
    }

    /// The managed core, when `sudo` has to be asked on the terminal: the
    /// main loop gives the terminal back, runs it, and reports with
    /// [`App::finish_terminal`].
    pub fn take_suspend(&mut self) -> Option<Arc<ManagedCore>> {
        self.suspend.take()
    }

    /// What the terminal run of `sudo` came to.
    pub fn finish_terminal(&mut self, result: Result<bool, CoreError>) {
        self.dirty = true;
        match result {
            Ok(true) => self.finish_authorization(),
            Ok(false) => {
                self.changing.remove(&Setting::Tun);
                self.toast(ToastKind::Error, "授权没有完成，TUN 保持关闭".to_owned());
            }
            Err(err) => {
                self.changing.remove(&Setting::Tun);
                self.toast(ToastKind::Error, failure("授权失败", &err));
            }
        }
    }

    /// What came of the capabilities TUN needs: what `getcap` says, what
    /// the dialog of the desktop (or `sudo`) did, and the restart after it.
    fn on_authorization(&mut self, event: CoreEvent) {
        match event {
            CoreEvent::Authority(authorized) => self.tun_authorized = Some(authorized),
            CoreEvent::Setcap(Ok(Graphical::Done)) => self.finish_authorization(),
            CoreEvent::Setcap(Ok(Graphical::Cancelled)) => {
                self.changing.remove(&Setting::Tun);
                self.toast(ToastKind::Error, "已取消授权，TUN 保持关闭".to_owned());
            }
            // Nobody to ask on the desktop (no polkit agent, as on a tiling
            // window manager): `sudo`, on the terminal.
            CoreEvent::Setcap(Ok(Graphical::NeedsTerminal)) => self.suspend = self.managed_core(),
            CoreEvent::Authorized(Ok(())) => {
                self.tun_authorized = Some(true);
                self.changing.remove(&Setting::Tun);
                // The kernel runs with the capabilities: TUN can be turned on.
                self.send_change(Change::Patch(Patch::Tun(true)));
            }
            CoreEvent::Setcap(Err(err)) | CoreEvent::Authorized(Err(err)) => {
                self.changing.remove(&Setting::Tun);
                self.toast(ToastKind::Error, failure("授权失败", &err));
            }
            _ => {}
        }
    }

    /// Asks whether the kernel starts at login.
    pub(super) fn check_login_start(&self) {
        self.spawn_core(|core, _| async move {
            CoreEvent::LoginStart(LoginStart {
                supported: core.supports_login_start().await,
                enabled: core.login_start_enabled(),
            })
        });
    }

    /// `登录时启动`: turns it on or off. Turning it on restarts the kernel
    /// as the unit, which takes about a second.
    pub(super) fn toggle_login_start(&mut self) {
        let Some(state) = self.login_start else {
            return;
        };
        if self.changing.contains_key(&Setting::LoginStart) {
            return;
        }
        let on = !state.enabled;
        self.changing.insert(Setting::LoginStart, Stage::Sending);
        self.dirty = true;
        self.spawn_core(move |core, _| async move {
            CoreEvent::LoginStarted(core.set_login_start(on).await.map(|()| on))
        });
    }

    /// Asks the kernel's file what it may do.
    pub(super) fn check_authority(&self) {
        self.spawn_core(|core, _| async move { CoreEvent::Authority(core.tun_authorized().await) });
    }

    /// Says that TUN was turned off because the kernel lost its
    /// capabilities (plan §8, R2).
    fn say_tun_dropped(&mut self) {
        if self
            .managed_core()
            .is_some_and(|core| core.take_tun_dropped())
        {
            self.toast(ToastKind::Error, "内核已更新，TUN 需要重新授权".to_owned());
        }
    }

    /// What a managed kernel is changed to, written down for its next
    /// start. The change has taken effect either way: failing to keep it
    /// is a thing to say, not to undo.
    pub(super) fn persist_patch(&mut self, patch: Patch) {
        let Some(core) = self.managed_core() else {
            return;
        };
        let change = match patch {
            Patch::Mode(mode) => Persist::Mode(mode.name().to_owned()),
            Patch::AllowLan(on) => Persist::AllowLan(on),
            Patch::LogLevel(level) => Persist::LogLevel(level.to_owned()),
            Patch::Ipv6(on) => Persist::Ipv6(on),
            Patch::Port(Port::Mixed, number) => Persist::MixedPort(number),
            // A managed kernel has no other ports.
            Patch::Port(..) => return,
            Patch::Tun(on) => Persist::Tun(on),
        };
        if let Err(err) = core.persist(change) {
            self.toast(ToastKind::Error, failure("已生效，但没能保存", &err));
        }
    }

    /// `y` to `重载配置` on a managed core: the config is written again
    /// from the subscription in use, and then the kernel reads it.
    pub(super) fn regenerate_config(&mut self) {
        if self.changing.contains_key(&Setting::Reload) {
            return;
        }
        self.changing.insert(Setting::Reload, Stage::Sending);
        self.dirty = true;
        self.spawn_core(|core, _| async move { CoreEvent::Regenerated(core.regenerate().await) });
    }

    /// The question `停止内核` asks.
    pub(super) fn ask_stop_core(&mut self) {
        self.pending = Some(Confirm::StopCore);
        self.overlay = Some(Overlay::Confirm);
        self.dirty = true;
    }

    /// The question that goes before a download of the kernel.
    fn ask_install(&mut self, release: Release, upgrade: bool) {
        self.pending = Some(Confirm::InstallCore { release, upgrade });
        self.overlay = Some(Overlay::Confirm);
        self.dirty = true;
    }

    /// `升级内核`: asks whether a newer kernel is out; a newer one is
    /// offered in a question.
    pub(super) fn check_upgrade(&mut self) {
        if self.changing.contains_key(&Setting::UpgradeCore) {
            return;
        }
        self.changing.insert(Setting::UpgradeCore, Stage::Sending);
        self.dirty = true;
        self.spawn_core(
            |core, _| async move { CoreEvent::UpgradeChecked(core.check_upgrade().await) },
        );
    }

    /// `y` to downloading the kernel at the first start: the kernel is
    /// downloaded, and started when it is in place.
    pub(super) fn install_core(&mut self, release: Release) {
        self.core_phase = Some(CorePhase::Starting(Step::Downloading(None)));
        self.dirty = true;
        self.spawn_core(|core, outbox| async move {
            let result = core
                .install(&release, |step| {
                    outbox.try_send(Msg::Core(CoreEvent::Step(step)));
                })
                .await;
            CoreEvent::Installed(result)
        });
    }

    /// `y` to an upgrade: downloads the kernel, puts it in place, and
    /// restarts the kernel on it.
    pub(super) fn upgrade_core(&mut self, release: Release) {
        self.changing.insert(Setting::UpgradeCore, Stage::Sending);
        self.dirty = true;
        self.spawn_core(|core, _| async move {
            let result = match core.install(&release, |_| {}).await {
                Ok(()) => core.restart().await.map(|()| release.tag),
                Err(err) => Err(err),
            };
            CoreEvent::Upgraded(result)
        });
    }

    /// The rows of the settings page, top to bottom: a managed core has the
    /// rows of its own and upgrades its kernel itself; an external
    /// controller has the kernel's `/upgrade`.
    pub fn settings(&self) -> Vec<Setting> {
        let managed = self.is_managed();
        Setting::ALL
            .into_iter()
            .filter(|setting| {
                if managed {
                    !setting.external_only()
                } else {
                    !setting.managed_only()
                }
            })
            .collect()
    }

    /// The setting under the settings page's cursor.
    pub fn selected_setting(&self) -> Setting {
        let rows = self.settings();
        rows[self.settings_view.cursor.min(rows.len() - 1)]
    }

    /// Where the managed core's kernel is, or is to be downloaded to.
    pub fn core_binary(&self) -> Option<std::path::PathBuf> {
        self.managed_core().map(|core| core.binary_path())
    }

    /// Whether the page draws the managed kernel's own state instead of its
    /// data.
    pub fn core_pane(&self) -> Option<&CorePhase> {
        self.core_phase
            .as_ref()
            .filter(|phase| !matches!(phase, CorePhase::Running))
    }
}
