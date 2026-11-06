use std::fmt::Write as _;

use serde_json::{Value, json};

use super::{settings::CoreSettings, state::State};

/// What the kernel runs when no subscription is imported: everything goes
/// out directly.
pub const BUILTIN: &str = "proxies: []\nproxy-groups: []\nrules:\n  - MATCH,DIRECT\n";

/// The config file the kernel is started with: `base` (a profile) followed
/// by the block of keys tabby takes over.
///
/// The block is written in JSON syntax, which YAML accepts: values need no
/// YAML emitter, and nothing in `base` is rewritten (plan §7.2). `base`
/// must not set a key of the block itself; the kernel rejects a key set
/// twice.
pub fn render(base: &str, settings: &CoreSettings, state: &State) -> String {
    let tun = &settings.tun;
    let overrides: [(&str, Value); 8] = [
        (
            "external-controller",
            json!(format!("127.0.0.1:{}", state.port)),
        ),
        ("secret", json!(state.secret)),
        ("profile", json!({ "store-selected": true })),
        ("mixed-port", json!(settings.mixed_port)),
        ("allow-lan", json!(settings.allow_lan)),
        ("ipv6", json!(settings.ipv6)),
        ("log-level", json!(settings.log_level)),
        ("mode", json!(settings.mode)),
    ];
    let mut text = String::from(base);
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("# The keys below are managed by tabby.\n");
    for (key, value) in overrides {
        let _ = writeln!(text, "{}: {value}", json!(key));
    }
    let tun = json!({
        "enable": tun.enable,
        "stack": tun.stack,
        "auto-route": tun.auto_route,
        "auto-detect-interface": tun.auto_detect_interface,
        "dns-hijack": tun.dns_hijack,
        "mtu": tun.mtu,
    });
    let _ = writeln!(text, "\"tun\": {tun}");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State {
            port: 41234,
            secret: "s3cret".to_owned(),
            unit: "tabby-mihomo-00000000".to_owned(),
            pid: None,
        }
    }

    #[test]
    fn the_builtin_profile_is_kept_as_it_is_and_the_managed_keys_follow() {
        let text = render(BUILTIN, &CoreSettings::default(), &state());
        assert!(text.starts_with(BUILTIN), "{text}");
        for line in [
            "\"external-controller\": \"127.0.0.1:41234\"",
            "\"secret\": \"s3cret\"",
            "\"profile\": {\"store-selected\":true}",
            "\"mixed-port\": 7890",
            "\"allow-lan\": false",
            "\"mode\": \"rule\"",
        ] {
            assert_eq!(text.matches(line).count(), 1, "{line} in\n{text}");
        }
        assert!(text.contains("\"tun\": {"), "{text}");
    }

    #[test]
    fn a_base_without_a_final_newline_does_not_swallow_the_first_key() {
        let text = render("rules: []", &CoreSettings::default(), &state());
        assert!(text.starts_with("rules: []\n# The keys below"), "{text}");
    }

    #[test]
    fn a_secret_is_quoted_whatever_it_holds() {
        let mut state = state();
        state.secret = "a\"b: c #d".to_owned();
        let text = render(BUILTIN, &CoreSettings::default(), &state);
        assert!(text.contains(r#""secret": "a\"b: c #d""#), "{text}");
    }
}
