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
}
