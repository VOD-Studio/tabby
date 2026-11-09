//! How the managed kernel is set up (plan §7.3): what tabby writes into the
//! config file it runs, over whatever the subscription has. Kept in
//! `settings.toml`, so that what is changed in the settings page outlives
//! the kernel.

use std::{collections::BTreeMap, fs, io, path::Path};

use serde::{Deserialize, Serialize};

use super::files::write_private;

/// The kernel's log levels.
const LOG_LEVELS: [&str; 5] = ["silent", "error", "warning", "info", "debug"];

/// The modes the kernel routes in.
const MODES: [&str; 3] = ["rule", "global", "direct"];

/// The TUN stacks the official kernel has (`FlClash`'s `mips` is its own).
const STACKS: [&str; 3] = ["system", "gvisor", "mixed"];

/// The keys of `[dns]` a subscription's own can be overridden in: what
/// `FlClash` lets a profile override, nothing that names the groups or rule
/// sets of a subscription, which change with every update.
pub const DNS_KEYS: [&str; 11] = [
    "listen",
    "ipv6",
    "prefer-h3",
    "enhanced-mode",
    "fake-ip-filter",
    "fake-ip-filter-mode",
    "default-nameserver",
    "nameserver",
    "fallback",
    "proxy-server-nameserver",
    "direct-nameserver",
];

/// What a `[dns]` key holds, and so how the settings page offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsKind {
    /// `true` or `false`.
    Switch,
    /// One of these words.
    Choice(&'static [&'static str]),
    /// One string.
    Text,
    /// A list of strings.
    List,
}

impl DnsKind {
    /// The kind of `key`, one of [`DNS_KEYS`].
    pub fn of(key: &str) -> Self {
        match key {
            "ipv6" | "prefer-h3" => Self::Switch,
            "enhanced-mode" => Self::Choice(&["fake-ip", "redir-host"]),
            "fake-ip-filter-mode" => Self::Choice(&["blacklist", "whitelist", "rule"]),
            "listen" => Self::Text,
            _ => Self::List,
        }
    }
}

/// The contents of `settings.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct CoreSettings {
    pub mixed_port: u16,
    pub allow_lan: bool,
    pub ipv6: bool,
    pub log_level: String,
    /// The kernel's spelling of the mode.
    pub mode: String,
    pub tun: TunSettings,
    /// Keys to set in the subscription's `dns` section, over its own.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub dns: BTreeMap<String, toml::Value>,
    /// How subscriptions are downloaded.
    pub subscription_download: DownloadSettings,
}

/// The way a subscription is downloaded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Route {
    /// From this machine, past any proxy.
    #[default]
    Direct,
    /// Through the managed kernel's mixed port.
    Core,
    /// Through the proxy the user gave.
    Proxy,
}

/// The `[subscription-download]` table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct DownloadSettings {
    pub via: Route,
    /// The proxy of [`Route::Proxy`]; kept while another way is chosen.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub proxy: String,
}

/// `text` as the address of a proxy a subscription can be downloaded
/// through: `http`, `https`, `socks5` or `socks5h`, with a host.
pub fn proxy_url(text: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(text).ok()?;
    let scheme = matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h");
    (scheme && url.host_str().is_some_and(|host| !host.is_empty())).then_some(url)
}

/// The `tun` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct TunSettings {
    pub enable: bool,
    pub stack: String,
    pub auto_route: bool,
    pub auto_detect_interface: bool,
    pub dns_hijack: Vec<String>,
    pub mtu: u32,
}

impl Default for TunSettings {
    fn default() -> Self {
        Self {
            enable: false,
            // The official kernel has no `mips`, which FlClash defaults to.
            stack: "mixed".to_owned(),
            auto_route: true,
            auto_detect_interface: true,
            dns_hijack: vec!["any:53".to_owned()],
            mtu: 9000,
        }
    }
}

impl Default for CoreSettings {
    fn default() -> Self {
        Self {
            mixed_port: 7890,
            allow_lan: false,
            ipv6: false,
            log_level: "info".to_owned(),
            mode: "rule".to_owned(),
            tun: TunSettings::default(),
            dns: BTreeMap::new(),
            subscription_download: DownloadSettings::default(),
        }
    }
}

/// What of the settings a change made on the settings page (or `m`) sets,
/// to be written down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Persist {
    Mode(String),
    AllowLan(bool),
    LogLevel(String),
    Ipv6(bool),
    MixedPort(u16),
    Tun(bool),
    SubscriptionDownload(DownloadSettings),
}

impl CoreSettings {
    /// Reads `path`; no file is the defaults. A file that does not read, or
    /// holds what the kernel would not take, is an error that names the
    /// file and what is wrong: it is the user's.
    pub fn load(path: &Path) -> io::Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err),
        };
        let invalid = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
        let settings: Self = toml::from_str(&text).map_err(|err| invalid(err.to_string()))?;
        settings.validate().map_err(invalid)?;
        Ok(settings)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
        write_private(path, &text)
    }

    /// Sets what `setting` says.
    pub fn set(&mut self, setting: Persist) {
        match setting {
            Persist::Mode(mode) => self.mode = mode,
            Persist::AllowLan(on) => self.allow_lan = on,
            Persist::LogLevel(level) => self.log_level = level,
            Persist::Ipv6(on) => self.ipv6 = on,
            Persist::MixedPort(port) => self.mixed_port = port,
            Persist::Tun(on) => self.tun.enable = on,
            Persist::SubscriptionDownload(download) => self.subscription_download = download,
        }
    }

    /// Sets `key` of `[dns]`, or lets the subscription's own value stand
    /// again when `value` is `None`.
    pub fn set_dns(&mut self, key: &str, value: Option<toml::Value>) {
        match value {
            Some(value) => {
                self.dns.insert(key.to_owned(), value);
            }
            None => {
                self.dns.remove(key);
            }
        }
    }

    fn validate(&self) -> Result<(), String> {
        let one_of = |key: &str, value: &str, allowed: &[&str]| {
            if allowed.contains(&value) {
                Ok(())
            } else {
                Err(format!(
                    "`{key}` is `{value}`, not one of {}",
                    allowed.join(", ")
                ))
            }
        };
        one_of("log-level", &self.log_level, &LOG_LEVELS)?;
        one_of("mode", &self.mode, &MODES)?;
        one_of("tun.stack", &self.tun.stack, &STACKS)?;
        let download = &self.subscription_download;
        if !download.proxy.is_empty() && proxy_url(&download.proxy).is_none() {
            return Err(format!(
                "`subscription-download.proxy` is `{}`, not an http, https or socks5 address",
                download.proxy
            ));
        }
        if download.via == Route::Proxy && download.proxy.is_empty() {
            return Err(
                "`subscription-download.via` is `proxy`, but there is no `proxy` to go through"
                    .to_owned(),
            );
        }
        for (key, value) in &self.dns {
            if !DNS_KEYS.contains(&key.as_str()) {
                return Err(format!(
                    "`[dns]` has no key `{key}`; the keys it takes are {}",
                    DNS_KEYS.join(", ")
                ));
            }
            let text = |value: &toml::Value| value.as_str().is_some();
            let fits = match value {
                toml::Value::String(_) | toml::Value::Boolean(_) => true,
                toml::Value::Array(items) => items.iter().all(text),
                _ => false,
            };
            if !fits {
                return Err(format!(
                    "`dns.{key}` must be a string, a boolean or a list of strings"
                ));
            }
        }
        Ok(())
    }
}

/// `value` as JSON, for the config file.
pub fn to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(text) => serde_json::Value::String(text.clone()),
        toml::Value::Boolean(on) => serde_json::Value::Bool(*on),
        toml::Value::Array(items) => serde_json::Value::Array(items.iter().map(to_json).collect()),
        // Nothing else passes `validate`.
        other => serde_json::Value::String(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tabby-{}-settings-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("settings.toml")
    }

    #[test]
    fn no_file_is_the_defaults_and_what_is_set_comes_back() {
        let path = scratch("round");
        let mut settings = CoreSettings::load(&path).unwrap();
        assert_eq!(settings, CoreSettings::default());
        assert_eq!(
            (
                settings.mixed_port,
                settings.log_level.as_str(),
                settings.tun.mtu
            ),
            (7890, "info", 9000)
        );

        for change in [
            Persist::MixedPort(17890),
            Persist::AllowLan(true),
            Persist::Mode("global".to_owned()),
            Persist::LogLevel("debug".to_owned()),
            Persist::Ipv6(true),
            Persist::Tun(true),
            Persist::SubscriptionDownload(DownloadSettings {
                via: Route::Proxy,
                proxy: "socks5://127.0.0.1:1080".to_owned(),
            }),
        ] {
            settings.set(change);
        }
        settings.dns.insert(
            "nameserver".to_owned(),
            toml::Value::Array(vec![toml::Value::String(
                "https://1.1.1.1/dns-query".to_owned(),
            )]),
        );
        settings.save(&path).unwrap();

        assert_eq!(CoreSettings::load(&path).unwrap(), settings);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_file_with_only_some_keys_keeps_the_defaults_of_the_rest() {
        let path = scratch("partial");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "mixed-port = 1234\n[tun]\nmtu = 1400\n").unwrap();
        let settings = CoreSettings::load(&path).unwrap();
        assert_eq!(settings.mixed_port, 1234);
        assert_eq!(settings.tun.mtu, 1400);
        assert!(settings.tun.auto_route && settings.log_level == "info");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_file_the_kernel_would_not_take_is_refused_saying_what_is_wrong() {
        let path = scratch("bad");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        for (text, expected) in [
            ("mode = \"script\"", "`mode` is `script`"),
            ("log-level = \"loud\"", "`log-level` is `loud`"),
            ("[tun]\nstack = \"mips\"", "`tun.stack` is `mips`"),
            ("[dns]\nproxies = []", "no key `proxies`"),
            ("[dns]\nnameserver = [1]", "`dns.nameserver` must be"),
            ("[dns]\nlisten = 53", "`dns.listen` must be"),
            ("mixed-port = 99999", "99999"),
            ("typo = 1", "typo"),
            ("mixed-port = ", "expected"),
            (
                "[subscription-download]\nvia = \"proxy\"",
                "no `proxy` to go through",
            ),
            (
                "[subscription-download]\nproxy = \"127.0.0.1:7890\"",
                "not an http, https or socks5 address",
            ),
            ("[subscription-download]\nvia = \"tor\"", "tor"),
        ] {
            fs::write(&path, text).unwrap();
            let err = CoreSettings::load(&path).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{text}");
            assert!(err.to_string().contains(expected), "{text}: {err}");
        }
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
