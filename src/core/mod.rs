//! The kernel tabby manages itself (plan `2026-11-06-managed-core.md`): its
//! files, its process, and how tabby finds it again after a restart.
//!
//! Everything that touches the system goes through [`Runner`], so the
//! decisions made here can be tested without systemd.

mod autostart;
mod encrypted;
mod files;
mod install;
mod privilege;
mod profiles;
mod runner;
mod runtime;
mod settings;
mod state;
mod subscription;
mod supervisor;
mod sysproxy;

use std::{
    env,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError, atomic::AtomicBool},
    time::{Duration, Instant},
};

use color_eyre::{Result, eyre::WrapErr};
use tokio::sync::OnceCell;

use crate::api::{client::ApiClient, error::ApiError, models::CoreKind};
pub use encrypted::Fault;
use install::Target;
pub use install::{Release, Upgrade};
pub use privilege::Graphical;
pub use profiles::Outcome;
use runner::Runner;
pub use runner::System;
use settings::CoreSettings;
pub use settings::{DNS_KEYS, DnsKind, Persist};
use state::State;
pub use subscription::{Kind, Store, Subscription};
use supervisor::Mode;
pub use sysproxy::SystemProxy;

/// How long a kernel just started has to answer on its controller.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// How often a starting kernel is asked whether it is up.
const READY_POLL: Duration = Duration::from_millis(200);

/// How long a kernel asked to stop has before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// Where the managed core keeps its files, under `<data>/core/`.
#[derive(Debug, Clone)]
struct Paths {
    root: PathBuf,
}

impl Paths {
    fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("core"),
        }
    }

    /// The kernel's working directory (`-d`): geo data and its caches live
    /// here, and it only reads files from inside it.
    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// The generated config file (`-f`).
    fn config(&self) -> PathBuf {
        self.home().join("config.yaml")
    }

    fn state(&self) -> PathBuf {
        self.root.join("state.toml")
    }

    /// What the kernel is set up with, written by tabby.
    fn settings(&self) -> PathBuf {
        self.root.join("settings.toml")
    }

    /// The kernel tabby downloads.
    fn bin(&self) -> PathBuf {
        self.root.join("bin").join("mihomo")
    }
    /// What the system proxy was before tabby pointed it at the kernel.
    fn system_proxy(&self) -> PathBuf {
        self.root.join("system-proxy.toml")
    }

    /// The kernel's output when it runs without systemd.
    fn log(&self) -> PathBuf {
        self.root.join("core.log")
    }
}

/// What [`ManagedCore::ensure_running`] is doing, for the screen that waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Looking for a kernel already running, or starting one.
    Starting,
    /// The kernel is running; its controller does not answer yet.
    WaitingController,
    /// Asking where the newest kernel is.
    CheckingRelease,
    /// Downloading it, `percent` done when the size is known.
    Downloading(Option<u8>),
    /// Unpacking it and making sure it runs.
    Installing,
}

/// What the managed core was asked to do when a program failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Restart,
    Stop,
    Install,
    Validate,
    Authorize,
    Autostart,
    SystemProxy,
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Start => "start the kernel",
            Self::Restart => "restart the kernel",
            Self::Install => "install the kernel",
            Self::Stop => "stop the kernel",
            Self::Validate => "check the subscription",
            Self::Authorize => "authorize the kernel",
            Self::Autostart => "set up the start at login",
            Self::SystemProxy => "set the system proxy",
        })
    }
}

/// Why the managed kernel could not be brought up or taken down.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// There is no kernel to run: `path` is where `[core].binary` points
    /// when it does not hold a file, `None` when tabby has not downloaded
    /// one yet and there is none on `$PATH`.
    #[error("no mihomo binary")]
    NoBinary { path: Option<PathBuf> },
    /// Something other than the managed kernel answers on its port.
    #[error("port {0} is taken by another program")]
    PortTaken(u16),
    /// The kernel did not come up; `log` is the end of its output.
    #[error("the kernel did not start")]
    StartFailed { log: String },
    /// A program tabby leans on failed: what it was asked, and what it said.
    #[error("cannot {action}: {message}")]
    Command { action: Action, message: String },
    #[error("{context}: {source}")]
    Io {
        context: &'static str,
        source: io::Error,
    },
    /// The subscription could not be downloaded.
    #[error("cannot download the subscription: {0}")]
    Fetch(String),
    /// The kernel does not accept the subscription, and says why.
    #[error("the kernel does not accept the subscription: {0}")]
    Invalid(String),
    /// A subscription whose top level is not a block mapping.
    #[error("the subscription is not a block mapping")]
    UnsupportedProfile,
    /// No such subscription in the list.
    #[error("no such subscription")]
    NoSubscription,
    /// The subscription in use cannot be deleted.
    #[error("the subscription is in use")]
    ActiveSubscription,
    /// The file a subscription was imported from is gone; its last copy is
    /// kept.
    #[error("{} is gone", .0.display())]
    SourceGone(PathBuf),
    /// There is no systemd user manager (or home directory) to keep a unit
    /// in.
    #[error("the start at login needs the systemd user manager")]
    NeedsSystemd,
    /// The desktop has no system proxy tabby knows how to set.
    #[error("the desktop has no system proxy tabby can set")]
    NoDesktopProxy,
    /// The subscription is encrypted with age, and no key was given for it.
    #[error("the subscription is encrypted and needs an age key")]
    AgeKeyNeeded,
    /// The age key does not open the subscription.
    #[error("the age key cannot read the subscription: {0:?}")]
    Age(Fault),
    /// The release list could not be read.
    #[error("cannot look up the latest kernel: {0}")]
    Release(String),
    /// The release has no kernel for this machine.
    #[error("no kernel for {arch} in the release")]
    NoAsset { arch: String },
    /// What was downloaded is not what the release says it is.
    #[error("checksum mismatch: {0}")]
    Checksum(String),
    #[error("download failed: {0}")]
    Download(String),
}

impl CoreError {
    fn io(context: &'static str, source: io::Error) -> Self {
        Self::Io { context, source }
    }
}

/// How the controller answered a probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    /// The managed kernel.
    Ready,
    /// Nothing listens.
    Down,
    /// Something listens that is not the managed kernel: it rejected the
    /// secret, or is not mihomo.
    Foreign,
}

/// The waits `ManagedCore` makes; tests shorten them.
#[derive(Debug, Clone, Copy)]
struct Timing {
    ready: Duration,
    poll: Duration,
    stop_grace: Duration,
}

/// The kernel tabby runs for the user.
#[derive(Debug)]
pub struct ManagedCore {
    paths: Paths,
    /// `[core].binary`.
    binary: Option<PathBuf>,
    /// `$PATH`, where a `mihomo` installed on the system is looked for when
    /// there is neither `[core].binary` nor a kernel tabby keeps.
    search_path: Option<OsString>,
    state: State,
    /// What `settings.toml` says, and what is changed since.
    settings: Mutex<CoreSettings>,
    client: ApiClient,
    runner: Arc<dyn Runner>,
    /// `$XDG_RUNTIME_DIR`, where a running user manager has its socket.
    runtime_dir: Option<PathBuf>,
    /// How the kernel is kept running, settled on first use.
    mode: OnceCell<Mode>,
    timing: Timing,
    /// Where releases are looked up.
    release_api: String,
    target: Target,
    /// Held across the operations that change the subscriptions, so that two
    /// do not write the list at once.
    lock: tokio::sync::Mutex<()>,
    /// Downloads subscriptions.
    http: reqwest::Client,
    /// What the nodes of a share-link subscription are tested with.
    delay_url: String,
    /// TUN was turned off for want of the capabilities, and nobody has been
    /// told.
    tun_dropped: AtomicBool,
    /// Where the mounts are listed (`/proc/self/mountinfo`).
    mountinfo: PathBuf,
    /// Where the systemd user manager keeps the units of the user.
    unit_dir: Option<PathBuf>,
    /// The desktop whose system proxy tabby can set, if it is one.
    desktop: Option<sysproxy::Desktop>,
}

impl ManagedCore {
    /// The managed core with its files in `data_dir`, running programs for
    /// real. Makes `state.toml` when there is none. `delay_url` is the page
    /// the nodes of a share-link subscription are tested with.
    pub fn open(binary: Option<PathBuf>, data_dir: &Path, delay_url: &str) -> Result<Self> {
        let runtime_dir = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        let mut core = Self::with(binary, data_dir, Arc::new(System), runtime_dir)?;
        delay_url.clone_into(&mut core.delay_url);
        core.search_path = env::var_os("PATH");
        Ok(core)
    }

    fn with(
        binary: Option<PathBuf>,
        data_dir: &Path,
        runner: Arc<dyn Runner>,
        runtime_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let paths = Paths::new(data_dir);
        let state = State::load_or_create(&paths.state())
            .wrap_err_with(|| format!("cannot use {}", paths.state().display()))?;
        let settings = CoreSettings::load(&paths.settings())
            .wrap_err_with(|| format!("cannot use {}", paths.settings().display()))?;
        let client = ApiClient::new(
            &format!("http://127.0.0.1:{}", state.port),
            Some(&state.secret),
        )?;
        Ok(Self {
            paths,
            binary,
            search_path: None,
            state,
            settings: Mutex::new(settings),
            client,
            runner,
            runtime_dir,
            mode: OnceCell::new(),
            release_api: install::API_BASE.to_owned(),
            target: Target::current(),
            lock: tokio::sync::Mutex::new(()),
            http: reqwest::Client::builder()
                .no_proxy()
                .user_agent(format!(
                    "tabby/v{} clash-verge Platform/{}",
                    env!("CARGO_PKG_VERSION"),
                    env::consts::OS
                ))
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(60))
                .build()
                .wrap_err("cannot build HTTP client")?,
            delay_url: crate::config::DEFAULT_DELAY_URL.to_owned(),
            tun_dropped: AtomicBool::new(false),
            mountinfo: PathBuf::from("/proc/self/mountinfo"),
            desktop: std::env::var("XDG_CURRENT_DESKTOP")
                .ok()
                .and_then(|current| sysproxy::Desktop::detect(&current)),
            unit_dir: directories::BaseDirs::new()
                .map(|dirs| dirs.config_dir().join("systemd").join("user")),
            timing: Timing {
                ready: READY_TIMEOUT,
                poll: READY_POLL,
                stop_grace: STOP_GRACE,
            },
        })
    }

    /// Where the managed kernel's controller listens.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.state.port)
    }

    pub fn secret(&self) -> &str {
        &self.state.secret
    }

    /// What the managed kernel is set up with.
    pub fn settings(&self) -> CoreSettings {
        self.settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Writes a setting down, to be what the kernel is started with next
    /// time.
    ///
    /// The config file is made again too: a kernel started by the session
    /// at login runs from it as it is, with no tabby to write it.
    pub fn persist(&self, change: Persist) -> Result<(), CoreError> {
        {
            let mut settings = self.settings.lock().unwrap_or_else(PoisonError::into_inner);
            settings.set(change);
            settings
                .save(&self.paths.settings())
                .map_err(|err| CoreError::io("cannot write settings.toml", err))?;
        }
        if self.paths.config().exists() {
            self.write_config_for(&self.subscriptions()?)?;
        }
        Ok(())
    }

    /// Sets `key` of the `[dns]` overrides (or, with `None`, lets the
    /// subscription's own value stand), and writes it down like
    /// [`ManagedCore::persist`].
    ///
    /// The kernel checks the config it would be run with first
    /// (`mihomo -t`): a value it does not accept would otherwise be kept
    /// and stop the kernel from starting next time. Nothing is changed
    /// when it complains.
    pub async fn set_dns(&self, key: &str, value: Option<toml::Value>) -> Result<(), CoreError> {
        let _guard = self.lock.lock().await;
        let mut next = self.settings();
        next.set_dns(key, value);
        let store = self.subscriptions()?;
        let text = runtime::render(&self.base_profile(&store)?, &next, &self.state)
            .map_err(|_| CoreError::UnsupportedProfile)?;
        if let Some(complaint) = self.check_text("dns", &text).await? {
            return Err(CoreError::Invalid(complaint));
        }
        {
            let mut settings = self.settings.lock().unwrap_or_else(PoisonError::into_inner);
            *settings = next;
            settings
                .save(&self.paths.settings())
                .map_err(|err| CoreError::io("cannot write settings.toml", err))?;
        }
        if self.paths.config().exists() {
            self.write_config_for(&store)?;
        }
        Ok(())
    }

    /// Writes the kernel's config file again from the subscription in use
    /// and the settings, for a kernel that is asked to read it again.
    pub async fn regenerate(&self) -> Result<(), CoreError> {
        let _guard = self.lock.lock().await;
        self.write_config()
    }

    /// Where the kernel to run is, whether or not it is there yet:
    /// `[core].binary`; else the one tabby keeps in its data directory; else
    /// a `mihomo` on `$PATH`; else where tabby would download one to.
    pub fn binary_path(&self) -> PathBuf {
        if let Some(binary) = &self.binary {
            return binary.clone();
        }
        let kept = self.paths.bin();
        if kept.is_file() {
            return kept;
        }
        self.on_path().unwrap_or(kept)
    }

    /// Whether the kernel is not one tabby keeps (`[core].binary`, or a
    /// `mihomo` on `$PATH`): tabby neither downloads nor replaces it.
    pub fn is_user_binary(&self) -> bool {
        self.binary.is_some() || (!self.paths.bin().is_file() && self.on_path().is_some())
    }

    /// The first executable `mihomo` in an absolute directory of `$PATH`.
    /// Relative entries are skipped: they name the working directory,
    /// which is not where an installed kernel is.
    fn on_path(&self) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        env::split_paths(self.search_path.as_ref()?)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join("mihomo"))
            .find(|candidate| {
                candidate
                    .metadata()
                    .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            })
    }

    /// The kernel binary to run, when there is one.
    pub fn binary(&self) -> Result<PathBuf, CoreError> {
        let path = self.binary_path();
        if path.is_file() {
            Ok(path)
        } else {
            Err(CoreError::NoBinary {
                path: self.binary.clone(),
            })
        }
    }

    /// Makes sure the kernel runs and its controller answers: attaches to
    /// one that is already there, else starts one (plan §5.2). `progress`
    /// hears what it is doing.
    pub async fn ensure_running(&self, progress: impl Fn(Step)) -> Result<(), CoreError> {
        match self.probe().await {
            Probe::Ready => return Ok(()),
            Probe::Foreign => return Err(CoreError::PortTaken(self.state.port)),
            Probe::Down => {}
        }
        if self.status().await.running() {
            progress(Step::WaitingController);
            return self.wait_ready().await;
        }
        progress(Step::Starting);
        let binary = self.binary()?;
        self.write_config()?;
        self.launch(&binary).await?;
        progress(Step::WaitingController);
        self.wait_ready().await
    }

    /// Restarts the kernel on a freshly written config, and waits for its
    /// controller.
    pub async fn restart(&self) -> Result<(), CoreError> {
        let binary = self.binary()?;
        self.write_config()?;
        self.relaunch(&binary).await?;
        self.wait_ready().await
    }

    /// Whether the kernel process is there, as far as its supervisor says.
    pub async fn alive(&self) -> bool {
        self.status().await.running()
    }

    fn write_config(&self) -> Result<(), CoreError> {
        // A kernel without the capabilities cannot run a TUN, whatever the
        // settings say.
        self.drop_tun_without_authority();
        self.write_config_for(&self.subscriptions()?)
    }

    async fn probe(&self) -> Probe {
        match self.client.version().await {
            Ok(version) if version.kind() == CoreKind::Mihomo => Probe::Ready,
            Err(ApiError::Connect(_) | ApiError::Timeout) => Probe::Down,
            // Something answered, and it is not the managed kernel: it
            // rejected the secret, or it is not mihomo at all.
            Ok(_) | Err(_) => Probe::Foreign,
        }
    }

    /// Waits for the controller of a kernel that was just started, and
    /// gives up on it, with the end of its log, when it stops or takes too
    /// long.
    async fn wait_ready(&self) -> Result<(), CoreError> {
        let deadline = Instant::now() + self.timing.ready;
        loop {
            match self.probe().await {
                Probe::Ready => return Ok(()),
                Probe::Foreign => return Err(CoreError::PortTaken(self.state.port)),
                Probe::Down => {}
            }
            if !self.status().await.running() || Instant::now() >= deadline {
                return Err(self.start_failed().await);
            }
            tokio::time::sleep(self.timing.poll).await;
        }
    }

    /// The failure of a kernel that did not come up. A kernel that keeps
    /// being restarted is stopped, so it does not go on failing in the
    /// background while the error is read.
    async fn start_failed(&self) -> CoreError {
        let log = self.log_tail().await;
        if let Err(err) = self.stop().await {
            tracing::warn!("cannot stop the kernel that failed to start: {err}");
        }
        CoreError::StartFailed { log }
    }

    /// The kernel process run without systemd, as `state.toml` has it.
    fn stored_process(&self) -> Option<supervisor::Process> {
        let state = State::load_or_create(&self.paths.state()).ok()?;
        Some(supervisor::Process {
            pid: state.pid?,
            start: state.pid_start,
        })
    }

    /// Records the kernel process run without systemd (and when it
    /// started), or forgets it.
    fn store_process(&self, pid: Option<u32>) -> Result<(), CoreError> {
        let mut state = State::load_or_create(&self.paths.state())
            .map_err(|err| CoreError::io("cannot read state.toml", err))?;
        state.pid = pid;
        state.pid_start = pid.and_then(supervisor::start_time);
        state
            .save(&self.paths.state())
            .map_err(|err| CoreError::io("cannot write state.toml", err))
    }
}
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod testing;
