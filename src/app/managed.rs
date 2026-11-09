//! The managed core's side of [`App`] (plan §3, §5): bringing the kernel
//! up before the pages have a controller to talk to, and what the screen
//! shows meanwhile.

use std::sync::Arc;

use super::{
    App, Change, Confirm, Field, Msg, Outbox, Overlay, Patch, Port, Reconnect, Setting, Stage,
    ToastKind,
};
use crate::{
    core::{
        CoreError, DownloadSettings, Graphical, ManagedCore, Persist, Release, Route, Step,
        SystemProxy, Upgrade, proxy_url, shown_proxy,
    },
    ui::text::core_reason,
};

/// What the proxy box offers when no proxy was given before: where a
/// desktop proxy client usually listens.
const PROXY_SUGGESTION: &str = "http://127.0.0.1:7890";

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
    /// A DNS key was set (or cleared) in the DNS popup, if the kernel took
    /// it.
    Dns {
        key: &'static str,
        result: Result<(), CoreError>,
    },
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
    /// Whether the system proxy points at the kernel, as tabby left it.
    SystemProxy(SystemProxy),
    /// The `系统代理` row ended: whether it is on now.
    SystemProxySet(Result<bool, CoreError>),
    /// The system proxy was left pointing at this port when the kernel went
    /// away without tabby (plan R6).
    ProxyDangling(u16),
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
    pub(super) fn spawn_core<F, Fut>(&self, work: F)
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
            // Before the kernel is started again: a proxy that points at a
            // kernel that is not running is only told while it is not.
            if let Some(port) = core.system_proxy_dangling().await {
                outbox.send(Msg::Core(CoreEvent::ProxyDangling(port))).await;
            }
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
                self.check_system_proxy();
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
            CoreEvent::SystemProxy(_)
            | CoreEvent::SystemProxySet(_)
            | CoreEvent::ProxyDangling(_)
            | CoreEvent::LoginStart(_)
            | CoreEvent::LoginStarted(_) => self.on_session(event),
            CoreEvent::Dns { key, result } => self.on_dns(key, result),
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
        self.spawn_core(|core, outbox| async move {
            // The proxy of the desktop must not be left pointing at a kernel
            // that is about to be gone; failing to put it back does not keep
            // the kernel from stopping, and the row shows it still on.
            if let Err(err) = core.restore_system_proxy().await {
                tracing::warn!("cannot put the system proxy back: {err}");
            }
            outbox
                .send(Msg::Core(CoreEvent::SystemProxy(core.system_proxy())))
                .await;
            CoreEvent::Stopped(core.stop().await)
        });
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

    /// What the session has of the kernel: whether it starts at login and
    /// whether the desktop's proxy follows it.
    fn on_session(&mut self, event: CoreEvent) {
        match event {
            CoreEvent::LoginStart(state) => self.login_start = Some(state),
            CoreEvent::LoginStarted(result) => {
                let on = self.switched(Setting::LoginStart, result, [
                    "内核不再在登录时启动",
                    "内核会在登录时启动",
                ]);
                if let (Some(on), Some(state)) = (on, &mut self.login_start) {
                    state.enabled = on;
                }
            }
            CoreEvent::SystemProxy(state) => self.system_proxy = Some(state),
            CoreEvent::SystemProxySet(result) => {
                let on = self.switched(Setting::SystemProxy, result, [
                    "系统代理已恢复",
                    "系统代理已指向内核",
                ]);
                if let (Some(on), Some(state)) = (on, &mut self.system_proxy) {
                    state.enabled = on;
                }
            }
            CoreEvent::ProxyDangling(port) => self.toast(
                ToastKind::Error,
                format!(
                    "上次内核退出时系统代理没有恢复，仍指向 127.0.0.1:{port}；内核已重新启动，不需要时在设置页关闭系统代理"
                ),
            ),
            _ => {}
        }
    }

    /// The end of a switch: the row is free again, and `texts` (off, on)
    /// say what came of it. `Some(on)` when it went through.
    fn switched(
        &mut self,
        setting: Setting,
        result: Result<bool, CoreError>,
        texts: [&str; 2],
    ) -> Option<bool> {
        self.changing.remove(&setting);
        match result {
            Ok(on) => {
                self.toast(ToastKind::Success, texts[usize::from(on)].to_owned());
                Some(on)
            }
            Err(err) => {
                self.toast(ToastKind::Error, failure("设置失败", &err));
                None
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

    /// Asks whether the system proxy points at the kernel.
    pub(super) fn check_system_proxy(&self) {
        self.spawn_core(|core, _| async move { CoreEvent::SystemProxy(core.system_proxy()) });
    }

    /// `系统代理`: points the desktop's proxy at the kernel's Mixed port, or
    /// puts it back.
    pub(super) fn toggle_system_proxy(&mut self) {
        let Some(state) = self.system_proxy else {
            return;
        };
        if self.changing.contains_key(&Setting::SystemProxy) {
            return;
        }
        let Some(Ok(configs)) = &self.configs else {
            return;
        };
        let on = !state.enabled;
        let port = Port::Mixed.of(configs).filter(|port| *port > 0);
        // Putting it back needs no port.
        let Some(port) = port.or((!on).then_some(0)) else {
            self.toast(ToastKind::Error, "Mixed 端口没有启用".to_owned());
            return;
        };
        self.changing.insert(Setting::SystemProxy, Stage::Sending);
        self.dirty = true;
        self.spawn_core(move |core, _| async move {
            CoreEvent::SystemProxySet(core.set_system_proxy(on, port).await.map(|()| on))
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
        // The desktop was pointed at the port it was; it follows the kernel.
        if let Patch::Port(Port::Mixed, number) = patch
            && self.system_proxy.is_some_and(|state| state.enabled)
        {
            let on = number > 0;
            self.spawn_core(move |core, _| async move {
                CoreEvent::SystemProxySet(core.set_system_proxy(on, number).await.map(|()| on))
            });
        }
    }

    /// `订阅下载`: directly, then through the managed kernel, then through
    /// a proxy (the box asks which), then directly again.
    pub(super) fn cycle_subscription_download(&mut self) {
        let Some(core) = self.managed_core() else {
            return;
        };
        let download = core.settings().subscription_download;
        match download.via {
            Route::Direct => self.set_subscription_download(DownloadSettings {
                via: Route::Core,
                ..download
            }),
            Route::Core => {
                let typed = if download.proxy.is_empty() {
                    PROXY_SUGGESTION.to_owned()
                } else {
                    download.proxy
                };
                self.settings_view.editing = Some((Field::Proxy, tui_input::Input::new(typed)));
                self.dirty = true;
            }
            Route::Proxy => self.set_subscription_download(DownloadSettings {
                via: Route::Direct,
                ..download
            }),
        }
    }

    /// `Enter` in the proxy box: a proxy address is kept, and downloads go
    /// through it; nothing typed closes the box; anything else is refused
    /// with the box left open to fix it.
    pub(super) fn submit_proxy(&mut self) {
        let Some((Field::Proxy, input)) = self.settings_view.editing.take() else {
            return;
        };
        self.dirty = true;
        let text = input.value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        if proxy_url(&text).is_none() {
            self.settings_view.editing = Some((Field::Proxy, input));
            self.toast(
                ToastKind::Error,
                "代理须是 http://、https:// 或 socks5:// 开头的地址".to_owned(),
            );
            return;
        }
        self.set_subscription_download(DownloadSettings {
            via: Route::Proxy,
            proxy: text,
        });
    }

    /// Writes down how subscriptions are downloaded, and says so.
    fn set_subscription_download(&mut self, download: DownloadSettings) {
        let Some(core) = self.managed_core() else {
            return;
        };
        let said = match download.via {
            Route::Direct => "订阅改为直连下载".to_owned(),
            Route::Core => "订阅改为经托管内核下载".to_owned(),
            Route::Proxy => format!("订阅改为经 {} 下载", shown_proxy(&download.proxy)),
        };
        match core.persist(Persist::SubscriptionDownload(download)) {
            Ok(()) => self.toast(ToastKind::Success, said),
            Err(err) => self.toast(ToastKind::Error, failure("没能保存", &err)),
        }
        self.dirty = true;
    }

    /// How subscriptions are downloaded, and the managed kernel's mixed
    /// port they would go through; `None` on an external controller.
    pub fn subscription_download(&self) -> Option<(DownloadSettings, u16)> {
        let settings = self.managed_core()?.settings();
        Some((settings.subscription_download, settings.mixed_port))
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
