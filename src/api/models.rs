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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Tun {
    pub enable: bool,
}

/// The name of the built-in group that lists every proxy and group, in
/// configuration order.
const GLOBAL: &str = "GLOBAL";

/// One entry of `GET /proxies`: a node, or a group when `all` is present.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Proxy {
    pub name: String,
    /// Member names; only groups have them.
    pub all: Option<Vec<String>>,
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
