use std::fmt::Write as _;

use saphyr_parser::{Event, Parser};

use serde_json::{Value, json};

use super::{settings::CoreSettings, state::State};

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

/// `text` without the top-level entries called one of `keys`, each taken
/// out from the line it starts on to the line of the next entry. Nothing
/// else is touched, not a byte: a profile read and written back through a
/// YAML library would turn `short-id: 0123` into a number, drop its
/// comments and renumber its anchors (plan §7.2).
pub fn strip(text: &str, keys: &[&str]) -> Result<String, Unsupported> {
    // The line each top-level key starts on, with the key's name.
    let mut entries: Vec<(usize, String)> = Vec::new();
    let mut depth = 0usize;
    let mut documents = 0;
    let mut top = false;
    let mut expect_key = true;
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
                // A flow mapping starts with its brace.
                let at = text
                    .char_indices()
                    .nth(span.start.index())
                    .map(|(byte, _)| byte);
                if at.is_some_and(|byte| text[byte..].starts_with('{')) {
                    return Err(Unsupported);
                }
                top = true;
                depth = 1;
            }
            // Anything but a mapping on top.
            Event::SequenceStart(..) | Event::Scalar(..) | Event::Alias(_) if depth == 0 => {
                return Err(Unsupported);
            }
            Event::MappingStart(..) | Event::SequenceStart(..) => depth += 1,
            Event::MappingEnd | Event::SequenceEnd => {
                depth -= 1;
                if depth == 1 {
                    expect_key = true;
                }
            }
            Event::Scalar(name, ..) if depth == 1 => {
                if expect_key {
                    entries.push((span.start.line(), name.into_owned()));
                }
                expect_key = !expect_key;
            }
            Event::Alias(_) if depth == 1 => expect_key = !expect_key,
            _ => {}
        }
    }
    if !top {
        return Err(Unsupported);
    }

    // Byte offset of the start of each line, the first being line 1.
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(at, _)| at + 1));
    let line_start = |line: usize| starts.get(line - 1).copied().unwrap_or(text.len());
    let mut kept = String::with_capacity(text.len());
    let mut from = 0;
    for (index, (line, name)) in entries.iter().enumerate() {
        if !keys.contains(&name.as_str()) {
            continue;
        }
        let end = entries
            .get(index + 1)
            .map_or(text.len(), |(next, _)| line_start(*next));
        kept.push_str(&text[from..line_start(*line)]);
        from = end;
    }
    kept.push_str(&text[from..]);
    Ok(kept)
}

/// The config file the kernel is started with: `base` (a profile) followed
/// by the block of keys tabby takes over.
///
/// The block is written in JSON syntax, which YAML accepts: values need no
/// YAML emitter, and nothing in `base` is rewritten (plan §7.2). `base`
/// must not set a key of the block itself, as the kernel rejects a key set
/// twice: [`strip`] takes them out first. A profile that is not a block
/// mapping at the top is [`Unsupported`].
pub fn render(base: &str, settings: &CoreSettings, state: &State) -> Result<String, Unsupported> {
    let base = strip(base, &TAKEN)?;
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
    let mut text = base;
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
    Ok(text)
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
}
