//! The desktop's system proxy (plan §9.2): pointed at the kernel's Mixed
//! port while the user wants it, and put back as it was after. On macOS it
//! is a setting of every network service (plan `2026-11-11-macos.md` §5).

use std::{fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};

use super::{Action, CoreError, ManagedCore, runner::Cmd};

/// What the user's desktop takes its proxy from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Desktop {
    /// GNOME, MATE and Cinnamon: `gsettings`.
    Gnome,
    /// KDE: `kioslaverc`.
    Kde,
    /// macOS: `networksetup`.
    Macos,
}

impl Desktop {
    /// The desktop `XDG_CURRENT_DESKTOP` names; it lists several, separated
    /// by `:` (`ubuntu:GNOME`, `X-Cinnamon`). `None` for the rest (a tiling
    /// window manager has no system proxy).
    pub fn detect(current: &str) -> Option<Self> {
        current.split(':').find_map(|name| {
            let name = name.to_ascii_lowercase();
            if name.contains("kde") {
                Some(Self::Kde)
            } else if ["gnome", "mate", "cinnamon"]
                .iter()
                .any(|known| name.contains(known))
            {
                Some(Self::Gnome)
            } else {
                None
            }
        })
    }
}

/// What the system proxy is, as the settings page needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemProxy {
    /// The desktop has a system proxy tabby can set.
    pub supported: bool,
    /// tabby has pointed it at the kernel and not taken it back.
    pub enabled: bool,
}

/// What `system-proxy.toml` keeps while the system proxy is pointed at the
/// kernel: what it was before, to put it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Saved {
    desktop: Desktop,
    /// The Mixed port it points at.
    port: u16,
    /// The mode it was in, as the desktop spells it (`'none'` of
    /// `gsettings`, `0` of KDE's `ProxyType`; empty on macOS).
    mode: String,
    /// The hosts it left alone, as the desktop spells them (empty on
    /// macOS).
    ignore: String,
    /// macOS: what each network service had.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    services: Vec<Service>,
}

/// One proxy of a network service on macOS, as `networksetup -get…proxy`
/// says it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Proxy {
    enabled: bool,
    server: String,
    port: u16,
}

impl Proxy {
    /// `Enabled: Yes`, `Server: host`, `Port: 3128`, a line each.
    fn parse(text: &str) -> Self {
        let mut proxy = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "Enabled" => proxy.enabled = value == "Yes",
                "Server" => value.clone_into(&mut proxy.server),
                "Port" => proxy.port = value.parse().unwrap_or_default(),
                _ => {}
            }
        }
        proxy
    }
}

/// What a network service on macOS had before tabby set its proxies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Service {
    name: String,
    /// HTTP, HTTPS and SOCKS, in the order of [`MAC_PROXIES`].
    proxies: Vec<Proxy>,
    bypass: Vec<String>,
}

/// The proxies of a network service `networksetup` sets: what reads one,
/// what sets its server (and turns it on), and what turns it off.
const MAC_PROXIES: [[&str; 3]; 3] = [
    ["-getwebproxy", "-setwebproxy", "-setwebproxystate"],
    [
        "-getsecurewebproxy",
        "-setsecurewebproxy",
        "-setsecurewebproxystate",
    ],
    [
        "-getsocksfirewallproxy",
        "-setsocksfirewallproxy",
        "-setsocksfirewallproxystate",
    ],
];

/// The network services `networksetup -listallnetworkservices` lists, but
/// the disabled ones (`*` first). The first line says what `*` means.
fn mac_services(text: &str) -> Vec<String> {
    text.lines()
        .skip(1)
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.starts_with('*'))
        .map(str::to_owned)
        .collect()
}

/// The hosts `networksetup -getproxybypassdomains` lists; it says so in a
/// sentence when there are none.
fn mac_bypass(text: &str) -> Vec<String> {
    if text.contains("There aren't any bypass domains") {
        return vec![];
    }
    text.lines()
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Hosts that are not sent through the kernel.
const BYPASS: [&str; 7] = [
    "localhost",
    "127.0.0.0/8",
    "192.168.0.0/16",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "::1",
    "*.local",
];

const GNOME: &str = "org.gnome.system.proxy";

/// KDE's file and group.
const KDE_ARGS: [&str; 4] = ["--file", "kioslaverc", "--group", "Proxy Settings"];

fn failed(message: impl Into<String>) -> CoreError {
    CoreError::Command {
        action: Action::SystemProxy,
        message: message.into(),
    }
}

impl ManagedCore {
    fn proxy_file(&self) -> PathBuf {
        self.paths.system_proxy()
    }

    fn saved_proxy(&self) -> Option<Saved> {
        toml::from_str(&fs::read_to_string(self.proxy_file()).ok()?).ok()
    }

    /// What the system proxy is.
    pub fn system_proxy(&self) -> SystemProxy {
        SystemProxy {
            supported: self.desktop.is_some(),
            enabled: self.proxy_file().exists(),
        }
    }

    /// The port the system proxy still points at when the kernel is not
    /// running: it was left on when the kernel last stopped without tabby
    /// (plan R6).
    pub async fn system_proxy_dangling(&self) -> Option<u16> {
        let saved = self.saved_proxy()?;
        (!self.status().await.running()).then_some(saved.port)
    }

    /// `program` run, and what it printed.
    async fn capture(&self, program: &str, args: Vec<String>) -> Result<String, CoreError> {
        let output = self
            .exec(Cmd::new(program, args))
            .await
            .map_err(|err| failed(format!("{program}: {err}")))?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(failed(if message.is_empty() {
                format!("{program}: exit status {}", output.status)
            } else {
                message
            }));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// Points the system proxy at `127.0.0.1:port`, or puts it back.
    ///
    /// Turned on again while on (the port changed), it keeps what it was
    /// before the first time.
    pub async fn set_system_proxy(&self, on: bool, port: u16) -> Result<(), CoreError> {
        let Some(desktop) = self.desktop else {
            return Err(CoreError::NoDesktopProxy);
        };
        if on {
            let before = self.saved_proxy();
            let saved = match desktop {
                Desktop::Gnome => self.point_gnome(port, before).await?,
                Desktop::Kde => self.point_kde(port, before).await?,
                Desktop::Macos => self.point_macos(port, before).await?,
            };
            let text = toml::to_string(&saved).map_err(|err| failed(err.to_string()))?;
            super::files::write_private(&self.proxy_file(), &text)
                .map_err(|err| CoreError::io("cannot write system-proxy.toml", err))
        } else {
            let Some(saved) = self.saved_proxy() else {
                return Ok(());
            };
            match saved.desktop {
                Desktop::Gnome => self.put_back_gnome(&saved).await?,
                Desktop::Kde => self.put_back_kde(&saved).await?,
                Desktop::Macos => self.put_back_macos(&saved).await?,
            }
            match fs::remove_file(self.proxy_file()) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(CoreError::io("cannot remove system-proxy.toml", err)),
            }
        }
    }

    /// Puts the system proxy back as it was, if tabby had pointed it at the
    /// kernel.
    pub async fn restore_system_proxy(&self) -> Result<(), CoreError> {
        match self.saved_proxy() {
            Some(saved) => self.set_system_proxy(false, saved.port).await,
            None => Ok(()),
        }
    }

    async fn gsettings(&self, args: &[&str]) -> Result<String, CoreError> {
        self.capture(
            "gsettings",
            args.iter().map(|arg| (*arg).to_owned()).collect(),
        )
        .await
    }

    async fn point_gnome(&self, port: u16, before: Option<Saved>) -> Result<Saved, CoreError> {
        let (mode, ignore) = match before {
            Some(saved) => (saved.mode, saved.ignore),
            None => (
                self.gsettings(&["get", GNOME, "mode"]).await?,
                self.gsettings(&["get", GNOME, "ignore-hosts"]).await?,
            ),
        };
        let number = port.to_string();
        for kind in ["http", "https", "socks"] {
            let schema = format!("{GNOME}.{kind}");
            self.gsettings(&["set", &schema, "host", "'127.0.0.1'"])
                .await?;
            self.gsettings(&["set", &schema, "port", &number]).await?;
        }
        let hosts: Vec<String> = BYPASS.iter().map(|host| format!("'{host}'")).collect();
        self.gsettings(&[
            "set",
            GNOME,
            "ignore-hosts",
            &format!("[{}]", hosts.join(", ")),
        ])
        .await?;
        // Last: nothing goes to a proxy that is half set up.
        self.gsettings(&["set", GNOME, "mode", "'manual'"]).await?;
        Ok(Saved {
            desktop: Desktop::Gnome,
            port,
            mode,
            ignore,
            services: vec![],
        })
    }

    async fn put_back_gnome(&self, saved: &Saved) -> Result<(), CoreError> {
        self.gsettings(&["set", GNOME, "mode", &saved.mode]).await?;
        self.gsettings(&["set", GNOME, "ignore-hosts", &saved.ignore])
            .await?;
        Ok(())
    }

    /// `6` for the `kwriteconfig6` of Plasma 6, `5` where there is only
    /// the older one.
    async fn kde_version(&self) -> &'static str {
        match self.exec(Cmd::new("kwriteconfig6", ["--version"])).await {
            Ok(_) => "6",
            Err(_) => "5",
        }
    }

    async fn kde_read(&self, version: &str, key: &str) -> Result<String, CoreError> {
        let mut args: Vec<String> = KDE_ARGS.iter().map(|arg| (*arg).to_owned()).collect();
        args.extend(["--key".to_owned(), key.to_owned()]);
        self.capture(&format!("kreadconfig{version}"), args).await
    }

    async fn kde_write(&self, version: &str, key: &str, value: &str) -> Result<(), CoreError> {
        let mut args: Vec<String> = KDE_ARGS.iter().map(|arg| (*arg).to_owned()).collect();
        args.extend(["--key".to_owned(), key.to_owned(), value.to_owned()]);
        self.capture(&format!("kwriteconfig{version}"), args)
            .await
            .map(|_| ())
    }

    /// Tells the running KDE applications that the proxy changed. Best
    /// effort: they read it on the next start anyway.
    async fn kde_notify(&self) {
        let cmd = Cmd::new(
            "dbus-send",
            [
                "--type=signal",
                "/KIO/Scheduler",
                "org.kde.KIO.Scheduler.reparseSlaveConfiguration",
                "string:",
            ],
        );
        if let Err(err) = self.exec(cmd).await {
            tracing::warn!("cannot tell KDE that the proxy changed: {err}");
        }
    }

    async fn point_kde(&self, port: u16, before: Option<Saved>) -> Result<Saved, CoreError> {
        let version = self.kde_version().await;
        let (mode, ignore) = if let Some(saved) = before {
            (saved.mode, saved.ignore)
        } else {
            let mode = self.kde_read(version, "ProxyType").await?;
            let ignore = self.kde_read(version, "NoProxyFor").await?;
            // Not set at all is no proxy.
            (
                if mode.is_empty() {
                    "0".to_owned()
                } else {
                    mode
                },
                ignore,
            )
        };
        // KDE spells an address as `host port`, with a space.
        let address = format!("127.0.0.1 {port}");
        self.kde_write(version, "httpProxy", &format!("http://{address}"))
            .await?;
        self.kde_write(version, "httpsProxy", &format!("http://{address}"))
            .await?;
        self.kde_write(version, "socksProxy", &format!("socks://{address}"))
            .await?;
        let hosts: Vec<String> = BYPASS
            .iter()
            .map(|host| host.strip_prefix('*').unwrap_or(host).to_owned())
            .collect();
        self.kde_write(version, "NoProxyFor", &hosts.join(","))
            .await?;
        // Last, as for GNOME: `1` is a manual proxy.
        self.kde_write(version, "ProxyType", "1").await?;
        self.kde_notify().await;
        Ok(Saved {
            desktop: Desktop::Kde,
            port,
            mode,
            ignore,
            services: vec![],
        })
    }

    async fn put_back_kde(&self, saved: &Saved) -> Result<(), CoreError> {
        let version = self.kde_version().await;
        self.kde_write(version, "NoProxyFor", &saved.ignore).await?;
        self.kde_write(version, "ProxyType", &saved.mode).await?;
        self.kde_notify().await;
        Ok(())
    }

    async fn networksetup(&self, args: Vec<String>) -> Result<String, CoreError> {
        self.capture("networksetup", args).await
    }

    /// What every enabled network service has now.
    async fn read_macos(&self) -> Result<Vec<Service>, CoreError> {
        let list = self
            .networksetup(vec!["-listallnetworkservices".to_owned()])
            .await?;
        let mut services = vec![];
        for name in mac_services(&list) {
            let mut proxies = vec![];
            for [get, _, _] in MAC_PROXIES {
                let text = self
                    .networksetup(vec![get.to_owned(), name.clone()])
                    .await?;
                proxies.push(Proxy::parse(&text));
            }
            let bypass = self
                .networksetup(vec!["-getproxybypassdomains".to_owned(), name.clone()])
                .await?;
            services.push(Service {
                bypass: mac_bypass(&bypass),
                name,
                proxies,
            });
        }
        Ok(services)
    }

    /// Every enabled network service gets the kernel for its HTTP, HTTPS
    /// and SOCKS proxies, the bypass list first: setting a proxy turns it
    /// on.
    async fn point_macos(&self, port: u16, before: Option<Saved>) -> Result<Saved, CoreError> {
        let services = match before {
            Some(saved) => saved.services,
            None => self.read_macos().await?,
        };
        let number = port.to_string();
        for service in &services {
            let mut bypass = vec!["-setproxybypassdomains".to_owned(), service.name.clone()];
            bypass.extend(BYPASS.iter().map(|host| (*host).to_owned()));
            self.networksetup(bypass).await?;
            for [_, set, _] in MAC_PROXIES {
                self.networksetup(vec![
                    set.to_owned(),
                    service.name.clone(),
                    "127.0.0.1".to_owned(),
                    number.clone(),
                ])
                .await?;
            }
        }
        Ok(Saved {
            desktop: Desktop::Macos,
            port,
            mode: String::new(),
            ignore: String::new(),
            services,
        })
    }

    /// Each service gets back its bypass list, and its proxies: the server
    /// of one that was on, off for one that was off (which keeps the
    /// kernel's address, as GNOME keeps its host).
    async fn put_back_macos(&self, saved: &Saved) -> Result<(), CoreError> {
        for service in &saved.services {
            let mut bypass = vec!["-setproxybypassdomains".to_owned(), service.name.clone()];
            if service.bypass.is_empty() {
                bypass.push("Empty".to_owned());
            } else {
                bypass.extend(service.bypass.iter().cloned());
            }
            self.networksetup(bypass).await?;
            for ([_, set, state], proxy) in MAC_PROXIES.iter().zip(&service.proxies) {
                let args = if proxy.enabled {
                    vec![
                        (*set).to_owned(),
                        service.name.clone(),
                        proxy.server.clone(),
                        proxy.port.to_string(),
                    ]
                } else {
                    vec![(*state).to_owned(), service.name.clone(), "off".to_owned()]
                };
                self.networksetup(args).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        process::Output,
        sync::{Arc, Mutex},
    };

    use super::*;
    use crate::core::testing::{Recorder, answer, core_at, free_port, scratch};

    #[test]
    fn the_desktop_is_found_among_the_names_the_session_lists() {
        for (current, desktop) in [
            ("GNOME", Some(Desktop::Gnome)),
            ("ubuntu:GNOME", Some(Desktop::Gnome)),
            ("X-Cinnamon", Some(Desktop::Gnome)),
            ("MATE", Some(Desktop::Gnome)),
            ("KDE", Some(Desktop::Kde)),
            ("Hyprland", None),
            ("sway", None),
            ("", None),
        ] {
            assert_eq!(Desktop::detect(current), desktop, "{current}");
        }
    }

    /// The calls of `program`, one string each, with the arguments.
    fn calls(recorder: &Recorder, program: &str) -> Vec<String> {
        recorder
            .calls()
            .iter()
            .filter(|cmd| cmd.program == program)
            .map(|cmd| cmd.args.join(" "))
            .collect()
    }

    fn gnome_core(dir: &std::path::Path, recorder: &Arc<Recorder>) -> ManagedCore {
        let mut core = core_at(dir, free_port(), recorder, true);
        core.desktop = Some(Desktop::Gnome);
        core
    }

    /// `gsettings`: `get` says what the user had.
    fn gsettings() -> Arc<Recorder> {
        Recorder::new(|cmd| match cmd.args.as_slice() {
            [get, _, key] if get == "get" && key == "mode" => answer(0, "'auto'\n"),
            [get, _, key] if get == "get" && key == "ignore-hosts" => {
                answer(0, "['localhost', '10.1.0.0/16']\n")
            }
            _ => answer(0, ""),
        })
    }

    #[tokio::test]
    async fn gnome_gets_the_mixed_port_for_every_scheme_and_the_proxy_last() {
        let dir = scratch("proxy-gnome");
        let recorder = gsettings();
        let core = gnome_core(&dir, &recorder);
        assert_eq!(
            core.system_proxy(),
            SystemProxy {
                supported: true,
                enabled: false
            }
        );

        core.set_system_proxy(true, 7890).await.unwrap();

        assert_eq!(
            calls(&recorder, "gsettings"),
            [
                "get org.gnome.system.proxy mode",
                "get org.gnome.system.proxy ignore-hosts",
                "set org.gnome.system.proxy.http host '127.0.0.1'",
                "set org.gnome.system.proxy.http port 7890",
                "set org.gnome.system.proxy.https host '127.0.0.1'",
                "set org.gnome.system.proxy.https port 7890",
                "set org.gnome.system.proxy.socks host '127.0.0.1'",
                "set org.gnome.system.proxy.socks port 7890",
                "set org.gnome.system.proxy ignore-hosts ['localhost', '127.0.0.0/8', '192.168.0.0/16', '10.0.0.0/8', '172.16.0.0/12', '::1', '*.local']",
                "set org.gnome.system.proxy mode 'manual'",
            ]
        );
        assert!(core.system_proxy().enabled);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn turning_it_off_puts_back_what_the_user_had_and_a_second_off_does_nothing() {
        let dir = scratch("proxy-gnome-off");
        let recorder = gsettings();
        let core = gnome_core(&dir, &recorder);
        core.set_system_proxy(true, 7890).await.unwrap();
        let before = recorder.calls().len();

        core.set_system_proxy(false, 7890).await.unwrap();

        let calls: Vec<String> = recorder.calls()[before..]
            .iter()
            .map(|cmd| cmd.args.join(" "))
            .collect();
        assert_eq!(
            calls,
            [
                "set org.gnome.system.proxy mode 'auto'",
                "set org.gnome.system.proxy ignore-hosts ['localhost', '10.1.0.0/16']",
            ]
        );
        assert!(!core.system_proxy().enabled);
        core.set_system_proxy(false, 7890).await.unwrap();
        core.restore_system_proxy().await.unwrap();
        assert_eq!(recorder.calls().len(), before + 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_new_port_keeps_what_was_there_before_the_first_time() {
        let dir = scratch("proxy-port");
        let recorder = gsettings();
        let core = gnome_core(&dir, &recorder);
        core.set_system_proxy(true, 7890).await.unwrap();
        core.set_system_proxy(true, 17890).await.unwrap();
        let reads = calls(&recorder, "gsettings")
            .iter()
            .filter(|call| call.starts_with("get "))
            .count();
        // Read once, not the second time: that would be tabby's own.
        assert_eq!(reads, 2);
        assert!(
            calls(&recorder, "gsettings")
                .contains(&"set org.gnome.system.proxy.socks port 17890".to_owned())
        );

        core.set_system_proxy(false, 17890).await.unwrap();

        assert_eq!(
            calls(&recorder, "gsettings").last().unwrap(),
            "set org.gnome.system.proxy ignore-hosts ['localhost', '10.1.0.0/16']"
        );
        assert!(
            calls(&recorder, "gsettings")
                .contains(&"set org.gnome.system.proxy mode 'auto'".to_owned())
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_failing_gsettings_is_an_error_and_leaves_no_state_behind() {
        let dir = scratch("proxy-fail");
        let recorder = Recorder::new(|cmd| {
            if cmd.args.first().map(String::as_str) == Some("get") {
                let mut output = answer(1, "");
                output.stderr = b"No such schema".to_vec();
                output
            } else {
                answer(0, "")
            }
        });
        let core = gnome_core(&dir, &recorder);

        let err = core.set_system_proxy(true, 7890).await.unwrap_err();

        assert!(err.to_string().contains("No such schema"), "{err}");
        assert!(!core.system_proxy().enabled);
        // Nothing was changed on the way.
        assert!(
            calls(&recorder, "gsettings")
                .iter()
                .all(|call| call.starts_with("get "))
        );
        fs::remove_dir_all(dir).unwrap();
    }

    fn kde_script(older: bool) -> Arc<Recorder> {
        let recorder = Recorder::new(|cmd| {
            if cmd.program.starts_with("kreadconfig") {
                match cmd.args.last().map(String::as_str) {
                    Some("ProxyType") => answer(0, "2\n"),
                    Some("NoProxyFor") => answer(0, "localhost\n"),
                    _ => answer(0, ""),
                }
            } else {
                answer(0, "")
            }
        });
        if older {
            recorder.without("kwriteconfig6").without("kreadconfig6")
        } else {
            recorder
        }
    }

    #[tokio::test]
    async fn kde_gets_a_manual_proxy_in_kioslaverc_and_the_old_type_back() {
        let dir = scratch("proxy-kde");
        let recorder = kde_script(false);
        let mut core = core_at(&dir, free_port(), &recorder, true);
        core.desktop = Some(Desktop::Kde);

        core.set_system_proxy(true, 7890).await.unwrap();

        let group = "--file kioslaverc --group Proxy Settings --key";
        assert_eq!(
            calls(&recorder, "kwriteconfig6"),
            [
                "--version".to_owned(),
                format!("{group} httpProxy http://127.0.0.1 7890"),
                format!("{group} httpsProxy http://127.0.0.1 7890"),
                format!("{group} socksProxy socks://127.0.0.1 7890"),
                format!(
                    "{group} NoProxyFor localhost,127.0.0.0/8,192.168.0.0/16,10.0.0.0/8,172.16.0.0/12,::1,.local"
                ),
                format!("{group} ProxyType 1"),
            ]
        );
        assert_eq!(
            calls(&recorder, "kreadconfig6"),
            [format!("{group} ProxyType"), format!("{group} NoProxyFor")]
        );
        assert_eq!(calls(&recorder, "dbus-send").len(), 1);

        core.set_system_proxy(false, 7890).await.unwrap();

        let writes = calls(&recorder, "kwriteconfig6");
        assert_eq!(
            writes[writes.len() - 3..],
            [
                "--version".to_owned(),
                format!("{group} NoProxyFor localhost"),
                format!("{group} ProxyType 2"),
            ]
        );
        assert!(!core.system_proxy().enabled);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn without_kwriteconfig6_it_is_kwriteconfig5() {
        let dir = scratch("proxy-kde5");
        let recorder = kde_script(true);
        let mut core = core_at(&dir, free_port(), &recorder, true);
        core.desktop = Some(Desktop::Kde);

        core.set_system_proxy(true, 7890).await.unwrap();

        assert!(calls(&recorder, "kwriteconfig5").len() >= 5);
        assert_eq!(calls(&recorder, "kreadconfig5").len(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_desktop_without_a_system_proxy_is_refused_before_anything_runs() {
        let dir = scratch("proxy-none");
        let recorder = Recorder::new(|_| answer(0, ""));
        let core = core_at(&dir, free_port(), &recorder, true);

        assert!(!core.system_proxy().supported);
        let err = core.set_system_proxy(true, 7890).await.unwrap_err();

        assert!(matches!(err, CoreError::NoDesktopProxy), "{err:?}");
        assert_eq!(recorder.calls().len(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_proxy_left_on_for_a_kernel_that_is_gone_is_found() {
        let dir = scratch("proxy-dangling");
        let running = Arc::new(Mutex::new(true));
        let seen = Arc::clone(&running);
        let recorder = Recorder::new(move |cmd| {
            if cmd.program == "systemctl" {
                if *seen.lock().unwrap() {
                    answer(0, "active\n")
                } else {
                    answer(3, "inactive\n")
                }
            } else {
                answer(0, "")
            }
        });
        let core = gnome_core(&dir, &recorder);
        // Never turned on: nothing to be left behind.
        assert_eq!(core.system_proxy_dangling().await, None);
        core.set_system_proxy(true, 7890).await.unwrap();
        assert_eq!(core.system_proxy_dangling().await, None);

        *running.lock().unwrap() = false;

        assert_eq!(core.system_proxy_dangling().await, Some(7890));
        fs::remove_dir_all(dir).unwrap();
    }

    const SERVICES: &str = "An asterisk (*) denotes that a network service is disabled.\n\
        Wi-Fi\n\
        *Bluetooth PAN\n\
        USB 10/100/1000 LAN\n";

    /// `networksetup` on a Mac whose Wi-Fi goes through a proxy of the
    /// office for the web, and whose wired network has none.
    fn networksetup() -> Arc<Recorder> {
        Recorder::new(|cmd| {
            let off = "Enabled: No\nServer: \nPort: 0\nAuthenticated Proxy Enabled: 0\n";
            match (cmd.args[0].as_str(), cmd.args.get(1).map(String::as_str)) {
                ("-listallnetworkservices", _) => answer(0, SERVICES),
                ("-getwebproxy", Some("Wi-Fi")) => answer(
                    0,
                    "Enabled: Yes\nServer: proxy.corp\nPort: 3128\nAuthenticated Proxy Enabled: 0\n",
                ),
                ("-getproxybypassdomains", Some("Wi-Fi")) => answer(0, "*.corp\nlocalhost\n"),
                ("-getproxybypassdomains", Some(name)) => answer(
                    0,
                    &format!("There aren't any bypass domains set on {name}.\n"),
                ),
                (get, _) if get.starts_with("-get") => answer(0, off),
                _ => answer(0, ""),
            }
        })
    }

    fn mac_core(dir: &std::path::Path, recorder: &Arc<Recorder>) -> ManagedCore {
        let mut core = core_at(dir, free_port(), recorder, false);
        core.desktop = Some(Desktop::Macos);
        core
    }

    #[test]
    fn what_networksetup_says_is_read_as_it_lists_it() {
        assert_eq!(mac_services(SERVICES), ["Wi-Fi", "USB 10/100/1000 LAN"]);
        assert_eq!(
            Proxy::parse(
                "Enabled: Yes\nServer: 10.0.0.1\nPort: 8080\nAuthenticated Proxy Enabled: 0\n"
            ),
            Proxy {
                enabled: true,
                server: "10.0.0.1".to_owned(),
                port: 8080
            }
        );
        assert_eq!(
            Proxy::parse("Enabled: No\nServer: \nPort: 0\n"),
            Proxy::default()
        );
        assert_eq!(
            mac_bypass("There aren't any bypass domains set on Wi-Fi.\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            mac_bypass("*.local\n169.254/16\n"),
            ["*.local", "169.254/16"]
        );
    }

    #[tokio::test]
    async fn every_enabled_service_of_a_mac_gets_the_kernel_and_its_own_proxies_back() {
        let dir = scratch("proxy-macos");
        let recorder = networksetup();
        let core = mac_core(&dir, &recorder);

        core.set_system_proxy(true, 7890).await.unwrap();

        let sets: Vec<String> = calls(&recorder, "networksetup")
            .into_iter()
            .filter(|call| call.starts_with("-set"))
            .collect();
        let bypass = BYPASS.join(" ");
        assert_eq!(
            sets,
            [
                format!("-setproxybypassdomains Wi-Fi {bypass}"),
                "-setwebproxy Wi-Fi 127.0.0.1 7890".to_owned(),
                "-setsecurewebproxy Wi-Fi 127.0.0.1 7890".to_owned(),
                "-setsocksfirewallproxy Wi-Fi 127.0.0.1 7890".to_owned(),
                format!("-setproxybypassdomains USB 10/100/1000 LAN {bypass}"),
                "-setwebproxy USB 10/100/1000 LAN 127.0.0.1 7890".to_owned(),
                "-setsecurewebproxy USB 10/100/1000 LAN 127.0.0.1 7890".to_owned(),
                "-setsocksfirewallproxy USB 10/100/1000 LAN 127.0.0.1 7890".to_owned(),
            ]
        );
        // The service names are one argument each, spaces and all.
        assert!(
            recorder
                .calls()
                .iter()
                .any(|cmd| cmd.args.get(1).map(String::as_str) == Some("USB 10/100/1000 LAN"))
        );
        assert!(core.system_proxy().enabled);

        // A new port: what was there before the first time is kept.
        core.set_system_proxy(true, 7891).await.unwrap();
        let before = recorder.calls().len();
        core.set_system_proxy(false, 7891).await.unwrap();

        let back: Vec<String> = recorder.calls()[before..]
            .iter()
            .map(|cmd| cmd.args.join(" "))
            .collect();
        assert_eq!(
            back,
            [
                "-setproxybypassdomains Wi-Fi *.corp localhost",
                "-setwebproxy Wi-Fi proxy.corp 3128",
                "-setsecurewebproxystate Wi-Fi off",
                "-setsocksfirewallproxystate Wi-Fi off",
                "-setproxybypassdomains USB 10/100/1000 LAN Empty",
                "-setwebproxystate USB 10/100/1000 LAN off",
                "-setsecurewebproxystate USB 10/100/1000 LAN off",
                "-setsocksfirewallproxystate USB 10/100/1000 LAN off",
            ]
        );
        assert!(!core.system_proxy().enabled);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_mac_that_refuses_a_change_says_why_and_is_not_marked_on() {
        let dir = scratch("proxy-macos-refused");
        let recorder = Recorder::new(|cmd| match cmd.args[0].as_str() {
            "-listallnetworkservices" => answer(0, SERVICES),
            set if set.starts_with("-set") => Output {
                status: answer(14, "").status,
                stdout: vec![],
                stderr: b"** Error: authorization failed\n".to_vec(),
            },
            _ => answer(0, "Enabled: No\n"),
        });
        let core = mac_core(&dir, &recorder);

        let err = core.set_system_proxy(true, 7890).await.unwrap_err();

        assert!(err.to_string().contains("authorization failed"), "{err}");
        assert!(!core.system_proxy().enabled);
        fs::remove_dir_all(dir).unwrap();
    }
}
