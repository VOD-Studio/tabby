/// How the managed kernel is set up: what tabby writes into the config file
/// it runs, over whatever the subscription has (plan §7.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreSettings {
    pub mixed_port: u16,
    pub allow_lan: bool,
    pub ipv6: bool,
    pub log_level: String,
    /// The kernel's spelling of the mode.
    pub mode: String,
    pub tun: TunSettings,
}

/// The `tun` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunSettings {
    pub enable: bool,
    pub stack: String,
    pub auto_route: bool,
    pub auto_detect_interface: bool,
    pub dns_hijack: Vec<String>,
    pub mtu: u32,
}

impl Default for CoreSettings {
    fn default() -> Self {
        Self {
            mixed_port: 7890,
            allow_lan: false,
            ipv6: false,
            log_level: "info".to_owned(),
            mode: "rule".to_owned(),
            tun: TunSettings {
                enable: false,
                // The official kernel has no `mips`, which FlClash defaults
                // to.
                stack: "mixed".to_owned(),
                auto_route: true,
                auto_detect_interface: true,
                dns_hijack: vec!["any:53".to_owned()],
                mtu: 9000,
            },
        }
    }
}
