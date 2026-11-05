use std::collections::{HashMap, hash_map::Entry};

use jiff::Timestamp;
use serde::{Deserialize, Deserializer};

use crate::format::natural_cmp;

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
/// A field a kernel leaves out is `None` (or empty), never 0 or `false`:
/// mihomo adds some on top of Clash, and a missing one must not read as
/// "off".
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Configs {
    /// The HTTP proxy port; 0 when it is off.
    pub port: Option<u16>,
    pub socks_port: Option<u16>,
    pub redir_port: Option<u16>,
    pub tproxy_port: Option<u16>,
    pub mixed_port: Option<u16>,
    pub allow_lan: Option<bool>,
    pub mode: String,
    /// How much the kernel itself logs, not what the log page subscribes
    /// to.
    pub log_level: String,
    pub ipv6: Option<bool>,
    /// mihomo only.
    pub tun: Option<Tun>,
}

impl Configs {
    /// The proxy mode, or `None` for a value tabby does not know.
    pub fn mode(&self) -> Option<Mode> {
        Mode::parse(&self.mode)
    }
}

/// The levels the kernel's own `log-level` takes, in the order the settings
/// page cycles through them.
pub const KERNEL_LOG_LEVELS: [&str; 5] = ["silent", "error", "warning", "info", "debug"];

/// The kernel log level after `level` (any case) in [`KERNEL_LOG_LEVELS`].
/// One tabby does not know goes to `info`, the kernels' default, as an
/// unknown mode goes to rule.
pub fn next_kernel_log_level(level: &str) -> &'static str {
    KERNEL_LOG_LEVELS
        .iter()
        .position(|known| level.eq_ignore_ascii_case(known))
        .map_or("info", |index| {
            KERNEL_LOG_LEVELS[(index + 1) % KERNEL_LOG_LEVELS.len()]
        })
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
    /// The network stack (`gvisor`, `system`, `mixed`); empty when not
    /// reported.
    pub stack: String,
    /// The interface name; empty when the kernel picks one.
    pub device: String,
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

/// One `/connections` frame, also the reply of `GET /connections`: the
/// totals since the kernel started and the connections open right now.
///
/// Only what the stat cards, the connections table and its detail popup
/// show is read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ConnectionSnapshot {
    pub upload_total: u64,
    pub download_total: u64,
    /// mihomo sends `null` rather than `[]` when nothing is open.
    #[serde(deserialize_with = "null_as_default")]
    pub connections: Vec<Connection>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Connection {
    pub id: String,
    pub metadata: Metadata,
    /// Bytes sent so far, over the connection's whole life.
    pub upload: u64,
    /// Bytes received so far, over the connection's whole life.
    pub download: u64,
    /// When the connection opened. `None` when the kernel's timestamp is
    /// missing or unreadable: one odd value must not cost the whole frame.
    #[serde(deserialize_with = "lenient_timestamp")]
    pub start: Option<Timestamp>,
    /// The route, innermost first: `[Node 1, Auto, Proxy]`.
    #[serde(deserialize_with = "null_as_default")]
    pub chains: Vec<String>,
    /// The type of the rule that matched, without its payload.
    pub rule: String,
    /// What the rule matched on (`example.com` for a domain suffix rule);
    /// empty for rules without one, such as `Match`.
    #[serde(rename = "rulePayload")]
    pub rule_payload: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Metadata {
    /// `tcp` or `udp`.
    pub network: String,
    /// The name the client asked for; empty when it connected by address.
    pub host: String,
    #[serde(rename = "destinationIP")]
    pub destination_ip: String,
    /// A string in both mihomo and original Clash, as are the other ports.
    #[serde(rename = "destinationPort")]
    pub destination_port: String,
    #[serde(rename = "sourceIP")]
    pub source_ip: String,
    #[serde(rename = "sourcePort")]
    pub source_port: String,
    /// How it came in: `HTTPS`, `Socks5`, `Tun` …
    #[serde(rename = "type")]
    pub kind: String,
    /// The local process that opened it. mihomo only, and only when it can
    /// tell; empty otherwise.
    pub process: String,
    /// Where that process's executable lives; empty along with `process`.
    #[serde(rename = "processPath")]
    pub process_path: String,
}

/// What a connection had transferred in total, as of one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    pub upload: u64,
    pub download: u64,
}

/// The totals of every connection in the last frame, by connection id: what
/// the next frame is measured against.
pub type Baseline = HashMap<String, Totals>;

/// Bytes per frame of one connection, which is bytes per second at the
/// kernel's default of one frame a second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub up: u64,
    pub down: u64,
}

/// What the stat cards keep of a [`ConnectionSnapshot`] frame: counts, not the
/// connections themselves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConnectionStats {
    pub upload_total: u64,
    pub download_total: u64,
    /// Open connections, whatever their network.
    pub active: u64,
    pub tcp: u64,
    pub udp: u64,
}

impl ConnectionSnapshot {
    pub fn stats(&self) -> ConnectionStats {
        let mut stats = ConnectionStats {
            upload_total: self.upload_total,
            download_total: self.download_total,
            ..ConnectionStats::default()
        };
        for connection in &self.connections {
            stats.active += 1;
            let network = &connection.metadata.network;
            if network.eq_ignore_ascii_case("tcp") {
                stats.tcp += 1;
            } else if network.eq_ignore_ascii_case("udp") {
                stats.udp += 1;
            }
        }
        stats
    }

    /// What each connection of this frame moved since `previous`, and the
    /// baseline for the frame after this one. Pure: the caller owns the state.
    ///
    /// The rates come in the order of `self.connections`:
    /// - in both frames: this frame's totals minus the last one's (0 when the
    ///   total shrank, which a live connection never does);
    /// - new in this frame: `None`, there is nothing to compare with.
    ///
    /// A connection that went away is in neither the rates nor the new
    /// baseline, so the baseline never outgrows the open connections.
    pub fn rates(&self, previous: &Baseline) -> (Vec<Option<Rate>>, Baseline) {
        let mut baseline = Baseline::with_capacity(self.connections.len());
        let rates = self
            .connections
            .iter()
            .map(|connection| {
                baseline.insert(
                    connection.id.clone(),
                    Totals {
                        upload: connection.upload,
                        download: connection.download,
                    },
                );
                previous.get(&connection.id).map(|before| Rate {
                    up: connection.upload.saturating_sub(before.upload),
                    down: connection.download.saturating_sub(before.download),
                })
            })
            .collect();
        (rates, baseline)
    }
}

/// One `/memory` frame. mihomo only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Memory {
    /// Bytes the kernel process uses; the first frame after connecting is 0.
    pub inuse: u64,
}

/// How much the kernel logs: the `level` the `/logs` stream is subscribed at.
/// A level also lets through every level after it here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    #[default]
    Info,
    Warning,
    Error,
}

impl LogLevel {
    /// In the order `l` cycles through them.
    pub const ALL: [Self; 4] = [Self::Debug, Self::Info, Self::Warning, Self::Error];

    /// Reads the kernel's spelling, ignoring case. `silent` and anything
    /// newer are `None`.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|level| text.eq_ignore_ascii_case(level.name()))
    }

    /// The spelling both kernels use, in the `level` parameter and the
    /// `type` of a line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    /// The level `l` switches to from this one.
    pub fn next(self) -> Self {
        match self {
            Self::Debug => Self::Info,
            Self::Info => Self::Warning,
            Self::Warning => Self::Error,
            Self::Error => Self::Debug,
        }
    }
}

/// The `type` of a log line: a level tabby knows, or the kernel's own word
/// for one it does not, kept so the line is still shown.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub enum LogKind {
    Level(LogLevel),
    Other(String),
}

impl Default for LogKind {
    fn default() -> Self {
        Self::Other(String::new())
    }
}

impl From<String> for LogKind {
    fn from(text: String) -> Self {
        LogLevel::parse(&text).map_or(Self::Other(text), Self::Level)
    }
}

/// One `/logs` frame, in the plain format mihomo and original Clash share:
/// `{"type":"info","payload":"…"}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Log {
    #[serde(rename = "type")]
    pub kind: LogKind,
    pub payload: String,
}

/// `#[serde(default)]` covers a missing field but not an explicit `null`;
/// this reads `null` as the default too.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// An RFC 3339 timestamp, or `None` for anything else, whatever it is.
fn lenient_timestamp<'de, D>(deserializer: D) -> Result<Option<Timestamp>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_str().and_then(|text| text.parse().ok()))
}

/// When a provider was last loaded: like [`lenient_timestamp`], and the
/// zero time `0001-01-01T00:00:00Z` the kernel reports for one that never
/// was is `None` too. Anything before 1970 counts as that.
fn provider_time<'de, D>(deserializer: D) -> Result<Option<Timestamp>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(lenient_timestamp(deserializer)?.filter(|time| *time > Timestamp::UNIX_EPOCH))
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
    /// The proxy provider the node comes from, for the nodes `GET /proxies`
    /// leaves out (see [`Proxies::add_provider_nodes`]). The kernel tests
    /// those through the provider: `/proxies/{name}/delay` does not know
    /// them.
    #[serde(skip)]
    pub provider: Option<String>,
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

    /// Adds the nodes of `providers`: `GET /proxies` leaves them out,
    /// although the groups that use a provider list them as members. A name
    /// `/proxies` has already keeps its entry there.
    pub fn add_provider_nodes(&mut self, providers: Vec<ProxyProvider>) {
        for provider in providers {
            for mut node in provider.proxies {
                if let Entry::Vacant(slot) = self.by_name.entry(node.name.clone()) {
                    node.provider = Some(provider.name.clone());
                    slot.insert(node);
                }
            }
        }
    }
}

/// `GET /rules`: the rules in the order the kernel matches them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RuleList {
    #[serde(deserialize_with = "null_as_default")]
    pub rules: Vec<Rule>,
}

/// One rule. Original Clash sends only these three fields; mihomo adds
/// `index`, `size` and `extra` (hit counts, `disabled`), which tabby does not
/// show and so does not read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Rule {
    /// The kernel's spelling: `DomainSuffix`, `RuleSet`, `Match`.
    #[serde(rename = "type")]
    pub kind: String,
    /// What it matches on; empty for `Match`.
    pub payload: String,
    /// The proxy or group it sends traffic to.
    pub proxy: String,
}

/// What both kinds of provider share: where they load from, under which
/// name.
pub trait Provider {
    fn vehicle(&self) -> &str;
    fn name_mut(&mut self) -> &mut String;
}

/// `GET /providers/proxies` and `GET /providers/rules`: providers by name.
#[derive(Debug, Deserialize)]
#[serde(bound = "T: Deserialize<'de>")]
pub struct ProviderList<T> {
    #[serde(default, deserialize_with = "null_as_default")]
    providers: HashMap<String, T>,
}

impl<T: Provider> ProviderList<T> {
    /// The providers that load from a source of their own (`HTTP`, `File`,
    /// `Inline`), by name. The rest are `Compatible`: the proxies written
    /// straight into the configuration (`default`) and each proxy group,
    /// which the kernel lists here too but which cannot be updated.
    pub fn shown(self) -> Vec<T> {
        let mut shown: Vec<(String, T)> = self
            .providers
            .into_iter()
            .filter(|(_, provider)| matches!(provider.vehicle(), "HTTP" | "File" | "Inline"))
            .map(|(name, mut provider)| {
                // The key is the name the endpoints take; the field echoes it.
                provider.name_mut().clone_from(&name);
                (name.to_lowercase(), provider)
            })
            .collect();
        shown.sort_by(|(a, _), (b, _)| natural_cmp(a, b));
        shown.into_iter().map(|(_, provider)| provider).collect()
    }
}

/// One entry of `GET /providers/proxies`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProxyProvider {
    pub name: String,
    /// `HTTP`, `File`, `Inline` or `Compatible`.
    pub vehicle_type: String,
    /// `None` when missing, unreadable, or never loaded.
    #[serde(deserialize_with = "provider_time")]
    pub updated_at: Option<Timestamp>,
    #[serde(deserialize_with = "null_as_default")]
    pub proxies: Vec<Proxy>,
    /// From the `subscription-userinfo` header of an HTTP provider; mihomo
    /// only, and only when the server sends one.
    pub subscription_info: Option<Subscription>,
}

impl Provider for ProxyProvider {
    fn vehicle(&self) -> &str {
        &self.vehicle_type
    }

    fn name_mut(&mut self) -> &mut String {
        &mut self.name
    }
}

/// Traffic and expiry of a subscription, as its server reports them. Zero
/// means not reported (`total`, `expire`). Signed like the kernel's own
/// fields, so an odd value from a server cannot fail the whole reply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Subscription {
    pub upload: i64,
    pub download: i64,
    pub total: i64,
    /// Unix seconds.
    pub expire: i64,
}

/// How many nodes of a proxy provider answered their latest test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Health {
    /// Nodes whose latest test came back with a delay.
    pub alive: usize,
    pub total: usize,
    /// Whether any node has been tested at all.
    pub tested: bool,
}

impl ProxyProvider {
    pub fn health(&self) -> Health {
        let latest = || self.proxies.iter().map(|proxy| proxy.history.last());
        Health {
            alive: latest()
                .filter(|record| record.is_some_and(|record| record.delay > 0))
                .count(),
            total: self.proxies.len(),
            tested: latest().any(|record| record.is_some()),
        }
    }

    /// The subscription, unless its server reported nothing in it.
    pub fn subscription(&self) -> Option<Subscription> {
        self.subscription_info
            .filter(|info| *info != Subscription::default())
    }
}

/// One entry of `GET /providers/rules` (mihomo only). Its `format` is not
/// read: the page does not show it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RuleProvider {
    pub name: String,
    /// `HTTP`, `File` or `Inline`.
    pub vehicle_type: String,
    /// `Domain`, `IPCIDR` or `Classical`.
    pub behavior: String,
    pub rule_count: u64,
    #[serde(deserialize_with = "provider_time")]
    pub updated_at: Option<Timestamp>,
}

impl Provider for RuleProvider {
    fn vehicle(&self) -> &str {
        &self.vehicle_type
    }

    fn name_mut(&mut self) -> &mut String {
        &mut self.name
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
        let ports = [
            configs.port,
            configs.socks_port,
            configs.redir_port,
            configs.tproxy_port,
            configs.mixed_port,
        ];
        assert_eq!(ports, [Some(0), Some(0), Some(0), Some(0), Some(17890)]);
        assert_eq!(configs.mode, "rule");
        assert_eq!(configs.log_level, "info");
        assert_eq!(configs.allow_lan, Some(false));
        assert_eq!(configs.ipv6, Some(true));
        let tun = configs.tun.unwrap();
        assert!(!tun.enable);
        assert_eq!((tun.stack.as_str(), tun.device.as_str()), ("Mips", ""));
    }

    #[test]
    fn parses_clash_samples_with_missing_fields() {
        let version: Version =
            serde_json::from_str(include_str!("../../tests/fixtures/clash_version.json")).unwrap();
        assert_eq!(version.kind(), CoreKind::Clash);
        assert_eq!(version.version, "v1.18.0");

        let configs: Configs =
            serde_json::from_str(include_str!("../../tests/fixtures/clash_configs.json")).unwrap();
        assert_eq!(configs.mixed_port, Some(17891));
        assert_eq!(configs.port, Some(0));
        assert_eq!(configs.mode, "rule");
        assert_eq!(configs.ipv6, Some(false));
        assert!(configs.tun.is_none());

        // A field left out reads as unknown, not as off or port 0.
        let configs: Configs = serde_json::from_str(r#"{"mode":"rule"}"#).unwrap();
        let ports = [
            configs.port,
            configs.socks_port,
            configs.redir_port,
            configs.tproxy_port,
            configs.mixed_port,
        ];
        assert_eq!(ports, [None; 5]);
        assert_eq!((configs.allow_lan, configs.ipv6), (None, None));
        assert!(configs.log_level.is_empty() && configs.tun.is_none());
    }

    #[test]
    fn kernel_log_levels_cycle_from_silent_to_debug() {
        let mut level = "silent";
        let mut seen = vec![];
        for _ in 0..5 {
            level = next_kernel_log_level(level);
            seen.push(level);
        }
        assert_eq!(seen, ["error", "warning", "info", "debug", "silent"]);
        assert_eq!(next_kernel_log_level("WARNING"), "info");
        assert_eq!(next_kernel_log_level("verbose"), "info");
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
    fn counts_the_connections_of_a_mihomo_snapshot_by_network() {
        let snapshot: ConnectionSnapshot =
            serde_json::from_str(include_str!("../../tests/fixtures/mihomo_connections.json"))
                .unwrap();
        assert_eq!(
            snapshot.stats(),
            ConnectionStats {
                upload_total: 493_862_931,
                download_total: 1_970_947_638,
                active: 5,
                tcp: 4,
                udp: 1,
            }
        );
    }

    #[test]
    fn a_snapshot_without_connections_counts_none() {
        // mihomo sends `null` when nothing is open; `[]` and no field at all
        // mean the same.
        for json in [
            r#"{"uploadTotal":5,"downloadTotal":7,"connections":null}"#,
            r#"{"uploadTotal":5,"downloadTotal":7,"connections":[]}"#,
            r#"{"uploadTotal":5,"downloadTotal":7}"#,
        ] {
            let stats = serde_json::from_str::<ConnectionSnapshot>(json)
                .unwrap()
                .stats();
            assert_eq!(
                stats,
                ConnectionStats {
                    upload_total: 5,
                    download_total: 7,
                    ..ConnectionStats::default()
                },
                "{json}"
            );
        }
    }

    #[test]
    fn connections_without_a_known_network_count_as_active_only() {
        let json = r#"{"connections":[{"metadata":{"network":"TCP"}},{"metadata":{}},{}]}"#;
        let stats = serde_json::from_str::<ConnectionSnapshot>(json)
            .unwrap()
            .stats();
        assert_eq!((stats.active, stats.tcp, stats.udp), (3, 1, 0));
    }

    fn mihomo_snapshot() -> ConnectionSnapshot {
        serde_json::from_str(include_str!("../../tests/fixtures/mihomo_connections.json")).unwrap()
    }

    #[test]
    fn reads_what_the_table_and_detail_show_from_a_mihomo_connection() {
        let snapshot = mihomo_snapshot();
        let first = &snapshot.connections[0];
        assert_eq!(first.id, "00000000-0000-4000-8000-000000000001");
        assert_eq!(first.metadata.host, "www.example.com");
        assert_eq!(first.metadata.destination_ip, "203.0.113.10");
        assert_eq!(first.metadata.destination_port, "443");
        assert_eq!(first.metadata.source_ip, "127.0.0.1");
        assert_eq!(first.metadata.source_port, "34238");
        assert_eq!(first.metadata.kind, "HTTPS");
        assert_eq!(first.metadata.process, "");
        assert_eq!(first.metadata.process_path, "");
        assert_eq!((first.upload, first.download), (296_708, 27_027));
        assert_eq!(first.chains, ["Node 1", "Auto", "Proxy"]);
        assert_eq!(first.rule, "RuleSet");
        assert_eq!(first.rule_payload, "example.com");
        // Nanoseconds and an offset: 16:30 at +08:00 is 08:30 UTC.
        assert_eq!(
            first.start,
            Some("2026-11-05T08:30:00Z".parse::<Timestamp>().unwrap())
        );
        // No host: the table falls back to the address. `Match` has no
        // payload.
        let udp = &snapshot.connections[3];
        assert_eq!(udp.metadata.host, "");
        assert_eq!(udp.metadata.destination_ip, "198.51.100.53");
        assert_eq!(udp.metadata.destination_port, "53");
        assert_eq!(udp.metadata.kind, "Socks5");
        assert_eq!(udp.chains, ["Node 4", "Auto", "Proxy"]);
        assert_eq!(
            (udp.rule.as_str(), udp.rule_payload.as_str()),
            ("Match", "")
        );
    }

    #[test]
    fn an_unreadable_start_costs_that_value_only() {
        // Original Clash has no `process`; the second connection's `start`
        // is not a timestamp at all, the third one's is not even a string.
        let json = r#"{"connections":[
            {"id":"a","metadata":{"host":"h","process":"curl","processPath":"/usr/bin/curl"},
             "upload":1,"download":2,
             "start":"2026-11-05T16:30:00.5+08:00","chains":["DIRECT"],"rule":"Match"},
            {"id":"b","metadata":{"host":"h"},"upload":3,"download":4,
             "start":"yesterday","chains":["Proxy"],"rule":"Match"},
            {"id":"c","upload":5,"download":6,"start":12,"chains":null}
        ]}"#;
        let snapshot: ConnectionSnapshot = serde_json::from_str(json).unwrap();
        let [a, b, c] = &snapshot.connections[..] else {
            panic!("three connections expected");
        };
        assert!(a.start.is_some());
        assert_eq!(
            (
                a.metadata.process.as_str(),
                a.metadata.process_path.as_str()
            ),
            ("curl", "/usr/bin/curl")
        );
        assert_eq!(b.start, None);
        assert_eq!(
            (b.id.as_str(), b.upload, b.download, b.chains.as_slice()),
            ("b", 3, 4, ["Proxy".to_owned()].as_slice())
        );
        assert_eq!(b.rule, "Match");
        // Fields the kernel left out are empty, not a failed frame.
        assert_eq!(b.metadata.process, "");
        assert_eq!(b.metadata.destination_port, "");
        assert_eq!(b.rule_payload, "");
        assert_eq!((c.start, c.upload, c.chains.len()), (None, 5, 0));
    }

    fn frame(connections: &[(&str, u64, u64)]) -> ConnectionSnapshot {
        ConnectionSnapshot {
            connections: connections
                .iter()
                .map(|&(id, upload, download)| Connection {
                    id: id.to_owned(),
                    upload,
                    download,
                    ..Connection::default()
                })
                .collect(),
            ..ConnectionSnapshot::default()
        }
    }

    #[test]
    fn rates_are_the_growth_since_the_previous_frame() {
        let (_, baseline) = frame(&[("a", 100, 1000), ("b", 5, 5)]).rates(&Baseline::new());
        let (rates, _) = frame(&[("b", 5, 50), ("a", 164, 1000)]).rates(&baseline);
        // In the order of the frame, not of the baseline.
        assert_eq!(
            rates,
            [
                Some(Rate { up: 0, down: 45 }),
                Some(Rate { up: 64, down: 0 })
            ]
        );
    }

    #[test]
    fn a_connection_new_in_the_frame_has_no_rate() {
        let (_, baseline) = frame(&[("a", 100, 100)]).rates(&Baseline::new());
        let (rates, _) = frame(&[("a", 150, 100), ("new", 9_999, 9_999)]).rates(&baseline);
        assert_eq!(rates, [Some(Rate { up: 50, down: 0 }), None]);
    }

    #[test]
    fn a_connection_that_is_gone_leaves_the_baseline() {
        let (_, baseline) = frame(&[("a", 1, 1), ("gone", 1, 1)]).rates(&Baseline::new());
        let (rates, baseline) = frame(&[("a", 2, 2)]).rates(&baseline);
        assert_eq!(rates.len(), 1);
        assert_eq!(baseline.keys().collect::<Vec<_>>(), ["a"]);
        // Were it still there, an id that comes back would be measured
        // against bytes from long ago.
        let (rates, _) = frame(&[("gone", 50, 50)]).rates(&baseline);
        assert_eq!(rates, [None]);
    }

    #[test]
    fn totals_that_shrink_count_as_no_traffic() {
        let (_, baseline) = frame(&[("a", 500, 500)]).rates(&Baseline::new());
        let (rates, _) = frame(&[("a", 100, 900)]).rates(&baseline);
        assert_eq!(rates, [Some(Rate { up: 0, down: 400 })]);
    }

    #[test]
    fn parses_a_memory_frame() {
        let frame: Memory =
            serde_json::from_str(include_str!("../../tests/fixtures/mihomo_memory.json")).unwrap();
        assert_eq!(frame, Memory { inuse: 188_715_008 });
        let frame: Memory = serde_json::from_str("{}").unwrap();
        assert_eq!(frame, Memory::default());
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

    #[test]
    fn parses_a_mihomo_log_line() {
        let log: Log =
            serde_json::from_str(include_str!("../../tests/fixtures/mihomo_log.json")).unwrap();
        assert_eq!(log.kind, LogKind::Level(LogLevel::Info));
        assert_eq!(
            log.payload,
            "[TCP] 127.0.0.1:37440 --> example.com:80 match DomainSuffix(example.com) using \
             节点选择[直连 A]"
        );
    }

    #[test]
    fn a_log_line_of_an_unknown_level_keeps_its_word() {
        let log: Log = serde_json::from_str(r#"{"type":"silent","payload":"x"}"#).unwrap();
        assert_eq!(log.kind, LogKind::Other("silent".to_owned()));
        let log: Log = serde_json::from_str(r#"{"type":"WARNING","payload":"x"}"#).unwrap();
        assert_eq!(log.kind, LogKind::Level(LogLevel::Warning));
        // `warn` is not how either kernel spells it.
        let log: Log = serde_json::from_str(r#"{"type":"warn"}"#).unwrap();
        assert_eq!(log.kind, LogKind::Other("warn".to_owned()));
        assert_eq!(log.payload, "");
    }

    #[test]
    fn log_levels_cycle_through_all_four() {
        let mut level = LogLevel::default();
        let mut seen = vec![];
        for _ in 0..5 {
            seen.push(level);
            level = level.next();
        }
        assert_eq!(
            seen,
            [
                LogLevel::Info,
                LogLevel::Warning,
                LogLevel::Error,
                LogLevel::Debug,
                LogLevel::Info
            ]
        );
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

    #[test]
    fn provider_nodes_fill_in_what_proxies_leaves_out() {
        // `直连 B` is only in the Compatible `负载均衡`; `集合 直连 2` is in
        // `/proxies` too, under another type.
        let mut proxies: Proxies = serde_json::from_str(
            r#"{"proxies":{
                "节点选择":{"name":"节点选择","type":"Selector",
                    "all":["直连 B","集合 直连 1","集合 直连 2","订阅 直连 1"]},
                "集合 直连 2":{"name":"集合 直连 2","type":"Shadowsocks","history":[]}
            }}"#,
        )
        .unwrap();
        let list: ProviderList<ProxyProvider> = serde_json::from_str(include_str!(
            "../../tests/fixtures/mihomo_proxy_providers.json"
        ))
        .unwrap();
        proxies.add_provider_nodes(list.shown());

        let node = |name| proxies.get(name);
        let joined = node("集合 直连 1").unwrap();
        assert_eq!(
            (
                joined.kind.as_str(),
                joined.provider.as_deref(),
                joined.delay()
            ),
            ("Direct", Some("本地集合"), Some(Delay::Millis(3422)))
        );
        assert_eq!(
            node("订阅 直连 1").unwrap().provider.as_deref(),
            Some("机场 订阅")
        );
        let kept = node("集合 直连 2").unwrap();
        assert_eq!(
            (kept.kind.as_str(), kept.provider.as_deref(), kept.delay()),
            ("Shadowsocks", None, None)
        );
        assert!(node("直连 B").is_none());
    }

    fn rule(kind: &str, payload: &str, proxy: &str) -> Rule {
        Rule {
            kind: kind.to_owned(),
            payload: payload.to_owned(),
            proxy: proxy.to_owned(),
        }
    }

    #[test]
    fn parses_mihomo_rules_in_order() {
        let list: RuleList =
            serde_json::from_str(include_str!("../../tests/fixtures/mihomo_rules.json")).unwrap();
        assert_eq!(
            list.rules,
            [
                rule("RuleSet", "测试域名", "自动选择"),
                rule("DomainSuffix", "example.com", "节点选择"),
                rule("DomainKeyword", "mirror", "直连 A"),
                rule("IPCIDR", "127.0.0.0/8", "DIRECT"),
                rule("IPCIDR", "192.0.2.0/24", "死节点 1"),
                rule("Match", "", "节点选择"),
            ]
        );
    }

    #[test]
    fn parses_clash_rules_and_missing_fields() {
        // Original Clash: three fields only.
        let json = r#"{"rules":[
            {"type":"DomainSuffix","payload":"google.com","proxy":"Proxy"},
            {"type":"GeoIP","payload":"CN","proxy":"DIRECT"},
            {"type":"Match","payload":"","proxy":"Proxy"}]}"#;
        let list: RuleList = serde_json::from_str(json).unwrap();
        assert_eq!(list.rules[1], rule("GeoIP", "CN", "DIRECT"));
        assert_eq!(list.rules.len(), 3);

        let list: RuleList = serde_json::from_str(r#"{"rules":[{"type":"Match"},{}]}"#).unwrap();
        assert_eq!(list.rules, [rule("Match", "", ""), rule("", "", "")]);
        for json in [r#"{"rules":null}"#, "{}"] {
            let list: RuleList = serde_json::from_str(json).unwrap();
            assert!(list.rules.is_empty(), "{json}");
        }
    }

    fn names<'a, T>(providers: &'a [T], name: impl Fn(&'a T) -> &'a str) -> Vec<&'a str> {
        providers.iter().map(name).collect()
    }

    #[test]
    fn parses_mihomo_providers_without_the_compatible_ones() {
        let list: ProviderList<ProxyProvider> = serde_json::from_str(include_str!(
            "../../tests/fixtures/mihomo_proxy_providers.json"
        ))
        .unwrap();
        // `default` and the group `负载均衡` are `Compatible`.
        let proxies = list.shown();
        assert_eq!(names(&proxies, |p| &p.name), ["本地集合", "机场 订阅"]);
        let [file, http] = &proxies[..] else {
            unreachable!()
        };
        assert_eq!(file.vehicle_type, "File");
        assert_eq!(
            file.updated_at,
            Some("2026-11-05T18:26:08.170423241+08:00".parse().unwrap())
        );
        assert_eq!(file.subscription(), None);
        assert_eq!(http.vehicle_type, "HTTP");
        assert_eq!(
            http.subscription(),
            Some(Subscription {
                upload: 1_288_490_188,
                download: 12_079_595_520,
                total: 107_374_182_400,
                expire: 1_798_732_800,
            })
        );
        assert_eq!(
            (file.health(), http.health()),
            (
                Health {
                    alive: 2,
                    total: 3,
                    tested: true
                },
                Health {
                    alive: 2,
                    total: 4,
                    tested: true
                }
            )
        );

        let list: ProviderList<RuleProvider> = serde_json::from_str(include_str!(
            "../../tests/fixtures/mihomo_rule_providers.json"
        ))
        .unwrap();
        let rules = list.shown();
        assert_eq!(names(&rules, |r| &r.name), ["测试域名", "远程 IP"]);
        assert_eq!(
            [&rules[0].behavior, &rules[1].behavior],
            ["Domain", "IPCIDR"]
        );
        assert_eq!([rules[0].rule_count, rules[1].rule_count], [3, 2]);
        assert_eq!(rules[1].vehicle_type, "HTTP");
        assert!(rules[1].updated_at.is_some());
    }

    #[test]
    fn unknown_or_zero_update_times_and_empty_subscriptions_read_as_none() {
        let json = r#"{"providers":{
            "zero":{"vehicleType":"HTTP","updatedAt":"0001-01-01T00:00:00Z"},
            "missing":{"vehicleType":"File"},
            "garbage":{"vehicleType":"Inline","updatedAt":"yesterday",
                       "subscriptionInfo":{"Upload":0,"Download":0,"Total":0,"Expire":0}},
            "number":{"vehicleType":"HTTP","updatedAt":17,"subscriptionInfo":null}}}"#;
        let providers = serde_json::from_str::<ProviderList<ProxyProvider>>(json)
            .unwrap()
            .shown();
        assert_eq!(providers.len(), 4);
        for provider in &providers {
            assert_eq!(provider.updated_at, None, "{}", provider.name);
            assert_eq!(provider.subscription(), None, "{}", provider.name);
        }
        for json in [r#"{"providers":null}"#, "{}"] {
            let list: ProviderList<RuleProvider> = serde_json::from_str(json).unwrap();
            assert!(list.shown().is_empty(), "{json}");
        }
    }

    #[test]
    fn parses_clash_providers() {
        // Original Clash: no subscription, no test URL, a compatible provider
        // without `updatedAt`, and a history with `meanDelay`.
        let json = r#"{"providers":{
            "default":{"name":"default","type":"Proxy","vehicleType":"Compatible",
                "proxies":[{"name":"DIRECT","type":"Direct","history":[]}]},
            "sub":{"name":"sub","type":"Proxy","vehicleType":"HTTP",
                "updatedAt":"2026-11-01T08:00:00.123456789+08:00",
                "proxies":[{"name":"a","type":"Shadowsocks",
                    "history":[{"time":"2026-11-05T12:00:00Z","delay":120,"meanDelay":130}]}]}}}"#;
        let providers = serde_json::from_str::<ProviderList<ProxyProvider>>(json)
            .unwrap()
            .shown();
        assert_eq!(names(&providers, |p| &p.name), ["sub"]);
        assert!(providers[0].updated_at.is_some());
        assert_eq!(providers[0].subscription(), None);
        assert_eq!(
            providers[0].health(),
            Health {
                alive: 1,
                total: 1,
                tested: true
            }
        );
    }

    /// A provider whose nodes' histories end in these delays (`None`: never
    /// tested).
    fn provider(latest: &[Option<u32>]) -> ProxyProvider {
        let proxies = latest
            .iter()
            .map(|delay| Proxy {
                history: delay
                    .iter()
                    .flat_map(|delay| {
                        [
                            DelayRecord::default(),
                            DelayRecord {
                                delay: *delay,
                                timed_out: false,
                            },
                        ]
                    })
                    .collect(),
                ..Proxy::default()
            })
            .collect();
        ProxyProvider {
            proxies,
            ..ProxyProvider::default()
        }
    }

    #[test]
    fn health_counts_nodes_whose_latest_test_answered() {
        let health = |latest: &[Option<u32>]| {
            let health = provider(latest).health();
            (health.alive, health.total, health.tested)
        };
        // Only the latest test counts: every history starts with a failure.
        assert_eq!(health(&[Some(80), Some(120)]), (2, 2, true));
        assert_eq!(health(&[Some(80), Some(0), None]), (1, 3, true));
        assert_eq!(health(&[Some(0), Some(0)]), (0, 2, true));
        assert_eq!(health(&[None, None]), (0, 2, false));
        assert_eq!(health(&[]), (0, 0, false));
    }
}
