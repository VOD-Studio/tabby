//! Starting the kernel when the user logs in (plan §9.1): a unit file of
//! the systemd user manager, which `systemd-run` does not leave behind, or
//! on macOS a launch agent (plan `2026-11-11-macos.md` §4.3).

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use super::{Action, CoreError, ManagedCore, runner::Cmd, supervisor::Mode};
use crate::os::Os;

/// `arg` as one word of an `ExecStart=` line: in double quotes, with what
/// systemd reads in them (`\`, `"` and the `%` of its specifiers) escaped.
fn word(arg: &str) -> String {
    let escaped = arg
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

impl ManagedCore {
    /// The unit file of the login start, where the user manager looks for
    /// the units of the user (launchd: its launch agents); `None` for an
    /// unknown home directory.
    pub(super) fn unit_path(&self) -> Option<PathBuf> {
        let extension = match self.os {
            Os::Linux => "service",
            Os::Macos => "plist",
        };
        Some(
            self.unit_dir
                .as_ref()?
                .join(format!("{}.{extension}", self.state.unit)),
        )
    }

    /// Whether the kernel is set to start at login.
    pub fn login_start_enabled(&self) -> bool {
        self.unit_path().is_some_and(|path| path.exists())
    }

    /// Whether the login start can be had: it takes the systemd user
    /// manager, or launchd.
    pub async fn supports_login_start(&self) -> bool {
        self.unit_dir.is_some() && matches!(self.mode().await, Mode::Systemd | Mode::Launchd(_))
    }

    /// The unit: the kernel as `systemd-run` starts it (plan §5.1), wanted by
    /// the session.
    fn unit_text(&self, binary: &Path) -> String {
        let command: Vec<String> = self
            .kernel_command(binary)
            .iter()
            .map(|arg| word(arg))
            .collect();
        format!(
            "[Unit]\nDescription=mihomo, run by tabby\n\n\
             [Service]\nExecStart={}\nRestart=on-failure\nRestartSec=2\n\n\
             [Install]\nWantedBy=default.target\n",
            command.join(" ")
        )
    }

    /// Turns the login start on or off.
    ///
    /// On, the unit is written and enabled; a kernel running as the unit
    /// `systemd-run` made is stopped and started again as this one, which
    /// takes about a second. Off, it is disabled and the file is deleted; the
    /// kernel that is running is left to run.
    pub async fn set_login_start(&self, on: bool) -> Result<(), CoreError> {
        if !self.supports_login_start().await {
            return Err(CoreError::NeedsServiceManager);
        }
        let Some(path) = self.unit_path() else {
            return Err(CoreError::NeedsServiceManager);
        };
        if let Mode::Launchd(uid) = self.mode().await {
            return self.set_launch_agent(uid, on, &path).await;
        }
        let unit = self.state.unit.as_str();
        let io = |err: io::Error| CoreError::io("cannot write the unit file", err);
        let systemctl = |verb: &str| Cmd::new("systemctl", ["--user", verb, unit]);
        let reload = || Cmd::new("systemctl", ["--user", "daemon-reload"]);
        if on {
            let binary = self.binary()?;
            // What the unit starts the kernel with has to be there.
            self.write_config()?;
            fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).map_err(io)?;
            fs::write(&path, self.unit_text(&binary)).map_err(io)?;
            // A unit `systemd-run` made cannot be enabled while it is loaded
            // (`transient or generated`): it goes first, and the unit takes
            // its place.
            let running = self.status().await.running();
            if running {
                self.exec_checked(Action::Autostart, systemctl("stop"))
                    .await?;
            }
            self.exec_checked(Action::Autostart, reload()).await?;
            self.exec_checked(Action::Autostart, systemctl("enable"))
                .await?;
            if running {
                self.exec_checked(Action::Autostart, systemctl("start"))
                    .await?;
                self.wait_ready().await?;
            }
        } else {
            self.exec_checked(Action::Autostart, systemctl("disable"))
                .await?;
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(io(err)),
            }
            self.exec_checked(Action::Autostart, reload()).await?;
        }
        Ok(())
    }

    /// The login start of launchd: the job's file among the launch agents.
    /// On, a kernel running as the job tabby started by itself is started
    /// again from it (the two have one label); off, the file goes and the
    /// kernel is left to run.
    async fn set_launch_agent(&self, uid: u32, on: bool, path: &Path) -> Result<(), CoreError> {
        let io = |err: io::Error| CoreError::io("cannot write the launch agent", err);
        if on {
            let binary = self.binary()?;
            self.write_config()?;
            fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).map_err(io)?;
            fs::write(path, self.plist_text(&binary)).map_err(io)?;
            if self.status().await.running() {
                self.launchd_start(uid, path, Action::Autostart).await?;
                self.wait_ready().await?;
            }
            Ok(())
        } else {
            match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(io(err)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::core::testing::{Recorder, answer, core_at, free_port, scratch, units_of};

    #[test]
    fn a_word_keeps_what_systemd_reads_in_quotes_from_being_read() {
        assert_eq!(word("/a b/mihomo"), "\"/a b/mihomo\"");
        assert_eq!(word("50%\"x\"\\"), "\"50%%\\\"x\\\"\\\\\"");
    }

    /// A user manager that says a unit is active once it was started, as
    /// either kind of unit.
    fn manager(running: bool) -> Arc<Recorder> {
        let active = Arc::new(Mutex::new(running));
        Recorder::new(move |cmd| {
            match (cmd.program.as_str(), cmd.args.get(1).map(String::as_str)) {
                ("systemd-run", _) | ("systemctl", Some("start")) => {
                    *active.lock().unwrap() = true;
                    answer(0, "")
                }
                ("systemctl", Some("stop")) => {
                    *active.lock().unwrap() = false;
                    answer(0, "")
                }
                ("systemctl", Some("is-active")) => {
                    if *active.lock().unwrap() {
                        answer(0, "active\n")
                    } else {
                        answer(3, "inactive\n")
                    }
                }
                _ => answer(0, ""),
            }
        })
    }

    #[tokio::test]
    async fn turning_it_on_writes_a_wanted_unit_enables_it_and_hands_a_running_kernel_over() {
        let dir = scratch("login-on");
        let port = free_port();
        crate::core::testing::serve(
            port,
            crate::core::testing::Reply::Mihomo,
            std::time::Duration::ZERO,
        );
        let recorder = manager(true);
        let core = core_at(&dir, port, &recorder, true);
        let binary = dir.join("mihomo");
        assert!(core.supports_login_start().await);
        assert!(!core.login_start_enabled());

        core.set_login_start(true).await.unwrap();

        let unit = dir.join(format!("units/{}.service", crate::core::testing::UNIT));
        let text = fs::read_to_string(&unit).unwrap();
        let home = dir.join("core/home");
        assert_eq!(
            text,
            format!(
                "[Unit]\nDescription=mihomo, run by tabby\n\n\
                 [Service]\nExecStart=\"{}\" \"-d\" \"{}\" \"-f\" \"{}\"\nRestart=on-failure\nRestartSec=2\n\n\
                 [Install]\nWantedBy=default.target\n",
                binary.display(),
                home.display(),
                home.join("config.yaml").display()
            )
        );
        assert!(core.login_start_enabled());
        // The kernel is told what to run. The transient unit that was running
        // goes before the unit is loaded (systemd cannot enable a unit while
        // a transient one of the name is), then the unit takes its place.
        assert!(home.join("config.yaml").exists());
        assert_eq!(
            units_of(&recorder),
            [
                format!("is-active {}", crate::core::testing::UNIT),
                format!("stop {}", crate::core::testing::UNIT),
                "daemon-reload".to_owned(),
                format!("enable {}", crate::core::testing::UNIT),
                format!("start {}", crate::core::testing::UNIT),
            ]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn turning_it_off_disables_it_deletes_the_file_and_leaves_the_kernel_running() {
        let dir = scratch("login-off");
        let recorder = manager(true);
        let core = core_at(&dir, free_port(), &recorder, true);
        let unit = dir.join(format!("units/{}.service", crate::core::testing::UNIT));
        fs::create_dir_all(unit.parent().unwrap()).unwrap();
        fs::write(&unit, "[Service]\n").unwrap();
        assert!(core.login_start_enabled());

        core.set_login_start(false).await.unwrap();

        assert!(!unit.exists() && !core.login_start_enabled());
        assert_eq!(
            units_of(&recorder),
            [
                format!("disable {}", crate::core::testing::UNIT),
                "daemon-reload".to_owned()
            ]
        );
        // Off when it was never on is no failure.
        core.set_login_start(false).await.unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn without_a_user_manager_there_is_no_login_start_and_nothing_is_written() {
        let dir = scratch("login-fallback");
        let recorder = Recorder::new(|_| answer(1, "offline\n"));
        let core = core_at(&dir, free_port(), &recorder, false);

        assert!(!core.supports_login_start().await);
        let err = core.set_login_start(true).await.unwrap_err();

        assert!(matches!(err, CoreError::NeedsServiceManager), "{err:?}");
        assert!(!dir.join("units").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_kernel_with_a_unit_file_is_started_through_it_not_as_a_new_transient_unit() {
        let dir = scratch("login-launch");
        let port = free_port();
        // The controller comes up with the unit, not on a timer that a slow
        // machine outruns: a kernel that already answers is never started.
        let recorder = Recorder::new(move |cmd| match cmd.args.get(1).map(String::as_str) {
            Some("start") => {
                crate::core::testing::serve(
                    port,
                    crate::core::testing::Reply::Mihomo,
                    std::time::Duration::ZERO,
                );
                answer(0, "")
            }
            Some("is-active") => answer(3, "inactive\n"),
            _ => answer(0, ""),
        });
        let core = core_at(&dir, port, &recorder, true);
        let unit = dir.join(format!("units/{}.service", crate::core::testing::UNIT));
        fs::create_dir_all(unit.parent().unwrap()).unwrap();
        fs::write(&unit, "[Service]\n").unwrap();

        core.ensure_running(|_| {}).await.unwrap();

        assert_eq!(recorder.runs("systemd-run"), Vec::<String>::new());
        assert!(units_of(&recorder).contains(&format!("start {}", crate::core::testing::UNIT)));
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_setting_changed_later_reaches_the_file_the_unit_starts_the_kernel_from() {
        let dir = scratch("login-persist");
        let recorder = manager(false);
        let core = core_at(&dir, free_port(), &recorder, true);
        let config = dir.join("core/home/config.yaml");

        // No kernel was ever set up: there is no file to keep up to date.
        core.persist(crate::core::Persist::Mode("rule".to_owned()))
            .unwrap();
        assert!(!config.exists());

        core.set_login_start(true).await.unwrap();
        assert!(!fs::read_to_string(&config).unwrap().contains("global"));
        core.persist(crate::core::Persist::Mode("global".to_owned()))
            .unwrap();

        let text = fs::read_to_string(&config).unwrap();
        assert!(text.contains("global"), "{text}");
        fs::remove_dir_all(dir).unwrap();
    }
}
