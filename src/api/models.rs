use std::collections::HashMap;

use serde::Deserialize;

/// Which kernel is behind the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreKind {
    Mihomo,
    Clash,
}

/// `GET /version`. Only mihomo reports `meta: true`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Version {
    pub version: String,
    pub meta: bool,
}

impl Version {
    pub fn kind(&self) -> CoreKind {
        if self.meta {
            CoreKind::Mihomo
        } else {
            CoreKind::Clash
        }
    }
}

/// `GET /configs`: the subset of runtime settings tabby shows.
///
/// Fields mihomo adds on top of Clash are `Option` or defaulted.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Configs {
    pub port: u16,
    pub socks_port: u16,
    pub mixed_port: u16,
    pub allow_lan: bool,
    pub mode: String,
    pub log_level: String,
    pub ipv6: bool,
    /// mihomo only.
    pub tun: Option<Tun>,
}

impl Configs {
    /// The proxy mode, or `None` for a value tabby does not know.
    pub fn mode(&self) -> Option<Mode> {
        Mode::parse(&self.mode)
    }
}

/// How the kernel routes traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Rule,
    Global,
    Direct,
}

impl Mode {
    /// In the order `m` cycles through them.
    pub const ALL: [Self; 3] = [Self::Rule, Self::Global, Self::Direct];

    /// Reads the kernel's spelling. Case does not matter: some Clash
    /// versions report `Rule`. Anything else (the old `script`) is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| text.eq_ignore_ascii_case(mode.name()))
    }

    /// The spelling the API takes.
    pub fn name(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Global => "global",
            Self::Direct => "direct",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Rule => "规则",
            Self::Global => "全局",
            Self::Direct => "直连",
        }
    }

    /// The mode `m` switches to from this one.
    pub fn next(self) -> Self {
        match self {
            Self::Rule => Self::Global,
            Self::Global => Self::Direct,
            Self::Direct => Self::Rule,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Tun {
    pub enable: bool,
}

/// One `/traffic` frame: current throughput in bytes per second.
///
/// The totals the kernel adds are not read here: the connections stream
/// carries them too and is the one the stat cards will use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Traffic {
    pub up: u64,
    pub down: u64,
}

/// The name of the built-in group that lists every proxy and group, in
/// configuration order.
pub const GLOBAL: &str = "GLOBAL";

/// One entry of `GET /proxies`: a node, or a group when `all` is present.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Proxy {
    pub name: String,
    /// Protocol or group type, as the kernel spells it: `Selector`, `URLTest`,
    /// `Fallback`, `LoadBalance`, `Shadowsocks`, `Trojan` …
    #[serde(rename = "type")]
    pub kind: String,
    /// Member names; only groups have them.
    pub all: Option<Vec<String>>,
    /// The member a group currently routes through.
    pub now: Option<String>,
    /// Delay test results, oldest first.
    pub history: Vec<DelayRecord>,
    pub udp: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DelayRecord {
    /// Milliseconds; 0 when the test failed or timed out (the kernel records
    /// both the same way).
    pub delay: u32,
    /// The test ran into the timeout. Only known for tests run from here:
    /// the kernel's history does not say.
    #[serde(skip)]
    timed_out: bool,
}

/// What the latest delay test of a node came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delay {
    Millis(u32),
    /// The node could not be reached. Also what a test the kernel recorded as
    /// 0 ms reads as after a reload, since the kernel does not keep why.
    Failed,
    /// The node did not answer in time.
    Timeout,
}

impl Delay {
    /// A reply of 0 ms is the kernel's way of saying the test failed.
    pub fn from_millis(millis: u32) -> Self {
        if millis == 0 {
            Self::Failed
        } else {
            Self::Millis(millis)
        }
    }
}

impl Proxy {
    /// Result of the latest delay test; `None` means never tested.
    pub fn delay(&self) -> Option<Delay> {
        self.history.last().map(|record| match record.delay {
            0 if record.timed_out => Delay::Timeout,
            0 => Delay::Failed,
            millis => Delay::Millis(millis),
        })
    }

    /// Whether the user picks this group's member (only `Selector` groups).
    pub fn is_selector(&self) -> bool {
        self.kind == "Selector"
    }
}

#[derive(Deserialize)]
struct RawProxies {
    proxies: HashMap<String, Proxy>,
}

/// `GET /proxies`, with the groups put in the order the user configured them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(from = "RawProxies")]
pub struct Proxies {
    by_name: HashMap<String, Proxy>,
    group_order: Vec<String>,
}

impl From<RawProxies> for Proxies {
    /// The API returns a map, so the order comes from `GLOBAL.all`. Groups it
    /// does not mention follow alphabetically, and `GLOBAL` itself comes last.
    fn from(raw: RawProxies) -> Self {
        let by_name = raw.proxies;
        let is_group = |name: &str| by_name.get(name).is_some_and(|proxy| proxy.all.is_some());

        let mut group_order: Vec<String> = by_name
            .get(GLOBAL)
            .and_then(|global| global.all.as_ref())
            .into_iter()
            .flatten()
            .filter(|name| *name != GLOBAL && is_group(name))
            .cloned()
            .collect();
        let mut unlisted: Vec<&str> = by_name
            .values()
            .filter(|proxy| {
                proxy.all.is_some() && proxy.name != GLOBAL && !group_order.contains(&proxy.name)
            })
            .map(|proxy| proxy.name.as_str())
            .collect();
        unlisted.sort_unstable();
        let unlisted: Vec<String> = unlisted.into_iter().map(str::to_owned).collect();
        group_order.extend(unlisted);
        if is_group(GLOBAL) {
            group_order.push(GLOBAL.to_owned());
        }

        Self {
            by_name,
            group_order,
        }
    }
}

impl Proxies {
    /// Groups in display order.
    pub fn groups(&self) -> impl Iterator<Item = &Proxy> {
        self.group_order
            .iter()
            .filter_map(|name| self.by_name.get(name))
    }

    pub fn get(&self, name: &str) -> Option<&Proxy> {
        self.by_name.get(name)
    }

    /// Records that `group` now routes through `node`, after the kernel
    /// confirmed the switch.
    pub fn set_now(&mut self, group: &str, node: &str) {
        if let Some(group) = self.by_name.get_mut(group) {
            group.now = Some(node.to_owned());
        }
    }

    /// Appends a delay test result to a node's history.
    pub fn record_delay(&mut self, name: &str, delay: Delay) {
        if let Some(proxy) = self.by_name.get_mut(name) {
            proxy.history.push(match delay {
                Delay::Millis(delay) => DelayRecord {
                    delay,
                    timed_out: false,
                },
                Delay::Failed => DelayRecord::default(),
                Delay::Timeout => DelayRecord {
                    delay: 0,
                    timed_out: true,
                },
            });
        }
    }

    /// Copies the delay history of `name` over from `earlier`, so a result
    /// measured here outlives a reply that predates it. Returns whether both
    /// sides know the node.
    pub fn carry_delay(&mut self, earlier: &Self, name: &str) -> bool {
        match (self.by_name.get_mut(name), earlier.by_name.get(name)) {
            (Some(this), Some(earlier)) => {
                this.history.clone_from(&earlier.history);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mihomo_samples() {
        let version: Version =
            serde_json::from_str(include_str!("../../tests/fixtures/mihomo_version.json")).unwrap();
        assert_eq!(version.kind(), CoreKind::Mihomo);
        assert_eq!(version.version, "v1.19.33");

        let configs: Configs =
            serde_json::from_str(include_str!("../../tests/fixtures/mihomo_configs.json")).unwrap();
        assert_eq!(configs.mixed_port, 17890);
        assert_eq!(configs.mode, "rule");
        assert_eq!(configs.log_level, "info");
        assert!(!configs.allow_lan);
        assert!(!configs.tun.unwrap().enable);
    }

    #[test]
    fn parses_clash_samples_with_missing_fields() {
        let version: Version =
            serde_json::from_str(include_str!("../../tests/fixtures/clash_version.json")).unwrap();
        assert_eq!(version.kind(), CoreKind::Clash);
        assert_eq!(version.version, "v1.18.0");

        let configs: Configs =
            serde_json::from_str(include_str!("../../tests/fixtures/clash_configs.json")).unwrap();
        assert_eq!(configs.mixed_port, 17891);
        assert_eq!(configs.mode, "rule");
        assert!(configs.tun.is_none());
    }

    #[test]
    fn parses_traffic_frames_with_extra_or_missing_fields() {
        // As sent by mihomo v1.19.33.
        let frame: Traffic =
            serde_json::from_str(r#"{"up":1536,"down":20,"upTotal":9,"downTotal":10}"#).unwrap();
        assert_eq!(frame, Traffic { up: 1536, down: 20 });
        let frame: Traffic = serde_json::from_str(r#"{"down":7}"#).unwrap();
        assert_eq!(frame, Traffic { up: 0, down: 7 });
        let frame: Traffic = serde_json::from_str("{}").unwrap();
        assert_eq!(frame, Traffic::default());
    }

    #[test]
    fn mode_ignores_case_and_rejects_unknown_values() {
        for (text, mode) in [
            ("rule", Some(Mode::Rule)),
            ("Rule", Some(Mode::Rule)),
            ("GLOBAL", Some(Mode::Global)),
            ("dIrEcT", Some(Mode::Direct)),
            ("script", None),
            ("", None),
            (" rule", None),
        ] {
            assert_eq!(Mode::parse(text), mode, "{text:?}");
        }
    }

    #[test]
    fn modes_cycle_through_all_three() {
        assert_eq!(Mode::Rule.next(), Mode::Global);
        assert_eq!(Mode::Global.next(), Mode::Direct);
        assert_eq!(Mode::Direct.next(), Mode::Rule);
    }

    fn group_names(proxies: &Proxies) -> Vec<&str> {
        proxies.groups().map(|group| group.name.as_str()).collect()
    }

    #[test]
    fn mihomo_groups_follow_configuration_order() {
        let proxies: Proxies =
            serde_json::from_str(include_str!("../../tests/fixtures/mihomo_proxies.json")).unwrap();
        // The JSON object is alphabetical; the order comes from GLOBAL.all.
        assert_eq!(
            group_names(&proxies),
            ["节点选择", "自动选择", "故障转移", "负载均衡", "GLOBAL"]
        );
        let selector = proxies.groups().next().unwrap();
        let members = selector.all.as_ref().unwrap();
        assert!(members.iter().any(|name| name == "a/b#c%d e?f"));
        assert!(members.iter().any(|name| name == "🚀 emoji 节点"));
    }

    #[test]
    fn clash_sample_parses_with_fewer_fields() {
        let proxies: Proxies =
            serde_json::from_str(include_str!("../../tests/fixtures/clash_proxies.json")).unwrap();
        assert_eq!(
            group_names(&proxies),
            ["节点选择", "自动选择", "故障转移", "负载均衡", "GLOBAL"]
        );
    }
}
