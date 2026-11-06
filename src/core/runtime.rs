use std::fmt::Write as _;

use saphyr_parser::{Event, Parser};

use serde_json::{Value, json};

use super::{
    settings::{CoreSettings, to_json},
    state::State,
};

/// What the kernel runs when no subscription is imported: everything goes
/// out directly.
pub const BUILTIN: &str = "proxies: []\nproxy-groups: []\nrules:\n  - MATCH,DIRECT\n";

/// The keys of a profile tabby sets itself, and so takes out of it. The
/// controller's other ways in are among them, so that a subscription cannot
/// open one; so are the ports other than the mixed one, as tabby listens on
/// one port only.
const TAKEN: [&str; 16] = [
    "external-controller",
    "external-controller-tls",
    "external-controller-unix",
    "external-controller-pipe",
    "secret",
    "profile",
    "port",
    "socks-port",
    "redir-port",
    "tproxy-port",
    "mixed-port",
    "allow-lan",
    "ipv6",
    "log-level",
    "mode",
    "tun",
];

/// A profile whose top level is not a block mapping of one document: a
/// `{…}` flow, several documents, or nothing. What tabby cannot take keys
/// out of without guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported;

/// What [`edit`] made of a profile.
#[derive(Debug, PartialEq, Eq)]
pub struct Edited {
    pub text: String,
    /// The `dns` overrides asked for have no `dns` section of the profile to
    /// go into (it has none, or it is not a block mapping, and was taken
    /// out): the caller writes one.
    pub dns_missing: bool,
}

/// The block-style `dns` section of a profile.
struct DnsBlock {
    /// The keys it has, with the line each starts on.
    children: Vec<(usize, String)>,
    /// The column they start in.
    indent: usize,
}

/// Whether the YAML at character `index` of `text` opens a flow collection.
fn is_flow(text: &str, index: usize) -> bool {
    text.chars().nth(index) == Some('{')
}

/// The top-level entries of a profile (each with the line it starts on), and
/// its `dns` section when that is a block mapping.
type Scan = (Vec<(usize, String)>, Option<DnsBlock>);

fn scan(text: &str) -> Result<Scan, Unsupported> {
    let mut entries: Vec<(usize, String)> = Vec::new();
    let mut block: Option<DnsBlock> = None;
    let mut depth = 0usize;
    let mut documents = 0;
    let mut top = false;
    // Whether the next scalar at the depth is a key.
    let (mut expect_key, mut dns_expect, mut in_dns) = (true, true, false);
    for event in Parser::new_from_str(text) {
        let Ok((event, span)) = event else {
            return Err(Unsupported);
        };
        match event {
            Event::DocumentStart(_) => {
                documents += 1;
                if documents > 1 {
                    return Err(Unsupported);
                }
            }
            Event::MappingStart(..) if depth == 0 => {
                if is_flow(text, span.start.index()) {
                    return Err(Unsupported);
                }
                top = true;
                depth = 1;
            }
            // Anything but a mapping on top.
            Event::SequenceStart(..) | Event::Scalar(..) | Event::Alias(_) if depth == 0 => {
                return Err(Unsupported);
            }
            // The value of `dns`, when it is a block mapping.
            Event::MappingStart(..)
                if depth == 1
                    && !expect_key
                    && entries.last().is_some_and(|(_, name)| name == "dns")
                    && !is_flow(text, span.start.index()) =>
            {
                block = Some(DnsBlock {
                    children: vec![],
                    indent: 0,
                });
                (in_dns, dns_expect, depth) = (true, true, 2);
            }
            Event::MappingStart(..) | Event::SequenceStart(..) => depth += 1,
            Event::MappingEnd | Event::SequenceEnd => {
                depth -= 1;
                if depth == 1 {
                    (expect_key, in_dns) = (true, false);
                } else if depth == 2 && in_dns {
                    dns_expect = true;
                }
            }
            Event::Scalar(name, ..) if depth == 1 => {
                if expect_key {
                    entries.push((span.start.line(), name.into_owned()));
                }
                expect_key = !expect_key;
            }
            Event::Scalar(name, ..) if depth == 2 && in_dns => {
                if dns_expect && let Some(block) = &mut block {
                    if block.children.is_empty() {
                        block.indent = span.start.col();
                    }
                    block.children.push((span.start.line(), name.into_owned()));
                }
                dns_expect = !dns_expect;
            }
            Event::Alias(_) if depth == 1 => expect_key = !expect_key,
            Event::Alias(_) if depth == 2 && in_dns => dns_expect = !dns_expect,
            _ => {}
        }
    }
    if !top {
        return Err(Unsupported);
    }
    Ok((entries, block))
}

/// `text` without the top-level entries called one of `remove`, each taken
/// out from the line it starts on to the line of the next entry; and with
/// the `dns` entries (`(key, JSON value)`) set in its `dns` section: the
/// keys of the section the profile has are taken out as the entries are,
/// the new ones go at the end of the section. Nothing else is touched, not
/// a byte: a profile read and written back through a YAML library would
/// turn `short-id: 0123` into a number, drop its comments and renumber its
/// anchors (plan §7.2).
pub fn edit(text: &str, remove: &[&str], dns: &[(&str, String)]) -> Result<Edited, Unsupported> {
    let (entries, block) = scan(text)?;
    // Byte offset of the start of each line, the first being line 1.
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(at, _)| at + 1));
    let line_start = |line: usize| starts.get(line - 1).copied().unwrap_or(text.len());
    let end_of = |index: usize| {
        entries
            .get(index + 1)
            .map_or(text.len(), |(next, _)| line_start(*next))
    };
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };

    // (from, to, what goes there)
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for (index, (line, name)) in entries.iter().enumerate() {
        if remove.contains(&name.as_str()) {
            edits.push((line_start(*line), end_of(index), String::new()));
        }
        if name != "dns" || dns.is_empty() {
            continue;
        }
        let Some(block) = &block else {
            // Not a block mapping: it goes, and is written anew.
            edits.push((line_start(*line), end_of(index), String::new()));
            continue;
        };
        for (child, (at, key)) in block.children.iter().enumerate() {
            if dns.iter().any(|(set, _)| set == key) {
                let to = block
                    .children
                    .get(child + 1)
                    .map_or(end_of(index), |(next, _)| line_start(*next));
                edits.push((line_start(*at), to, String::new()));
            }
        }
        let at = end_of(index);
        let mut lines = String::new();
        if at > 0 && !text[..at].ends_with('\n') {
            lines.push_str(newline);
        }
        for (key, value) in dns {
            let _ = write!(
                lines,
                "{}{}: {value}{newline}",
                " ".repeat(block.indent),
                json!(key)
            );
        }
        edits.push((at, at, lines));
    }
    edits.sort_by_key(|(from, to, _)| (*from, *to));
    let mut kept = String::with_capacity(text.len());
    let mut from = 0;
    for (start, end, with) in edits {
        kept.push_str(&text[from..start]);
        kept.push_str(&with);
        from = end;
    }
    kept.push_str(&text[from..]);
    let has_section = block.is_some();
    Ok(Edited {
        text: kept,
        dns_missing: !dns.is_empty() && !has_section,
    })
}

/// The config file the kernel is started with: `base` (a profile) followed
/// by the block of keys tabby takes over.
///
/// The block is written in JSON syntax, which YAML accepts: values need no
/// YAML emitter, and nothing in `base` is rewritten (plan §7.2). `base`
/// must not set a key of the block itself, as the kernel rejects a key set
/// twice: [`edit`] takes them out first. A profile that is not a block
/// mapping at the top is [`Unsupported`].
pub fn render(base: &str, settings: &CoreSettings, state: &State) -> Result<String, Unsupported> {
    let dns: Vec<(&str, String)> = settings
        .dns
        .iter()
        .map(|(key, value)| (key.as_str(), to_json(value).to_string()))
        .collect();
    let edited = edit(base, &TAKEN, &dns)?;
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
    let mut text = edited.text;
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
    if edited.dns_missing {
        // The profile has no section to set the keys in: a new one, which
        // has to be on to do anything.
        let mut section = serde_json::Map::new();
        section.insert("enable".to_owned(), Value::Bool(true));
        for (key, value) in &settings.dns {
            section.insert(key.clone(), to_json(value));
        }
        let _ = writeln!(text, "\"dns\": {}", Value::Object(section));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`edit`] with only keys to take out.
    fn strip(text: &str, keys: &[&str]) -> Result<String, Unsupported> {
        edit(text, keys, &[]).map(|edited| edited.text)
    }

    fn state() -> State {
        State {
            port: 41234,
            secret: "s3cret".to_owned(),
            unit: "tabby-mihomo-00000000".to_owned(),
            pid: None,
            pid_start: None,
        }
    }

    #[test]
    fn the_builtin_profile_is_kept_as_it_is_and_the_managed_keys_follow() {
        let text = render(BUILTIN, &CoreSettings::default(), &state()).unwrap();
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
        let text = render("rules: []", &CoreSettings::default(), &state()).unwrap();
        assert!(text.starts_with("rules: []\n# The keys below"), "{text}");
    }

    #[test]
    fn a_secret_is_quoted_whatever_it_holds() {
        let mut state = state();
        state.secret = "a\"b: c #d".to_owned();
        let text = render(BUILTIN, &CoreSettings::default(), &state).unwrap();
        assert!(text.contains(r#""secret": "a\"b: c #d""#), "{text}");
    }

    const PROFILE: &str = "---
# my subscription
port: 7890
mixed-port: 7891
proxies:
  - {name: a, type: vless, reality-opts: {short-id: 0123}}
  - &shared {name: b, type: ss}
proxy-groups:
  - name: g   # the group
    proxies: [a, b]
    again: *shared
mode: global
rules: [MATCH,g]
tun:
  enable: true
  stack: system
";

    /// What [`PROFILE`] is without the keys of the managed block.
    const KEPT: &str = "---
# my subscription
proxies:
  - {name: a, type: vless, reality-opts: {short-id: 0123}}
  - &shared {name: b, type: ss}
proxy-groups:
  - name: g   # the group
    proxies: [a, b]
    again: *shared
rules: [MATCH,g]
";

    #[test]
    fn what_is_taken_out_is_the_only_thing_that_changes_and_not_a_byte_else() {
        // Quoted numbers, anchors, aliases, comments and the document
        // marker all stay as written.
        assert_eq!(strip(PROFILE, &TAKEN).unwrap(), KEPT);
        // Line endings are not touched either.
        let crlf = PROFILE.replace('\n', "\r\n");
        assert_eq!(strip(&crlf, &TAKEN).unwrap(), KEPT.replace('\n', "\r\n"));
    }

    #[test]
    fn a_key_in_a_nested_map_is_not_a_key_of_the_top() {
        let text = "proxies:\n  - {name: a, mode: x}\ndns:\n  mode: fake\n  ipv6: true\n";
        assert_eq!(strip(text, &TAKEN).unwrap(), text);
    }

    #[test]
    fn the_managed_block_comes_once_after_a_profile_that_set_the_same_keys() {
        let text = render(PROFILE, &CoreSettings::default(), &state()).unwrap();
        assert!(text.starts_with(KEPT), "{text}");
        for key in ["mixed-port", "mode", "tun", "secret", "external-controller"] {
            assert_eq!(text.matches(&format!("\"{key}\":")).count(), 1, "{key}");
            assert_eq!(text.matches(&format!("\n{key}:")).count(), 0, "{key}");
        }
    }

    #[test]
    fn a_top_level_that_is_not_one_block_mapping_is_unsupported() {
        for text in [
            "{proxies: [], mode: global}",
            "- a\n- b\n",
            "just text",
            "",
            "a: 1\n---\nb: 2\n",
            "a: [unclosed",
        ] {
            assert_eq!(strip(text, &TAKEN), Err(Unsupported), "{text:?}");
        }
    }

    /// A `dns` override as `edit` takes it.
    fn set(key: &'static str, json: &str) -> (&'static str, String) {
        (key, json.to_owned())
    }

    #[test]
    fn the_dns_keys_set_replace_the_same_keys_of_the_profiles_section_and_nothing_else_in_it() {
        let text = "# top\nproxies: []\ndns:\n  enable: true\n  # kept\n  nameserver:\n    - 8.8.8.8\n    - 1.1.1.1\n  ipv6: false\n  fake-ip-filter: ['*.lan']\nrules: []\n";
        let edited = edit(
            text,
            &[],
            &[
                set("nameserver", r#"["https://dns.test/q"]"#),
                set("ipv6", "true"),
            ],
        )
        .unwrap();
        assert!(!edited.dns_missing);
        assert_eq!(
            edited.text,
            "# top\nproxies: []\ndns:\n  enable: true\n  # kept\n  fake-ip-filter: ['*.lan']\n  \"nameserver\": [\"https://dns.test/q\"]\n  \"ipv6\": true\nrules: []\n"
        );
    }

    #[test]
    fn a_dns_section_at_the_end_of_the_file_takes_the_new_keys_after_it_even_without_a_final_newline()
     {
        let edited = edit(
            "dns:\n  enable: true\n  nameserver: [a]",
            &[],
            &[set("nameserver", "[\"b\"]")],
        )
        .unwrap();
        assert_eq!(
            edited.text,
            "dns:\n  enable: true\n\n  \"nameserver\": [\"b\"]\n"
        );
        // Its lines keep the file's line ends.
        let crlf = edit(
            "dns:\r\n    enable: true\r\nrules: []\r\n",
            &[],
            &[set("ipv6", "true")],
        )
        .unwrap();
        assert_eq!(
            crlf.text,
            "dns:\r\n    enable: true\r\n    \"ipv6\": true\r\nrules: []\r\n"
        );
    }

    #[test]
    fn without_a_block_dns_section_the_keys_are_written_in_a_new_one_and_a_flow_one_goes() {
        for text in [
            "rules: []\n",
            "dns: {enable: true, ipv6: false}\nrules: []\n",
            "dns:\nrules: []\n",
        ] {
            let edited = edit(text, &[], &[set("ipv6", "true")]).unwrap();
            assert!(edited.dns_missing, "{text}");
            assert_eq!(edited.text, "rules: []\n", "{text}");
        }
        // With no keys to set a profile's own section is left alone, whatever its style.
        let own = "dns: {enable: true}\nrules: []\n";
        assert_eq!(
            edit(own, &[], &[]).unwrap(),
            Edited {
                text: own.to_owned(),
                dns_missing: false
            }
        );
    }

    #[test]
    fn the_rendered_config_has_one_dns_section_with_the_keys_set() {
        let mut settings = CoreSettings::default();
        settings
            .dns
            .insert("ipv6".to_owned(), toml::Value::Boolean(true));
        settings.dns.insert(
            "nameserver".to_owned(),
            toml::Value::Array(vec![toml::Value::String("1.1.1.1".to_owned())]),
        );
        // A profile with its own: its other keys stay, the two are set.
        let own = render(
            "dns:\n  enable: true\n  ipv6: false\n  listen: ':53'\n",
            &settings,
            &state(),
        )
        .unwrap();
        assert!(own.starts_with("dns:\n  enable: true\n  listen: ':53'\n  \"ipv6\": true\n  \"nameserver\": [\"1.1.1.1\"]\n"), "{own}");
        assert_eq!(own.matches("dns:").count(), 1, "{own}");
        // A profile without: one is added, and turned on.
        let none = render(BUILTIN, &settings, &state()).unwrap();
        assert!(
            none.contains("\"dns\": {\"enable\":true,\"ipv6\":true,\"nameserver\":[\"1.1.1.1\"]}"),
            "{none}"
        );
    }

    #[test]
    fn the_settings_are_what_the_managed_keys_say() {
        let mut settings = CoreSettings::default();
        for change in [
            crate::core::settings::Persist::MixedPort(17890),
            crate::core::settings::Persist::AllowLan(true),
            crate::core::settings::Persist::Mode("global".to_owned()),
            crate::core::settings::Persist::LogLevel("debug".to_owned()),
            crate::core::settings::Persist::Ipv6(true),
            crate::core::settings::Persist::Tun(true),
        ] {
            settings.set(change);
        }
        let text = render(BUILTIN, &settings, &state()).unwrap();
        for line in [
            "\"mixed-port\": 17890",
            "\"allow-lan\": true",
            "\"mode\": \"global\"",
            "\"log-level\": \"debug\"",
            "\"ipv6\": true",
            "\"tun\": {\"auto-detect-interface\":true,\"auto-route\":true,\"dns-hijack\":[\"any:53\"],\"enable\":true,\"mtu\":9000,\"stack\":\"mixed\"}",
        ] {
            assert_eq!(text.matches(line).count(), 1, "{line} in\n{text}");
        }
    }
}
