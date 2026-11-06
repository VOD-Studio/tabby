//! The kernel tabby manages itself (plan `2026-11-06-managed-core.md`): its
//! files, its process, and how tabby finds it again after a restart.
//!
//! Everything that touches the system goes through [`Runner`], so the
//! decisions made here can be tested without systemd.

mod files;
mod install;
mod profiles;
mod runner;
mod runtime;
mod settings;
mod state;
mod subscription;
mod supervisor;

use std::{
    env, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use color_eyre::{Result, eyre::WrapErr};
use tokio::sync::OnceCell;

use crate::api::{client::ApiClient, error::ApiError, models::CoreKind};
use install::Target;
pub use install::{Release, Upgrade};
pub use profiles::Outcome;
use runner::Runner;
pub use runner::System;
use settings::CoreSettings;
use state::State;
pub use subscription::{Kind, Store, Subscription};
use supervisor::Mode;

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

    /// The kernel tabby downloads.
    fn bin(&self) -> PathBuf {
        self.root.join("bin").join("mihomo")
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
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Start => "start the kernel",
            Self::Restart => "restart the kernel",
            Self::Install => "install the kernel",
            Self::Stop => "stop the kernel",
            Self::Validate => "check the subscription",
        })
    }
}

/// Why the managed kernel could not be brought up or taken down.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// There is no kernel to run: `path` is where `[core].binary` points
    /// when it does not hold a file, `None` when tabby has not downloaded
    /// one yet.
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
    state: State,
    settings: CoreSettings,
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
}

impl ManagedCore {
    /// The managed core with its files in `data_dir`, running programs for
    /// real. Makes `state.toml` when there is none. `delay_url` is the page
    /// the nodes of a share-link subscription are tested with.
    pub fn open(binary: Option<PathBuf>, data_dir: &Path, delay_url: &str) -> Result<Self> {
        let runtime_dir = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        let mut core = Self::with(binary, data_dir, Arc::new(System), runtime_dir)?;
        delay_url.clone_into(&mut core.delay_url);
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
        let client = ApiClient::new(
            &format!("http://127.0.0.1:{}", state.port),
            Some(&state.secret),
        )?;
        Ok(Self {
            paths,
            binary,
            state,
            settings: CoreSettings::default(),
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

    /// Where the kernel to run is, whether or not it is there yet:
    /// `[core].binary`, else the one tabby keeps in its data directory.
    pub fn binary_path(&self) -> PathBuf {
        self.binary.clone().unwrap_or_else(|| self.paths.bin())
    }

    /// Whether `[core].binary` names the kernel: tabby neither downloads nor
    /// replaces one that is the user's own.
    pub fn is_user_binary(&self) -> bool {
        self.binary.is_some()
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
