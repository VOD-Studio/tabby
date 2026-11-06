//! Keeping the kernel running: a transient systemd user unit where there is
//! a user manager, else a process of its own (plan §5).

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::Path,
    process::{Command, Output, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use super::{Action, CoreError, ManagedCore, runner::Cmd};

/// Lines of the kernel's output the error page shows.
const LOG_LINES: usize = 20;

/// How often a kernel asked to stop is looked at again.
const STOP_POLL: Duration = Duration::from_millis(100);

/// A process, as it is told apart from the next one with its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    /// When it started, from [`start_time`].
    pub start: Option<u64>,
}

/// When process `pid` started, in clock ticks since boot (field 22 of
/// `/proc/<pid>/stat`); `None` when there is no such process, it has ended
/// without being waited for, or there is no `/proc`.
pub fn start_time(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The name in the second field may hold spaces and parentheses: the
    // rest starts after the last one. Then comes the state, field 3.
    let mut fields = stat.rsplit_once(')')?.1.split_whitespace();
    let state = fields.next()?;
    if state == "Z" || state == "X" {
        return None;
    }
    fields.nth(18)?.parse().ok()
}

/// How the kernel is kept running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A transient unit of the systemd user manager.
    Systemd,
    /// A process of its own, in a group of its own, outliving tabby.
    Fallback,
}

/// What the supervisor says about the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Active,
    /// Starting, or waiting to be started again after a failure.
    Activating,
    Failed,
    /// Not running, and not known to the supervisor.
    Inactive,
}

impl Status {
    fn parse(output: &str) -> Self {
        match output.trim() {
            "active" | "reloading" => Self::Active,
            "activating" => Self::Activating,
            "failed" => Self::Failed,
            _ => Self::Inactive,
        }
    }

    pub fn running(self) -> bool {
        matches!(self, Self::Active | Self::Activating)
    }
}

impl ManagedCore {
    /// Runs `cmd` on a thread of its own: the runner blocks.
    pub(super) async fn exec(&self, cmd: Cmd) -> io::Result<Output> {
        let runner = Arc::clone(&self.runner);
        tokio::task::spawn_blocking(move || runner.run(&cmd))
            .await
            .map_err(io::Error::other)?
    }

    /// `exec`, turning a failure to run or a nonzero exit into an error that
    /// says what was being done.
    async fn exec_checked(&self, action: Action, cmd: Cmd) -> Result<(), CoreError> {
        let output = self.exec(cmd).await.map_err(|err| CoreError::Command {
            action,
            message: err.to_string(),
        })?;
        if output.status.success() {
            return Ok(());
        }
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(CoreError::Command {
            action,
            message: if message.is_empty() {
                format!("exit status {}", output.status)
            } else {
                message
            },
        })
    }

    pub(super) async fn mode(&self) -> Mode {
        *self.mode.get_or_init(|| self.detect_mode()).await
    }

    /// A user manager is there when its socket is, or when it says it
    /// runs.
    async fn detect_mode(&self) -> Mode {
        if let Some(dir) = &self.runtime_dir
            && dir.join("systemd/private").exists()
        {
            return Mode::Systemd;
        }
        let asked = self
            .exec(Cmd::new("systemctl", ["--user", "is-system-running"]))
            .await;
        match asked {
            Ok(output)
                if matches!(
                    String::from_utf8_lossy(&output.stdout).trim(),
                    "running" | "degraded"
                ) =>
            {
                Mode::Systemd
            }
            _ => Mode::Fallback,
        }
    }

    pub(super) async fn status(&self) -> Status {
        match self.mode().await {
            Mode::Systemd => {
                let asked = self
                    .exec(Cmd::new(
                        "systemctl",
                        ["--user", "is-active", self.state.unit.as_str()],
                    ))
                    .await;
                asked.map_or(Status::Inactive, |output| {
                    Status::parse(&String::from_utf8_lossy(&output.stdout))
                })
            }
            Mode::Fallback => match self.stored_process() {
                Some(process) if self.is_kernel(process).await => Status::Active,
                _ => Status::Inactive,
            },
        }
    }

    /// Starts the kernel `binary`.
    pub(super) async fn launch(&self, binary: &Path) -> Result<(), CoreError> {
        match self.mode().await {
            Mode::Systemd => {
                let mut args = vec![
                    "--user".to_owned(),
                    format!("--unit={}", self.state.unit),
                    "--collect".to_owned(),
                    "-p".to_owned(),
                    "Restart=on-failure".to_owned(),
                    "-p".to_owned(),
                    "RestartSec=2".to_owned(),
                    "--".to_owned(),
                ];
                args.extend(self.kernel_command(binary));
                self.exec_checked(Action::Start, Cmd::new("systemd-run", args))
                    .await
            }
            Mode::Fallback => {
                let pid = self
                    .spawn_detached(binary)
                    .map_err(|err| CoreError::Command {
                        action: Action::Start,
                        message: err.to_string(),
                    })?;
                self.store_process(Some(pid))
            }
        }
    }

    /// The kernel's own command line, program first.
    fn kernel_command(&self, binary: &Path) -> Vec<String> {
        vec![
            binary.to_string_lossy().into_owned(),
            "-d".to_owned(),
            self.paths.home().to_string_lossy().into_owned(),
            "-f".to_owned(),
            self.paths.config().to_string_lossy().into_owned(),
        ]
    }

    /// Starts the kernel again so that it reads its config anew: through
    /// the unit while there is one, else as a new start.
    pub(super) async fn relaunch(&self, binary: &Path) -> Result<(), CoreError> {
        match self.mode().await {
            Mode::Systemd if self.status().await.running() => {
                self.exec_checked(
                    Action::Restart,
                    Cmd::new("systemctl", ["--user", "restart", self.state.unit.as_str()]),
                )
                .await
            }
            Mode::Systemd => self.launch(binary).await,
            Mode::Fallback => {
                self.stop().await?;
                self.launch(binary).await
            }
        }
    }

    /// Stops the kernel; one that is not running is already stopped. It is
    /// asked to end first, so that it can clean up its routes, and killed
    /// if it does not (without systemd; a unit does that itself).
    pub async fn stop(&self) -> Result<(), CoreError> {
        match self.mode().await {
            Mode::Systemd => {
                if self.status().await == Status::Inactive {
                    return Ok(());
                }
                self.exec_checked(
                    Action::Stop,
                    Cmd::new("systemctl", ["--user", "stop", self.state.unit.as_str()]),
                )
                .await
            }
            Mode::Fallback => {
                let Some(process) = self.stored_process() else {
                    return Ok(());
                };
                if self.is_kernel(process).await {
                    self.signal("TERM", process.pid).await?;
                    let deadline = Instant::now() + self.timing.stop_grace;
                    while self.is_kernel(process).await {
                        if Instant::now() >= deadline {
                            self.signal("KILL", process.pid).await?;
                            break;
                        }
                        tokio::time::sleep(STOP_POLL).await;
                    }
                }
                self.store_process(None)
            }
        }
    }

    async fn signal(&self, name: &str, pid: u32) -> Result<(), CoreError> {
        self.exec_checked(
            Action::Stop,
            Cmd::new("kill", [format!("-{name}"), pid.to_string()]),
        )
        .await
    }

    /// Whether `process` is still the kernel tabby started, and not just a
    /// process with that number: it started when the kernel did. Without
    /// `/proc` only the number can be checked.
    async fn is_kernel(&self, process: Process) -> bool {
        if Path::new("/proc/self").exists() {
            return process.start.is_some() && start_time(process.pid) == process.start;
        }
        self.exec(Cmd::new("kill", ["-0".to_owned(), process.pid.to_string()]))
            .await
            .is_ok_and(|output| output.status.success())
    }

    /// Starts the kernel as a process of its own: in a group of its own, so
    /// that a ^C meant for tabby does not reach it, with its output in
    /// `core.log` (the last one kept as `core.log.1`). Returns its pid.
    fn spawn_detached(&self, binary: &Path) -> io::Result<u32> {
        let log = self.paths.log();
        if let Some(dir) = log.parent() {
            fs::create_dir_all(dir)?;
        }
        if log.exists() {
            fs::rename(&log, log.with_extension("log.1"))?;
        }
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let out: File = options.open(&log)?;
        let err = out.try_clone()?;
        let mut command = Command::new(binary);
        command
            .arg("-d")
            .arg(self.paths.home())
            .arg("-f")
            .arg(self.paths.config())
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn()?;
        let pid = child.id();
        // Reaps the kernel if it ends while tabby runs, so that it does not
        // stay a zombie that still looks like a process.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(pid)
    }

    /// The end of what the kernel said, for the page that explains why it
    /// did not start.
    pub(super) async fn log_tail(&self) -> String {
        match self.mode().await {
            Mode::Systemd => {
                let asked = self
                    .exec(Cmd::new(
                        "journalctl",
                        [
                            "--user",
                            "-u",
                            self.state.unit.as_str(),
                            "-n",
                            &LOG_LINES.to_string(),
                            "-o",
                            "cat",
                            "--no-pager",
                        ],
                    ))
                    .await;
                asked
                    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
                    .unwrap_or_default()
            }
            Mode::Fallback => {
                let text = fs::read_to_string(self.paths.log()).unwrap_or_default();
                let lines: Vec<&str> = text.lines().collect();
                lines[lines.len().saturating_sub(LOG_LINES)..]
                    .join("\n")
                    .trim()
                    .to_owned()
            }
        }
    }
}
