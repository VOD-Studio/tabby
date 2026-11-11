//! What TUN needs of the kernel (plan §8): permission to manage the
//! network, which a file capability on the binary gives without a process
//! that stays root. macOS has no file capabilities: the kernel is made
//! setuid root instead (plan `2026-11-11-macos.md` §6).

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
};

use super::{
    Action, CoreError, ManagedCore, Persist,
    runner::{Cmd, Runner},
    supervisor::Mode,
};
use crate::os::Os;

/// The capabilities the kernel is given: to set up the device and routes,
/// and to bind the ports below 1024 a DNS listener may want.
const CAPABILITIES: &str = "cap_net_admin,cap_net_bind_service=+ep";

/// Where `pkexec` finds `setcap`: it runs programs by their full path.
const SETCAP: &str = "/usr/bin/setcap";

/// What the graphical way of asking for permission came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Graphical {
    /// `setcap` ran.
    Done,
    /// The user turned the question down.
    Cancelled,
    /// There is nobody to ask (no `pkexec`, or no polkit agent, as on a
    /// tiling window manager): it takes a terminal.
    NeedsTerminal,
}

/// What makes the kernel setuid root on macOS, `$1` being the kernel.
const SETUID: &str = "/usr/sbin/chown root:wheel \"$1\" && /bin/chmod 4755 \"$1\"";

/// The command that gives `binary` its capabilities, through `pkexec`; on
/// macOS the one that makes it setuid root, through the dialog that asks
/// for an administrator's password. The path reaches the script as an
/// argument, quoted by `osascript`, never as part of its text.
fn graphical_command(os: Os, binary: &Path) -> Cmd {
    let path = binary.to_string_lossy().into_owned();
    if os == Os::Macos {
        return Cmd::new(
            "osascript",
            [
                "-e".to_owned(),
                "on run argv".to_owned(),
                "-e".to_owned(),
                "set kernel to quoted form of item 1 of argv".to_owned(),
                "-e".to_owned(),
                "do shell script \"/usr/sbin/chown root:wheel \" & kernel & \" && /bin/chmod 4755 \" & kernel with administrator privileges".to_owned(),
                "-e".to_owned(),
                "end run".to_owned(),
                path,
            ],
        );
    }
    Cmd::new("pkexec", [SETCAP.to_owned(), CAPABILITIES.to_owned(), path])
}

/// The same through `sudo`, which asks on the terminal.
fn terminal_command(os: Os, binary: &Path) -> Cmd {
    let path = binary.to_string_lossy().into_owned();
    if os == Os::Macos {
        return Cmd::new(
            "sudo",
            [
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                SETUID.to_owned(),
                "tabby".to_owned(),
                path,
            ],
        );
    }
    Cmd::new("sudo", ["setcap".to_owned(), CAPABILITIES.to_owned(), path])
}

/// Whether a file of owner `uid` and `mode` runs with root's rights.
fn is_setuid_root(uid: u32, mode: u32) -> bool {
    uid == 0 && mode & 0o4000 != 0
}

/// Whether `binary` (or the file it links to) runs with root's rights.
fn setuid_root(binary: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let real = fs::canonicalize(binary).unwrap_or_else(|_| binary.to_owned());
        fs::metadata(real).is_ok_and(|meta| is_setuid_root(meta.uid(), meta.mode()))
    }
    #[cfg(not(unix))]
    {
        let _ = binary;
        false
    }
}

/// Whether `binary` may set up a TUN on `os`: `getcap` names the
/// capability (Linux), or it is setuid root (macOS).
fn may_manage_network(os: Os, runner: &dyn Runner, binary: &Path) -> bool {
    match os {
        Os::Linux => has_capability(runner, binary),
        Os::Macos => setuid_root(binary),
    }
}

/// Whether `getcap` says `binary` may manage the network.
fn has_capability(runner: &dyn Runner, binary: &Path) -> bool {
    let asked = runner.run(&Cmd::new("getcap", [binary.to_string_lossy().into_owned()]));
    asked.is_ok_and(|output| {
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("cap_net_admin")
    })
}

/// `text` with the `\040`-style octal escapes `/proc/self/mountinfo` writes
/// a space or a backslash in a path as, undone.
fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let octal = (bytes[at] == b'\\')
            .then(|| bytes.get(at + 1..at + 4))
            .flatten()
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u8::from_str_radix(digits, 8).ok());
        if let Some(byte) = octal {
            out.push(byte);
            at += 4;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The mount point holding `path`, when it is mounted `nosuid` (as
/// `mountinfo`, the text of `/proc/self/mountinfo`, tells), where a file
/// capability has no effect.
fn nosuid_in(mountinfo: &str, path: &Path) -> Option<PathBuf> {
    let mounts = mountinfo.lines().filter_map(|line| {
        let mut fields = line.split(' ');
        // Field 5 is the mount point, field 6 the options of this mount.
        let (Some(mount_point), Some(options)) = (fields.nth(4), fields.next()) else {
            return None;
        };
        let nosuid = options.split(',').any(|option| option == "nosuid");
        Some((PathBuf::from(unescape(mount_point)), nosuid))
    });
    refusing(mounts, path)
}

/// The same from what `mount` prints on macOS (`/dev/disk4s1 on
/// /Volumes/My Disk (apfs, local, nosuid, noowners)`), where a volume that
/// ignores owners (an external disk, by default) ignores setuid too.
fn mac_nosuid_in(mount: &str, path: &Path) -> Option<PathBuf> {
    let mounts = mount.lines().filter_map(|line| {
        let (_, rest) = line.split_once(" on ")?;
        let (mount_point, options) = rest.rsplit_once(" (")?;
        let nosuid = options
            .trim_end_matches(')')
            .split(',')
            .any(|option| matches!(option.trim(), "nosuid" | "noowners"));
        Some((PathBuf::from(mount_point), nosuid))
    });
    refusing(mounts, path)
}

/// Of `mounts` (each a mount point, and whether it refuses setuid), the
/// one holding `path`, when it refuses. The innermost mount counts; of two
/// at the same place the later one, which is on top.
fn refusing(mounts: impl Iterator<Item = (PathBuf, bool)>, path: &Path) -> Option<PathBuf> {
    let mut best: Option<(PathBuf, bool)> = None;
    for (mount_point, nosuid) in mounts {
        if !path.starts_with(&mount_point) {
            continue;
        }
        let deeper = best
            .as_ref()
            .is_none_or(|(held, _)| mount_point.components().count() >= held.components().count());
        if deeper {
            best = Some((mount_point, nosuid));
        }
    }
    best.filter(|(_, nosuid)| *nosuid).map(|(point, _)| point)
}

impl ManagedCore {
    /// Whether the kernel has the capabilities TUN needs (on macOS: is
    /// setuid root). A machine without `getcap` has none that can be seen.
    pub async fn tun_authorized(&self) -> bool {
        let Ok(binary) = self.binary() else {
            return false;
        };
        let (os, runner) = (self.os, Arc::clone(&self.runner));
        tokio::task::spawn_blocking(move || may_manage_network(os, &*runner, &binary))
            .await
            .unwrap_or(false)
    }

    /// The mount point the kernel is on when it is mounted `nosuid`: a
    /// capability set there does nothing, so asking for one is no use. On
    /// macOS `mount` says (a moment's work: it reads the kernel's table).
    pub fn nosuid_mount(&self) -> Option<PathBuf> {
        let binary = self.binary().ok()?;
        let binary = fs::canonicalize(&binary).unwrap_or(binary);
        match self.os {
            Os::Linux => nosuid_in(&fs::read_to_string(&self.mountinfo).ok()?, &binary),
            Os::Macos => {
                let output = self.runner.run(&Cmd::new("mount", [] as [&str; 0])).ok()?;
                mac_nosuid_in(&String::from_utf8_lossy(&output.stdout), &binary)
            }
        }
    }

    /// Asks for the capabilities through `pkexec`: a dialog of the desktop
    /// to answer. 126 is the user saying no, and 127 that there is no one to
    /// ask.
    ///
    /// On macOS the dialog takes a GUI session (launchd has one for the
    /// user), and saying no is the script error -128.
    pub async fn authorize_graphical(&self) -> Result<Graphical, CoreError> {
        let binary = self.binary()?;
        let failed = |message: String| CoreError::Command {
            action: Action::Authorize,
            message,
        };
        if self.os == Os::Macos && !matches!(self.mode().await, Mode::Launchd(_)) {
            return Ok(Graphical::NeedsTerminal);
        }
        let output = match self.exec(graphical_command(self.os, &binary)).await {
            Ok(output) => output,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Graphical::NeedsTerminal);
            }
            Err(err) => return Err(failed(err.to_string())),
        };
        let said = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if self.os == Os::Macos {
            return match output.status.code() {
                Some(0) => Ok(Graphical::Done),
                _ if said.contains("(-128)") => Ok(Graphical::Cancelled),
                _ => Err(failed(said)),
            };
        }
        match output.status.code() {
            Some(0) => Ok(Graphical::Done),
            Some(126) => Ok(Graphical::Cancelled),
            Some(127) => Ok(Graphical::NeedsTerminal),
            _ => Err(failed(said)),
        }
    }

    /// Asks for the capabilities through `sudo`, on the terminal, which
    /// the caller has given back. `false` when `sudo` did not go through:
    /// a wrong password, or a no.
    pub async fn authorize_terminal(&self) -> Result<bool, CoreError> {
        let binary = self.binary()?;
        let runner = Arc::clone(&self.runner);
        let failed = |message: String| CoreError::Command {
            action: Action::Authorize,
            message,
        };
        let os = self.os;
        tokio::task::spawn_blocking(move || runner.run_interactive(&terminal_command(os, &binary)))
            .await
            .map_err(|err| failed(err.to_string()))?
            .map(|status| status.success())
            .map_err(|err| failed(err.to_string()))
    }

    /// After the capabilities were set: makes sure they are there, and
    /// starts the kernel again, as they take hold when it is executed.
    pub async fn finish_authorization(&self) -> Result<(), CoreError> {
        if !self.tun_authorized().await {
            return Err(CoreError::Command {
                action: Action::Authorize,
                message: "the kernel is not authorized after it was asked".to_owned(),
            });
        }
        self.restart().await
    }

    /// Before the config is written for a start: a TUN that is on in the
    /// settings but cannot work, as the kernel lost its capabilities (it
    /// was replaced), is turned off, and [`ManagedCore::take_tun_dropped`]
    /// says so.
    pub(super) fn drop_tun_without_authority(&self) {
        if !self.settings().tun.enable {
            return;
        }
        let Ok(binary) = self.binary() else {
            return;
        };
        if !may_manage_network(self.os, &*self.runner, &binary)
            && self.persist(Persist::Tun(false)).is_ok()
        {
            self.tun_dropped.store(true, Ordering::SeqCst);
        }
    }

    /// Whether TUN was turned off for want of the capabilities since this
    /// was last asked.
    pub fn take_tun_dropped(&self) -> bool {
        self.tun_dropped.swap(false, Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::core::{
        runner::Cmd,
        testing::{Recorder, answer, core_at, free_port, scratch},
    };

    fn binary(dir: &Path) -> String {
        dir.join("mihomo").to_string_lossy().into_owned()
    }

    #[test]
    fn the_commands_ask_for_exactly_the_two_capabilities_on_the_kernel_alone() {
        let kernel = Path::new("/home/u/.local/share/tabby/core/bin/mihomo");
        assert_eq!(
            graphical_command(Os::Linux, kernel),
            Cmd::new(
                "pkexec",
                [
                    "/usr/bin/setcap",
                    "cap_net_admin,cap_net_bind_service=+ep",
                    "/home/u/.local/share/tabby/core/bin/mihomo",
                ]
            )
        );
        assert_eq!(
            terminal_command(Os::Linux, kernel),
            Cmd::new(
                "sudo",
                [
                    "setcap",
                    "cap_net_admin,cap_net_bind_service=+ep",
                    "/home/u/.local/share/tabby/core/bin/mihomo",
                ]
            )
        );
    }

    #[test]
    fn a_mount_is_nosuid_only_for_what_is_under_it_and_the_innermost_mount_decides() {
        let info = "\
24 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw\n\
31 24 8:1 / /run/media/xfy/Disk\\040One rw,nosuid,nodev,relatime shared:2 - exfat /dev/sda1 rw\n\
32 24 8:2 / /run/media/xfy/Disk rw,relatime shared:3 - ext4 /dev/sda2 rw\n\
40 31 8:3 / /run/media/xfy/Disk\\040One/exec rw,relatime - ext4 /dev/sda3 rw\n";
        let on = |path: &str| nosuid_in(info, Path::new(path));
        // Under the nosuid mount, whose name has a space in it.
        assert_eq!(
            on("/run/media/xfy/Disk One/bin/mihomo"),
            Some(PathBuf::from("/run/media/xfy/Disk One"))
        );
        // A mount nested in it that allows suid: it decides.
        assert_eq!(on("/run/media/xfy/Disk One/exec/mihomo"), None);
        // A sibling that merely starts with the same letters is another place.
        assert_eq!(on("/run/media/xfy/Disk/mihomo"), None);
        assert_eq!(on("/usr/bin/mihomo"), None);
        // Of two mounts in one place the later is the one on top.
        let stacked = "1 0 0:1 / / rw - ext4 a rw\n2 1 0:2 / /m rw,nosuid - ext4 b rw\n3 1 0:3 / /m rw - ext4 c rw\n";
        assert_eq!(nosuid_in(stacked, Path::new("/m/x")), None);
    }

    /// A machine where `getcap` says `says`, and `setcap` through
    /// `pkexec` exits with `pkexec_code`.
    fn machine(says: &'static str, pkexec_code: i32) -> Arc<Recorder> {
        Recorder::new(move |cmd| match cmd.program.as_str() {
            "getcap" => answer(0, says),
            "pkexec" => {
                let mut output = answer(pkexec_code, "");
                output.stderr = b"Error: not authorized".to_vec();
                output
            }
            _ => answer(0, ""),
        })
    }

    #[tokio::test]
    async fn the_kernel_is_authorized_when_getcap_names_the_capability() {
        let dir = scratch("priv-getcap");
        for (says, expected) in [
            ("/x/mihomo cap_net_admin,cap_net_bind_service=ep\n", true),
            ("/x/mihomo cap_net_bind_service=ep\n", false),
            ("", false),
        ] {
            let recorder = machine(says, 0);
            let core = core_at(&dir, free_port(), &recorder, true);
            assert_eq!(core.tun_authorized().await, expected, "{says:?}");
            assert_eq!(recorder.runs("getcap"), [binary(&dir)]);
        }
        // Without `getcap` nothing can be seen.
        let recorder = machine("cap_net_admin", 0).without("getcap");
        assert!(
            !core_at(&dir, free_port(), &recorder, true)
                .tun_authorized()
                .await
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn pkexec_is_tried_first_and_what_it_answers_says_what_comes_next() {
        let dir = scratch("priv-pkexec");
        for (code, expected) in [
            (0, Graphical::Done),
            (126, Graphical::Cancelled),
            (127, Graphical::NeedsTerminal),
        ] {
            let recorder = machine("", code);
            let core = core_at(&dir, free_port(), &recorder, true);
            assert_eq!(
                core.authorize_graphical().await.unwrap(),
                expected,
                "{code}"
            );
            assert_eq!(
                recorder.runs("pkexec"),
                [format!(
                    "/usr/bin/setcap cap_net_admin,cap_net_bind_service=+ep {}",
                    binary(&dir)
                )]
            );
        }
        // No `pkexec` at all is no one to ask, too.
        let recorder = machine("", 0).without("pkexec");
        let core = core_at(&dir, free_port(), &recorder, true);
        assert_eq!(
            core.authorize_graphical().await.unwrap(),
            Graphical::NeedsTerminal
        );
        // Any other failure is the failure of `setcap`, with its words.
        let recorder = machine("", 1);
        let core = core_at(&dir, free_port(), &recorder, true);
        let err = core.authorize_graphical().await.unwrap_err();
        assert!(
            matches!(&err, CoreError::Command { action: Action::Authorize, message } if message == "Error: not authorized"),
            "{err:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn sudo_is_asked_on_the_terminal_and_a_refusal_is_not_an_error() {
        let dir = scratch("priv-sudo");
        for (code, expected) in [(0, true), (1, false)] {
            let recorder = Recorder::new(move |_| answer(code, ""));
            let core = core_at(&dir, free_port(), &recorder, true);
            assert_eq!(core.authorize_terminal().await.unwrap(), expected);
            assert_eq!(
                recorder.runs("sudo"),
                [format!(
                    "setcap cap_net_admin,cap_net_bind_service=+ep {}",
                    binary(&dir)
                )]
            );
        }
        let recorder = Recorder::new(|_| answer(0, "")).without("sudo");
        let core = core_at(&dir, free_port(), &recorder, true);
        assert!(matches!(
            core.authorize_terminal().await,
            Err(CoreError::Command {
                action: Action::Authorize,
                ..
            })
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_kernel_is_started_again_only_once_it_really_has_the_capability() {
        let dir = scratch("priv-finish");
        // `setcap` ran, but the capability is not there: a mount that
        // ignores it, say. Nothing is restarted.
        let recorder = machine("", 0);
        let core = core_at(&dir, free_port(), &recorder, true);
        assert!(matches!(
            core.finish_authorization().await,
            Err(CoreError::Command {
                action: Action::Authorize,
                ..
            })
        ));
        assert_eq!(recorder.runs("systemctl"), Vec::<String>::new());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_tun_that_cannot_work_is_turned_off_before_the_kernel_starts_and_said_so_once() {
        let dir = scratch("priv-drop");
        let authorized = Arc::new(Mutex::new(true));
        let seen = Arc::clone(&authorized);
        let recorder = Recorder::new(move |cmd| match cmd.program.as_str() {
            "getcap" if *seen.lock().unwrap() => answer(0, "/x cap_net_admin=ep\n"),
            _ => answer(0, ""),
        });
        let core = core_at(&dir, free_port(), &recorder, true);
        core.persist(Persist::Tun(true)).unwrap();

        // With the capability TUN stays as it was set.
        core.write_config().unwrap();
        assert!(core.settings().tun.enable && !core.take_tun_dropped());
        assert!(
            std::fs::read_to_string(dir.join("core/home/config.yaml"))
                .unwrap()
                .contains("\"enable\":true")
        );

        // The kernel is replaced, and the capability is gone.
        *authorized.lock().unwrap() = false;
        core.write_config().unwrap();
        assert!(!core.settings().tun.enable);
        assert!(
            std::fs::read_to_string(dir.join("core/settings.toml"))
                .unwrap()
                .contains("enable = false")
        );
        assert!(
            std::fs::read_to_string(dir.join("core/home/config.yaml"))
                .unwrap()
                .contains("\"enable\":false")
        );
        assert!(core.take_tun_dropped());
        // Said once.
        assert!(!core.take_tun_dropped());
        // A TUN that was off is not a TUN that was dropped.
        core.write_config().unwrap();
        assert!(!core.take_tun_dropped());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn on_macos_the_kernel_is_made_setuid_root_with_its_path_kept_out_of_the_script() {
        let kernel =
            Path::new("/Users/u/Library/Application Support/tabby/core/bin/mihomo\"; rm -rf ~");
        let path = kernel.to_string_lossy().into_owned();
        let graphical = graphical_command(Os::Macos, kernel);
        assert_eq!(graphical.program, "osascript");
        assert_eq!(graphical.args.last(), Some(&path));
        let script: Vec<&String> = graphical.args[..graphical.args.len() - 1]
            .iter()
            .filter(|arg| *arg != "-e")
            .collect();
        assert!(
            script.iter().all(|line| !line.contains("mihomo")),
            "{script:?}"
        );
        assert!(
            script
                .iter()
                .any(|line| line.contains("with administrator privileges"))
        );
        assert_eq!(
            terminal_command(Os::Macos, kernel),
            Cmd::new(
                "sudo",
                [
                    "/bin/sh",
                    "-c",
                    "/usr/sbin/chown root:wheel \"$1\" && /bin/chmod 4755 \"$1\"",
                    "tabby",
                    &path,
                ]
            )
        );
    }

    #[test]
    fn on_macos_only_a_setuid_kernel_of_root_is_authorized() {
        assert!(is_setuid_root(0, 0o104_755));
        assert!(!is_setuid_root(0, 0o100_755));
        assert!(!is_setuid_root(501, 0o104_755));
        // The kernel of the tests is the user's own.
        let dir = scratch("priv-setuid");
        let recorder = machine("/x cap_net_admin=ep\n", 0);
        let core = core_at(&dir, free_port(), &recorder, true);
        assert!(!may_manage_network(
            Os::Macos,
            &recorder,
            &core.binary().unwrap()
        ));
        // What getcap says does not count there.
        assert_eq!(recorder.runs("getcap"), Vec::<String>::new());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_mac_volume_that_ignores_setuid_or_owners_holds_no_authorized_kernel() {
        let mount = "\
/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)\n\
devfs on /dev (devfs, local, nobrowse)\n\
/dev/disk3s5 on /System/Volumes/Data (apfs, local, journaled, nobrowse, protect, root data)\n\
/dev/disk4s1 on /Volumes/Plextor 512 (apfs, local, nodev, nosuid, journaled, noowners)\n\
/dev/disk6s1 on /Volumes/Shared (apfs, local, noowners)\n\
/dev/disk7s1 on /Volumes/Shared (on top) (apfs, local)\n";
        let on = |path: &str| mac_nosuid_in(mount, Path::new(path));
        assert_eq!(
            on("/Volumes/Plextor 512/tabby/mihomo"),
            Some(PathBuf::from("/Volumes/Plextor 512"))
        );
        assert_eq!(
            on("/Volumes/Shared/mihomo"),
            Some(PathBuf::from("/Volumes/Shared"))
        );
        assert_eq!(on("/Users/u/Library/mihomo"), None);
        assert_eq!(on("/opt/homebrew/bin/mihomo"), None);
        // A mount point with " (" in its name is still read whole.
        assert_eq!(on("/Volumes/Shared (on top)/mihomo"), None);
    }

    #[tokio::test]
    async fn on_a_mac_the_dialog_is_asked_in_a_gui_session_and_a_no_is_no_error() {
        let dir = scratch("priv-osascript");
        let mac = |code: i32, said: &'static str, gui: bool| {
            Recorder::new(move |cmd| match cmd.program.as_str() {
                "id" => answer(0, "501\n"),
                "launchctl" if gui => answer(0, ""),
                "launchctl" => answer(113, ""),
                "osascript" => {
                    let mut output = answer(code, "");
                    output.stderr = said.as_bytes().to_vec();
                    output
                }
                _ => answer(0, ""),
            })
        };
        let core_of = |recorder: &Arc<Recorder>| {
            let mut core = core_at(&dir, free_port(), recorder, false);
            core.os = Os::Macos;
            core
        };
        for (code, said, expected) in [
            (0, "", Graphical::Done),
            (
                1,
                "0:150: execution error: User canceled. (-128)\n",
                Graphical::Cancelled,
            ),
        ] {
            let recorder = mac(code, said, true);
            assert_eq!(
                core_of(&recorder).authorize_graphical().await.unwrap(),
                expected
            );
            assert_eq!(recorder.runs("osascript").len(), 1);
        }
        let recorder = mac(
            1,
            "execution error: The administrator user name or password was incorrect. (-60005)",
            true,
        );
        let err = core_of(&recorder).authorize_graphical().await.unwrap_err();
        assert!(err.to_string().contains("-60005"), "{err}");
        // Over SSH there is no one to show a dialog to: the terminal asks.
        let recorder = mac(0, "", false);
        assert_eq!(
            core_of(&recorder).authorize_graphical().await.unwrap(),
            Graphical::NeedsTerminal
        );
        assert_eq!(recorder.runs("osascript"), Vec::<String>::new());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
