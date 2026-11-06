//! What tabby keeps about the subscriptions, and how it reads them (plan
//! §7.1): the list in `subscriptions.toml`, the headers a subscription
//! server sends, and the share-link lists some of them send instead of a
//! config.

use std::{fs, io, path::Path};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};

use super::files::write_private;

/// How often a subscription is updated, unless told otherwise: once a day,
/// as in `FlClash`.
pub const DEFAULT_INTERVAL_HOURS: u32 = 24;

/// Where a subscription comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// An address the server is asked for it.
    Url,
    /// A file on this machine, read again on every update.
    File,
}

/// What a subscription server says it has used and until when. Zero is not
/// reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub upload: i64,
    pub download: i64,
    pub total: i64,
    /// Unix seconds.
    pub expire: i64,
}

/// One imported subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    /// Names its files: `profiles/<id>.yaml`.
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// The address, or the path of the file it was read from.
    pub source: String,
    #[serde(default = "yes")]
    pub auto_update: bool,
    #[serde(default = "default_interval")]
    pub interval_hours: u32,
    /// Unix seconds of the last import or update.
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// The age secret key the subscription is encrypted to; kept with the
    /// rest of the list, which only its owner can read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_key: Option<String>,
}

fn yes() -> bool {
    true
}

fn default_interval() -> u32 {
    DEFAULT_INTERVAL_HOURS
}

/// The contents of `subscriptions.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    /// The id of the subscription the kernel runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    #[serde(default, rename = "subscription")]
    pub subscriptions: Vec<Subscription>,
}

impl Store {
    /// Reads `path`; no file is an empty list.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    /// Writes the list where only the user can read it: it holds the
    /// addresses, which carry their owner's token.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
        write_private(path, &text)
    }

    pub fn get(&self, id: &str) -> Option<&Subscription> {
        self.subscriptions.iter().find(|sub| sub.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Subscription> {
        self.subscriptions.iter_mut().find(|sub| sub.id == id)
    }

    /// The subscription the kernel runs.
    pub fn active(&self) -> Option<&Subscription> {
        self.get(self.active.as_deref()?)
    }

    /// The subscriptions to update at `now` (unix seconds): those that are
    /// asked for from an address, with auto-update on, whose last update is
    /// more than their interval ago. One never updated is due. A file is
    /// read when the user says so: it is theirs, and may be half written.
    pub fn due(&self, now: i64) -> Vec<&Subscription> {
        self.subscriptions
            .iter()
            .filter(|sub| {
                let interval = i64::from(sub.interval_hours.max(1)) * 3600;
                sub.kind == Kind::Url
                    && sub.auto_update
                    && sub.updated_at.saturating_add(interval) <= now
            })
            .collect()
    }
}

/// What a `subscription-userinfo` header says
/// (`upload=1; download=2; total=3; expire=4`), as the providers page reads
/// it: a field left out is 0, and a header that reports nothing is `None`.
pub fn parse_usage(header: &str) -> Option<Usage> {
    let mut usage = Usage::default();
    for part in header.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let value = value.trim().parse().unwrap_or(0);
        match key.trim().to_ascii_lowercase().as_str() {
            "upload" => usage.upload = value,
            "download" => usage.download = value,
            "total" => usage.total = value,
            "expire" => usage.expire = value,
            _ => {}
        }
    }
    (usage != Usage::default()).then_some(usage)
}

/// The file name a `content-disposition` header gives, without its
/// extension: `filename*=UTF-8''…` (percent-encoded) first, as RFC 6266
/// has it win over `filename=`, which may be quoted.
pub fn parse_filename(header: &str) -> Option<String> {
    let param = |name: &str| {
        header.split(';').find_map(|part| {
            let (key, value) = part.split_once('=')?;
            (key.trim().eq_ignore_ascii_case(name)).then(|| value.trim().to_owned())
        })
    };
    let name = if let Some(extended) = param("filename*") {
        // `charset'language'value`: only UTF-8 is read.
        let mut parts = extended.splitn(3, '\'');
        let (charset, _, value) = (parts.next()?, parts.next()?, parts.next()?);
        if !charset.eq_ignore_ascii_case("utf-8") {
            return param("filename").and_then(|plain| plain_name(&plain));
        }
        percent_decode_str(value).decode_utf8().ok()?.into_owned()
    } else {
        plain_name(&param("filename")?)?
    };
    let name = strip_extension(name.trim());
    (!name.is_empty()).then_some(name)
}

fn plain_name(value: &str) -> Option<String> {
    let value = value.trim().trim_matches('"');
    (!value.is_empty()).then(|| strip_extension(value))
}

/// `name` without a `.yaml`, `.yml`, `.txt` or `.conf` ending.
fn strip_extension(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for extension in [".yaml", ".yml", ".txt", ".conf"] {
        if lower.ends_with(extension) {
            return name[..name.len() - extension.len()].to_owned();
        }
    }
    name.to_owned()
}

/// The schemes of the share links the kernel turns into nodes.
const SCHEMES: [&str; 12] = [
    "ss",
    "ssr",
    "vmess",
    "vless",
    "trojan",
    "hysteria",
    "hysteria2",
    "hy2",
    "tuic",
    "anytls",
    "socks",
    "socks5",
];

/// How many share links `text` holds, plain or base64 as a whole: servers
/// send either, and so does the kernel's provider read either. 0 for
/// anything else; `mihomo -t` cannot tell, as it does not open provider
/// files.
pub fn link_count(text: &str) -> usize {
    let decoded;
    let text = if text.contains("://") {
        text
    } else {
        let compact: String = text.split_whitespace().collect();
        let Some(bytes) = [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD]
            .iter()
            .find_map(|engine| engine.decode(&compact).ok())
        else {
            return 0;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return 0;
        };
        decoded = text;
        &decoded
    };
    text.lines()
        .filter(|line| {
            line.trim()
                .split_once("://")
                .is_some_and(|(scheme, _)| SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()))
        })
        .count()
}

/// The first complaint in what `mihomo -t` printed: the `msg` of its first
/// `level=error` or `level=fatal` line, else its first line.
pub fn first_error(output: &str) -> String {
    let message = |line: &str| -> Option<String> {
        let rest = &line[line.find("msg=\"")? + 5..];
        let mut text = String::new();
        let mut chars = rest.chars();
        while let Some(ch) = chars.next() {
            match ch {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => text.push('\n'),
                    Some(other) => text.push(other),
                    None => break,
                },
                other => text.push(other),
            }
        }
        Some(text)
    };
    output
        .lines()
        .find(|line| line.contains("level=error") || line.contains("level=fatal"))
        .and_then(message)
        .or_else(|| {
            output
                .lines()
                .find(|line| !line.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usage_header_reads_what_it_has_and_nothing_when_it_reports_none() {
        assert_eq!(
            parse_usage(
                "upload=455727941; download=6174315083; total=1073741824000; expire=1671815872"
            ),
            Some(Usage {
                upload: 455_727_941,
                download: 6_174_315_083,
                total: 1_073_741_824_000,
                expire: 1_671_815_872,
            })
        );
        // Fields a server leaves out are 0, and odd ones do not spoil the rest.
        assert_eq!(
            parse_usage("download=5;total=oops;Expire=9;unknown=1"),
            Some(Usage {
                download: 5,
                expire: 9,
                ..Usage::default()
            })
        );
        assert_eq!(parse_usage("upload=0; download=0; total=0; expire=0"), None);
        assert_eq!(parse_usage("nonsense"), None);
        assert_eq!(parse_usage(""), None);
    }

    #[test]
    fn a_name_comes_from_the_extended_filename_first_then_the_plain_one() {
        let cases = [
            // RFC 5987, percent-encoded UTF-8.
            (
                "attachment; filename*=UTF-8''%E6%9C%BA%E5%9C%BA.yaml",
                Some("机场"),
            ),
            // It wins over a plain one beside it.
            (
                "attachment; filename=\"fallback.yaml\"; filename*=UTF-8''%E6%9C%BA%E5%9C%BA",
                Some("机场"),
            ),
            ("attachment; filename=\"My Sub.yaml\"", Some("My Sub")),
            ("attachment; filename=plain.txt", Some("plain")),
            ("attachment; filename=noextension", Some("noextension")),
            ("attachment; FILENAME=Upper.YML", Some("Upper")),
            // Another charset is not read; the plain one is.
            (
                "attachment; filename*=ISO-8859-1''%E9.yaml; filename=latin.yaml",
                Some("latin"),
            ),
            ("attachment", None),
            ("attachment; filename=\"\"", None),
            ("", None),
        ];
        for (header, expected) in cases {
            assert_eq!(parse_filename(header).as_deref(), expected, "{header}");
        }
    }

    #[test]
    fn share_links_count_plain_or_as_one_base64_block_and_nothing_else_does() {
        let links =
            "ss://YWVzLTI1Ni1nY206cGFzcw@1.2.3.4:8388#a\ntrojan://pw@5.6.7.8:443#b\n\nnot a link\n";
        assert_eq!(link_count(links), 2);
        for engine in [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD] {
            // With the line breaks some servers wrap base64 at.
            let wrapped = engine
                .encode(links)
                .as_bytes()
                .chunks(16)
                .map(|chunk| std::str::from_utf8(chunk).unwrap())
                .collect::<Vec<_>>()
                .join("\r\n");
            assert_eq!(link_count(&wrapped), 2, "{wrapped}");
        }
        assert_eq!(link_count("VLESS://u@h:1#upper"), 1);
        for text in [
            "",
            "this is not a subscription",
            "http://example.com/page",
            "proxies: []",
            // Base64, of something that is not links.
            "aGVsbG8gd29ybGQ=",
            // Not UTF-8 once decoded.
            "/////w==",
        ] {
            assert_eq!(link_count(text), 0, "{text:?}");
        }
    }

    #[test]
    fn the_first_complaint_of_the_kernel_is_what_is_reported() {
        let output = "time=\"2026-11-06T18:10:07+08:00\" level=info msg=\"Start\"\n\
            time=\"2026-11-06T18:10:07+08:00\" level=error msg=\"yaml: line 1: did not find \\\"expected\\\" node\\n  more\"\n\
            configuration file x test failed\n";
        assert_eq!(
            first_error(output),
            "yaml: line 1: did not find \"expected\" node\n  more"
        );
        // No log line to read: what there is.
        assert_eq!(
            first_error("\n  boom happened\nsecond\n"),
            "  boom happened"
        );
        assert_eq!(first_error(""), "");
    }

    fn sub(id: &str, kind: Kind, auto: bool, hours: u32, updated_at: i64) -> Subscription {
        Subscription {
            id: id.to_owned(),
            name: id.to_owned(),
            kind,
            source: "https://x.test".to_owned(),
            auto_update: auto,
            interval_hours: hours,
            updated_at,
            usage: None,
            age_key: None,
        }
    }

    #[test]
    fn a_subscription_is_due_once_its_interval_has_passed_and_only_if_it_may_be_updated_alone() {
        const HOUR: i64 = 3600;
        let store = Store {
            active: None,
            subscriptions: vec![
                sub("on-time", Kind::Url, true, 24, 1_000_000),
                sub("off", Kind::Url, false, 1, 0),
                sub("file", Kind::File, true, 1, 0),
                sub("never", Kind::Url, true, 24, 0),
                // An interval of 0 is no reason to ask every minute.
                sub("zero", Kind::Url, true, 0, 1_000_000),
            ],
        };
        let due =
            |now: i64| -> Vec<&str> { store.due(now).iter().map(|sub| sub.id.as_str()).collect() };
        // Just short of 24 hours after the last: only the one never updated.
        assert_eq!(due(1_000_000 + 24 * HOUR - 1), ["never", "zero"]);
        // On the hour it is due, as the one that asks for an hour is.
        assert_eq!(due(1_000_000 + 24 * HOUR), ["on-time", "never", "zero"]);
        // A zero interval is an hour.
        assert!(!due(1_000_000 + HOUR - 1).contains(&"zero"));
        assert!(due(1_000_000 + HOUR).contains(&"zero"));
        // Off, and files, never.
        assert!(!due(i64::MAX / 2).contains(&"off") && !due(i64::MAX / 2).contains(&"file"));
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tabby-{}-subs-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("subscriptions.toml")
    }

    #[test]
    fn the_list_round_trips_through_a_private_file_and_a_missing_one_is_empty() {
        use std::os::unix::fs::PermissionsExt;
        let path = scratch("store");
        assert_eq!(Store::load(&path).unwrap(), Store::default());

        let store = Store {
            active: Some("b".to_owned()),
            subscriptions: vec![
                Subscription {
                    id: "a".to_owned(),
                    name: "机场 \"A\"".to_owned(),
                    kind: Kind::Url,
                    source: "https://example.test/sub?token=secret".to_owned(),
                    auto_update: false,
                    interval_hours: 6,
                    updated_at: 1_700_000_000,
                    usage: Some(Usage {
                        upload: 1,
                        download: 2,
                        total: 3,
                        expire: 4,
                    }),
                    age_key: Some("AGE-SECRET-KEY-1QQQ".to_owned()),
                },
                Subscription {
                    id: "b".to_owned(),
                    name: "local".to_owned(),
                    kind: Kind::File,
                    source: "/tmp/x.yaml".to_owned(),
                    auto_update: true,
                    interval_hours: DEFAULT_INTERVAL_HOURS,
                    updated_at: 0,
                    usage: None,
                    age_key: None,
                },
            ],
        };
        store.save(&path).unwrap();
        assert_eq!(Store::load(&path).unwrap(), store);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(store.active().map(|sub| sub.name.as_str()), Some("local"));

        // A file written by hand with only what it needs gets the defaults.
        fs::write(
            &path,
            "[[subscription]]\nid = \"c\"\nname = \"c\"\nkind = \"url\"\nsource = \"https://x.test\"\n",
        )
        .unwrap();
        let loaded = Store::load(&path).unwrap();
        assert!(loaded.subscriptions[0].auto_update);
        assert_eq!(
            loaded.subscriptions[0].interval_hours,
            DEFAULT_INTERVAL_HOURS
        );
        assert_eq!(loaded.active, None);

        // One that does not parse is an error, not an empty list that the
        // next save would write over.
        fs::write(&path, "[[subscription]]\nid = 1\n").unwrap();
        assert_eq!(
            Store::load(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
