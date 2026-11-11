//! The kernel as a job of launchd on macOS (plan `2026-11-11-macos.md`
//! §4.1): what a transient unit of the systemd user manager is on Linux. The
//! job lives in the GUI domain of the user (`gui/<uid>`), is described by a
//! property list tabby writes, and outlives tabby.

use std::{fmt::Write as _, fs, io, path::Path, time::Instant};

use super::{
    Action, CoreError, ManagedCore,
    runner::Cmd,
    supervisor::{STOP_POLL, Status},
};

/// `text` as the content of an XML element.
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// What `launchctl print gui/<uid>/<label>` says of a job that is loaded.
/// Only the keys of the job itself count, the lines indented once: the
/// blocks in it have keys of their own (`state = active` of an endpoint).
fn parse_print(text: &str) -> Status {
    let value = |key: &str| {
        text.lines().find_map(|line| {
            let line = line.strip_prefix('\t')?;
            if line.starts_with(char::is_whitespace) {
                return None;
            }
            line.strip_prefix(key)?.strip_prefix(" = ").map(str::trim)
        })
    };
    match value("state") {
        Some("running") => Status::Active,
        Some("spawn scheduled" | "xpcproxy") => Status::Activating,
        // Not running: it ran and failed, or it ended (or never started).
        _ => match value("last exit code").map(str::parse::<i32>) {
            Some(Ok(code)) if code != 0 => Status::Failed,
            _ => Status::Inactive,
        },
    }
}

impl ManagedCore {
    /// The user's GUI domain.
    fn domain(uid: u32) -> String {
        format!("gui/{uid}")
    }

    /// The kernel's job in the domain.
    fn service(&self, uid: u32) -> String {
        format!("gui/{uid}/{}", self.state.unit)
    }

    /// Where the job tabby starts by itself is described: not among the
    /// launch agents, which would start it at login.
    fn job_path(&self) -> std::path::PathBuf {
        self.paths
            .root
            .join("launchd")
            .join(format!("{}.plist", self.state.unit))
    }

    /// The uid whose GUI domain launchd has, when it has one: a user logged
    /// in at the screen. Over SSH alone there is none, and the kernel runs
    /// as a process of its own.
    pub(super) async fn launchd_domain(&self) -> Option<u32> {
        let id = self.exec(Cmd::new("id", ["-u"])).await.ok()?;
        if !id.status.success() {
            return None;
        }
        let uid: u32 = String::from_utf8_lossy(&id.stdout).trim().parse().ok()?;
        let asked = self
            .exec(Cmd::new(
                "launchctl",
                ["print".to_owned(), Self::domain(uid)],
            ))
            .await
            .ok()?;
        asked.status.success().then_some(uid)
    }

    /// The job: the kernel as [`ManagedCore::kernel_command`] runs it,
    /// started when it is loaded, started again when it fails (as
    /// `Restart=on-failure`), with its output in `core.log`.
    pub(super) fn plist_text(&self, binary: &Path) -> String {
        let arguments = self
            .kernel_command(binary)
            .iter()
            .fold(String::new(), |mut list, arg| {
                let _ = writeln!(list, "\t\t<string>{}</string>", xml(arg));
                list
            });
        let log = xml(&self.paths.log().to_string_lossy());
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \t<key>Label</key>\n\
             \t<string>{label}</string>\n\
             \t<key>ProgramArguments</key>\n\
             \t<array>\n\
             {arguments}\
             \t</array>\n\
             \t<key>RunAtLoad</key>\n\
             \t<true/>\n\
             \t<key>KeepAlive</key>\n\
             \t<dict>\n\
             \t\t<key>SuccessfulExit</key>\n\
             \t\t<false/>\n\
             \t</dict>\n\
             \t<key>ThrottleInterval</key>\n\
             \t<integer>2</integer>\n\
             \t<key>ExitTimeOut</key>\n\
             \t<integer>{exit}</integer>\n\
             \t<key>StandardOutPath</key>\n\
             \t<string>{log}</string>\n\
             \t<key>StandardErrorPath</key>\n\
             \t<string>{log}</string>\n\
             </dict>\n\
             </plist>\n",
            label = xml(&self.state.unit),
            exit = self.timing.stop_grace.as_secs().max(1),
        )
    }

    /// What `launchctl print` says of the job; `None` when it is not
    /// loaded.
    async fn print_job(&self, uid: u32) -> Option<String> {
        let asked = self
            .exec(Cmd::new(
                "launchctl",
                ["print".to_owned(), self.service(uid)],
            ))
            .await
            .ok()?;
        asked
            .status
            .success()
            .then(|| String::from_utf8_lossy(&asked.stdout).into_owned())
    }

    pub(super) async fn launchd_status(&self, uid: u32) -> Status {
        self.print_job(uid)
            .await
            .map_or(Status::Inactive, |text| parse_print(&text))
    }

    /// Loads the job of `plist`, which starts the kernel; one still loaded
    /// (it ended, or runs from another file) goes first.
    pub(super) async fn launchd_start(
        &self,
        uid: u32,
        plist: &Path,
        action: Action,
    ) -> Result<(), CoreError> {
        self.launchd_stop(uid).await?;
        // The job appends to the log: it starts empty, the last one kept,
        // and private, as the kernel's own log is everywhere else.
        drop(
            self.fresh_log()
                .map_err(|err| CoreError::io("cannot open core.log", err))?,
        );
        self.exec_checked(
            action,
            Cmd::new(
                "launchctl",
                [
                    "bootstrap".to_owned(),
                    Self::domain(uid),
                    plist.to_string_lossy().into_owned(),
                ],
            ),
        )
        .await
    }

    /// Starts the kernel as the job tabby describes in its own directory,
    /// or among the launch agents when it starts at login. The file is
    /// written anew either way, in case the kernel changed.
    pub(super) async fn launchd_launch(
        &self,
        uid: u32,
        binary: &Path,
        action: Action,
    ) -> Result<(), CoreError> {
        let path = match self.unit_path() {
            Some(agent) if agent.exists() => agent,
            _ => self.job_path(),
        };
        let io = |err: io::Error| CoreError::io("cannot write the launchd job", err);
        fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).map_err(io)?;
        fs::write(&path, self.plist_text(binary)).map_err(io)?;
        self.launchd_start(uid, &path, action).await
    }

    /// Unloads the job, which stops the kernel (`SIGTERM`, then `SIGKILL`
    /// after `ExitTimeOut`), and waits until it is gone, so that it can be
    /// loaded again at once. A job that is not loaded is already stopped.
    pub(super) async fn launchd_stop(&self, uid: u32) -> Result<(), CoreError> {
        if self.print_job(uid).await.is_none() {
            return Ok(());
        }
        self.exec_checked(
            Action::Stop,
            Cmd::new("launchctl", ["bootout".to_owned(), self.service(uid)]),
        )
        .await?;
        let deadline = Instant::now() + self.timing.stop_grace;
        while self.print_job(uid).await.is_some() {
            if Instant::now() >= deadline {
                return Err(CoreError::Command {
                    action: Action::Stop,
                    message: format!("{} is still loaded", self.service(uid)),
                });
            }
            tokio::time::sleep(STOP_POLL).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::PermissionsExt,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::Duration,
    };

    use super::*;
    use crate::core::testing::{Recorder, Reply, UNIT, answer, core_at, free_port, scratch, serve};
    use crate::os::Os;

    /// What `launchctl print` says of a running job, shortened from a Mac.
    const RUNNING: &str = "gui/501/tabby-mihomo-x = {\n\
        \tactive count = 1\n\
        \tpath = /Users/me/x.plist\n\
        \ttype = LaunchAgent\n\
        \tstate = running\n\
        \n\
        \tprogram = /Users/me/mihomo\n\
        \tendpoints = {\n\
        \t\t\"x\" = {\n\
        \t\t\tstate = active\n\
        \t\t}\n\
        \t}\n\
        \tpid = 4242\n\
        \tlast exit code = (never exited)\n\
        }\n";

    #[test]
    fn a_job_is_read_from_its_own_keys_and_not_from_the_blocks_in_it() {
        assert_eq!(parse_print(RUNNING), Status::Active);
        let waiting = RUNNING.replace("\tstate = running", "\tstate = spawn scheduled");
        assert_eq!(parse_print(&waiting), Status::Activating);
        // `state = active` of the endpoint is not the job's.
        let failed = RUNNING
            .replace("\tstate = running", "\tstate = not running")
            .replace("(never exited)", "1");
        assert_eq!(parse_print(&failed), Status::Failed);
        let ended = failed.replace("last exit code = 1", "last exit code = 0");
        assert_eq!(parse_print(&ended), Status::Inactive);
        let never = RUNNING.replace("\tstate = running", "\tstate = not running");
        assert_eq!(parse_print(&never), Status::Inactive);
    }

    #[test]
    fn what_the_job_runs_is_escaped_as_xml() {
        assert_eq!(
            xml("a&b <c> \"d\" 'e'"),
            "a&amp;b &lt;c&gt; &quot;d&quot; &apos;e&apos;"
        );
    }

    /// launchd for user 501, loading a job that runs a kernel on `port`
    /// (after `delay`), or one that fails at once when `fails`.
    pub(crate) fn launchd(port: u16, delay: Duration, fails: bool) -> Arc<Recorder> {
        let loaded = Arc::new(Mutex::new(false));
        let served = AtomicBool::new(false);
        Recorder::new(move |cmd| {
            let verb = cmd.args.first().map(String::as_str);
            let target = cmd.args.get(1).map(String::as_str);
            match (cmd.program.as_str(), verb) {
                ("id", _) => answer(0, "501\n"),
                ("launchctl", Some("print")) if target == Some("gui/501") => answer(0, "{}\n"),
                ("launchctl", Some("print")) => {
                    if !*loaded.lock().unwrap() {
                        return answer(113, "");
                    }
                    let text = if fails {
                        RUNNING
                            .replace("\tstate = running", "\tstate = not running")
                            .replace("(never exited)", "1")
                    } else {
                        RUNNING.to_owned()
                    };
                    answer(0, &text)
                }
                ("launchctl", Some("bootstrap")) => {
                    // The stand-in controller outlives a bootout.
                    if !fails && !served.swap(true, Ordering::SeqCst) {
                        serve(port, Reply::Mihomo, delay);
                    }
                    *loaded.lock().unwrap() = true;
                    answer(0, "")
                }
                ("launchctl", Some("bootout")) => {
                    *loaded.lock().unwrap() = false;
                    answer(0, "")
                }
                _ => answer(1, ""),
            }
        })
    }

    fn mac_core(dir: &Path, port: u16, recorder: &Arc<Recorder>) -> ManagedCore {
        let mut core = core_at(dir, port, recorder, false);
        core.os = Os::Macos;
        core
    }

    /// What `launchctl` was asked, one string each.
    fn launchctl(recorder: &Recorder) -> Vec<String> {
        recorder.runs("launchctl")
    }

    #[tokio::test]
    async fn a_mac_with_a_gui_domain_runs_the_kernel_under_launchd_and_else_as_a_process() {
        let dir = scratch("launchd-mode");
        let core = mac_core(&dir, free_port(), &launchd(1, Duration::ZERO, false));
        assert_eq!(
            core.mode().await,
            crate::core::supervisor::Mode::Launchd(501)
        );
        fs::remove_dir_all(&dir).unwrap();

        // Over SSH: no GUI domain.
        let ssh = Recorder::new(|cmd| match cmd.program.as_str() {
            "id" => answer(0, "501\n"),
            _ => answer(113, ""),
        });
        let core = mac_core(&dir, free_port(), &ssh);
        assert_eq!(core.mode().await, crate::core::supervisor::Mode::Fallback);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_kernel_starts_as_a_job_of_its_own_file_and_stops_when_unloaded() {
        let dir = scratch("launchd-run");
        let port = free_port();
        let recorder = launchd(port, Duration::from_millis(100), false);
        let core = mac_core(&dir, port, &recorder);
        thread::sleep(Duration::from_millis(50));

        core.ensure_running(|_| {}).await.unwrap();

        let plist = dir.join(format!("core/launchd/{UNIT}.plist"));
        assert!(
            launchctl(&recorder).contains(&format!("bootstrap gui/501 {}", plist.display())),
            "{:?}",
            launchctl(&recorder)
        );
        let text = fs::read_to_string(&plist).unwrap();
        let binary = core.binary().unwrap();
        for wanted in [
            format!("<string>{UNIT}</string>"),
            format!("<string>{}</string>", binary.display()),
            format!(
                "<string>{}</string>",
                dir.join("core/home/config.yaml").display()
            ),
            format!("<string>{}</string>", dir.join("core/core.log").display()),
        ] {
            assert!(text.contains(&wanted), "{wanted} not in {text}");
        }
        // The secret stays off the command line.
        assert!(!text.contains(crate::core::testing::SECRET));
        let log = fs::metadata(dir.join("core/core.log")).unwrap();
        assert_eq!(log.permissions().mode() & 0o777, 0o600);
        assert!(core.alive().await);

        core.stop().await.unwrap();
        assert_eq!(
            launchctl(&recorder)
                .iter()
                .filter(|run| run.starts_with("bootout"))
                .count(),
            1
        );
        assert!(!core.alive().await);
        // Stopped twice is stopped once.
        core.stop().await.unwrap();
        assert_eq!(
            launchctl(&recorder)
                .iter()
                .filter(|run| run.starts_with("bootout"))
                .count(),
            1
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_restart_loads_the_job_anew_and_a_failing_kernel_is_unloaded_with_its_log() {
        let dir = scratch("launchd-restart");
        let port = free_port();
        let recorder = launchd(port, Duration::ZERO, false);
        let core = mac_core(&dir, port, &recorder);
        core.ensure_running(|_| {}).await.unwrap();
        core.restart().await.unwrap();
        let runs = launchctl(&recorder);
        let verbs: Vec<&str> = runs
            .iter()
            .filter_map(|run| run.split(' ').next())
            .filter(|verb| *verb != "print")
            .collect();
        assert_eq!(verbs, ["bootstrap", "bootout", "bootstrap"]);
        fs::remove_dir_all(&dir).unwrap();

        let recorder = launchd(free_port(), Duration::ZERO, true);
        let core = mac_core(&dir, free_port(), &recorder);
        fs::create_dir_all(dir.join("core")).unwrap();
        let result = core.ensure_running(|_| {}).await;
        // The job wrote to the log the kernel was given.
        assert!(
            matches!(result, Err(CoreError::StartFailed { .. })),
            "{result:?}"
        );
        assert!(
            launchctl(&recorder)
                .iter()
                .any(|run| run.starts_with("bootout"))
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_login_start_is_a_launch_agent_that_takes_over_the_running_job() {
        let dir = scratch("launchd-agent");
        let port = free_port();
        let recorder = launchd(port, Duration::ZERO, false);
        let core = mac_core(&dir, port, &recorder);
        let agent = dir.join(format!("units/{UNIT}.plist"));
        assert!(core.supports_login_start().await);
        core.ensure_running(|_| {}).await.unwrap();

        core.set_login_start(true).await.unwrap();

        assert!(core.login_start_enabled());
        let job = dir.join(format!("core/launchd/{UNIT}.plist"));
        assert_eq!(
            fs::read_to_string(&agent).unwrap(),
            fs::read_to_string(&job).unwrap()
        );
        let loads: Vec<String> = launchctl(&recorder)
            .into_iter()
            .filter(|run| !run.starts_with("print"))
            .collect();
        assert_eq!(
            loads,
            [
                format!("bootstrap gui/501 {}", job.display()),
                format!("bootout gui/501/{UNIT}"),
                format!("bootstrap gui/501 {}", agent.display()),
            ]
        );
        // Started again later, it is the agent that is loaded.
        core.restart().await.unwrap();
        assert_eq!(
            launchctl(&recorder).last().unwrap(),
            &format!("bootstrap gui/501 {}", agent.display())
        );

        // Off: the file goes, the kernel stays.
        let before = launchctl(&recorder).len();
        core.set_login_start(false).await.unwrap();
        assert!(!agent.exists() && !core.login_start_enabled());
        assert!(
            launchctl(&recorder)[before..]
                .iter()
                .all(|run| !run.starts_with("bootout"))
        );
        assert!(core.alive().await);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn turned_on_while_the_kernel_is_stopped_it_only_writes_the_agent() {
        let dir = scratch("launchd-agent-off");
        let recorder = launchd(free_port(), Duration::ZERO, false);
        let core = mac_core(&dir, free_port(), &recorder);

        core.set_login_start(true).await.unwrap();

        assert!(dir.join(format!("units/{UNIT}.plist")).exists());
        assert!(dir.join("core/home/config.yaml").exists());
        assert!(
            launchctl(&recorder)
                .iter()
                .all(|run| run.starts_with("print"))
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
