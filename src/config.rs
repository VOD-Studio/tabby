use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use clap::{Args, ValueEnum};
use color_eyre::{
    Result,
    eyre::{WrapErr, eyre},
};
use directories::ProjectDirs;
use ratatui::style::Color;
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, Visitor},
};

pub const DEFAULT_URL: &str = "http://127.0.0.1:9090";

/// Page fetched through a node to measure its delay.
pub const DEFAULT_DELAY_URL: &str = "https://www.gstatic.com/generate_204";

/// How long a delay test may take before the node counts as failed.
pub const DEFAULT_DELAY_TIMEOUT_MS: u32 = 5000;

/// Name shown for the connection when no profile is selected.
const FALLBACK_PROFILE: &str = "default";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeName {
    #[default]
    Terminal,
    TokyoNight,
    CatppuccinMocha,
}

/// The `icons` setting: which symbols the UI draws (plan §7.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IconSet {
    /// Symbols any font has.
    #[default]
    Unicode,
    /// Nerd Fonts v3 glyphs, for a terminal font patched with them.
    Nerd,
}

/// The `[theme_overrides]` table: single semantic colors (plan §7.1) laid
/// over whichever theme is active. A key that is not there leaves the
/// theme's own color.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThemeOverrides {
    pub fg: Option<Color>,
    pub muted: Option<Color>,
    /// The cursor row's background; see `Theme::with_overrides`.
    pub surface: Option<Color>,
    pub accent: Option<Color>,
    pub success: Option<Color>,
    pub warning: Option<Color>,
    pub error: Option<Color>,
    pub c1: Option<Color>,
    pub c2: Option<Color>,
    pub c3: Option<Color>,
    pub c4: Option<Color>,
}

/// The keys of `[theme_overrides]`.
const OVERRIDE_KEYS: [&str; 11] = [
    "fg", "muted", "surface", "accent", "success", "warning", "error", "c1", "c2", "c3", "c4",
];

/// The 16 ANSI colors by the kebab-case form of ratatui's names: the
/// terminal's own palette decides what each looks like.
const ANSI_COLORS: [(&str, Color); 16] = [
    ("black", Color::Black),
    ("red", Color::Red),
    ("green", Color::Green),
    ("yellow", Color::Yellow),
    ("blue", Color::Blue),
    ("magenta", Color::Magenta),
    ("cyan", Color::Cyan),
    ("gray", Color::Gray),
    ("dark-gray", Color::DarkGray),
    ("light-red", Color::LightRed),
    ("light-green", Color::LightGreen),
    ("light-yellow", Color::LightYellow),
    ("light-blue", Color::LightBlue),
    ("light-magenta", Color::LightMagenta),
    ("light-cyan", Color::LightCyan),
    ("white", Color::White),
];

impl ThemeOverrides {
    fn slot(&mut self, key: &str) -> Option<&mut Option<Color>> {
        // Exhaustive, so a color added to the struct cannot be left out.
        let Self {
            fg,
            muted,
            surface,
            accent,
            success,
            warning,
            error,
            c1,
            c2,
            c3,
            c4,
        } = self;
        Some(match key {
            "fg" => fg,
            "muted" => muted,
            "surface" => surface,
            "accent" => accent,
            "success" => success,
            "warning" => warning,
            "error" => error,
            "c1" => c1,
            "c2" => c2,
            "c3" => c3,
            "c4" => c4,
            _ => return None,
        })
    }
}

/// `#rrggbb` (either case), one of the 16 ANSI names, or `reset` for the
/// terminal's own default color.
fn parse_color(text: &str) -> Option<Color> {
    if let Some(hex) = text.strip_prefix('#') {
        if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let rgb = u32::from_str_radix(hex, 16).ok()?;
        let [_, red, green, blue] = rgb.to_be_bytes();
        return Some(Color::Rgb(red, green, blue));
    }
    if text == "reset" {
        return Some(Color::Reset);
    }
    ANSI_COLORS
        .iter()
        .find_map(|&(name, color)| (name == text).then_some(color))
}

impl<'de> Deserialize<'de> for ThemeOverrides {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Table;

        impl<'de> Visitor<'de> for Table {
            type Value = ThemeOverrides;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a table of colors")
            }

            // Hand-written so that every error can name the key: the
            // position TOML reports for a table value is not the line.
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut overrides = ThemeOverrides::default();
                while let Some(key) = map.next_key::<String>()? {
                    let value = map.next_value::<toml::Value>()?;
                    let Some(slot) = overrides.slot(&key) else {
                        return Err(de::Error::custom(format!(
                            "unknown key `{key}` in [theme_overrides], expected one of {}",
                            OVERRIDE_KEYS.join(", ")
                        )));
                    };
                    let toml::Value::String(text) = &value else {
                        return Err(de::Error::custom(format!(
                            "[theme_overrides] `{key}` must be a string, found {}",
                            value.type_str()
                        )));
                    };
                    let color = parse_color(text).filter(|color| {
                        // A background that is the terminal's own would hide
                        // the cursor row.
                        key != "surface" || *color != Color::Reset
                    });
                    let Some(color) = color else {
                        let names = ANSI_COLORS.map(|(name, _)| name).join(", ");
                        let reset = if key == "surface" { "" } else { ", reset" };
                        return Err(de::Error::custom(format!(
                            "[theme_overrides] `{key}`: invalid color `{text}`, expected #rrggbb or one of {names}{reset}"
                        )));
                    };
                    *slot = Some(color);
                }
                Ok(overrides)
            }
        }

        deserializer.deserialize_map(Table)
    }
}

/// Contents of `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    default: Option<String>,
    theme: Option<ThemeName>,
    theme_overrides: ThemeOverrides,
    icons: IconSet,
    delay: DelayConfig,
    profiles: BTreeMap<String, Profile>,
}

/// The `[delay]` table.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DelayConfig {
    url: Option<String>,
    /// Milliseconds.
    timeout_ms: Option<u32>,
}

/// One External Controller endpoint.
#[derive(Debug, Deserialize)]
pub struct Profile {
    url: String,
    #[serde(default)]
    secret: Option<String>,
}

/// Command-line / environment overrides. They take priority over `config.toml`.
#[derive(Debug, Default, Args)]
pub struct Overrides {
    /// Profile from config.toml to connect with
    #[arg(long, env = "TABBY_PROFILE")]
    pub profile: Option<String>,
    /// External Controller URL
    #[arg(long, env = "TABBY_URL")]
    pub url: Option<String>,
    /// External Controller secret
    #[arg(long, env = "TABBY_SECRET", hide_env_values = true)]
    pub secret: Option<String>,
    /// Color theme
    #[arg(long, env = "TABBY_THEME", value_enum)]
    pub theme: Option<ThemeName>,
}

/// The connection and look tabby will use, after applying all overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub profile: String,
    pub url: String,
    pub secret: Option<String>,
    pub theme: ThemeName,
    pub theme_overrides: ThemeOverrides,
    pub icons: IconSet,
    pub delay_url: String,
    pub delay_timeout_ms: u32,
}

/// Where a connection the profile popup offers comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A `[profiles.<name>]` table of `config.toml`.
    Profile,
    /// The `--url` / `--secret` overrides (or their variables) tabby was
    /// started with.
    Overrides,
    /// No profile was chosen at start: the built-in address.
    Default,
}

/// One connection the profile popup offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub origin: Origin,
    /// What the header calls the connection.
    pub name: String,
    pub url: String,
    pub secret: Option<String>,
}

impl Choice {
    /// What the popup and the switch toast call it.
    pub fn label(&self) -> &str {
        match self.origin {
            Origin::Profile => &self.name,
            Origin::Overrides => "启动参数指定",
            Origin::Default => "默认地址",
        }
    }
}

/// The connections tabby can switch between at runtime.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profiles {
    /// The connection tabby started with first, when no profile describes
    /// it as written; then every profile, by name.
    pub choices: Vec<Choice>,
    /// Index into `choices` of the connection in use.
    pub current: usize,
    /// `config.toml`, for the popup to point at when it defines no profile.
    pub path: Option<PathBuf>,
}

impl Profiles {
    /// Whether `config.toml` defines no profile to switch to.
    pub fn is_empty(&self) -> bool {
        !self
            .choices
            .iter()
            .any(|choice| choice.origin == Origin::Profile)
    }
}

/// Where `config.toml` lives; `None` when the home directory is unknown.
pub fn path() -> Option<PathBuf> {
    ProjectDirs::from("", "", "tabby").map(|dirs| dirs.config_dir().join("config.toml"))
}

impl Config {
    /// Loads `config.toml` from `path`.
    ///
    /// A missing file (or no path, for an unknown home directory) yields
    /// the defaults.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .wrap_err_with(|| format!("invalid config file {}", path.display())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => {
                Err(err).wrap_err_with(|| format!("cannot read config file {}", path.display()))
            }
        }
    }

    /// The profile `overrides` pick: `overrides.profile`, else the file's
    /// `default`, else none.
    fn chosen(&self, overrides: &Overrides) -> Result<Option<(&str, &Profile)>> {
        let name = non_empty(overrides.profile.as_ref()).or(self.default.as_deref());
        name.map(|name| {
            self.profiles
                .get_key_value(name)
                .map(|(name, profile)| (name.as_str(), profile))
                .ok_or_else(|| eyre!("profile `{name}` is not defined in config.toml"))
        })
        .transpose()
    }

    /// Merges `overrides` over the file over the built-in defaults.
    ///
    /// The profile is `overrides.profile`, else the file's `default`, else none;
    /// an explicit `url` / `secret` override beats the profile's own value.
    /// An empty override (`TABBY_SECRET=`) counts as unset.
    pub fn resolve(&self, overrides: &Overrides) -> Result<Settings> {
        let chosen = self.chosen(overrides)?;
        let profile = chosen.map(|(_, profile)| profile);
        Ok(Settings {
            profile: chosen.map_or(FALLBACK_PROFILE, |(name, _)| name).to_owned(),
            url: non_empty(overrides.url.as_ref())
                .or_else(|| profile.map(|p| p.url.as_str()))
                .unwrap_or(DEFAULT_URL)
                .to_owned(),
            secret: non_empty(overrides.secret.as_ref())
                .or_else(|| profile.and_then(|p| p.secret.as_deref()))
                .map(str::to_owned),
            theme: overrides.theme.or(self.theme).unwrap_or_default(),
            theme_overrides: self.theme_overrides,
            icons: self.icons,
            delay_url: self
                .delay
                .url
                .clone()
                .filter(|url| !url.is_empty())
                .unwrap_or_else(|| DEFAULT_DELAY_URL.to_owned()),
            delay_timeout_ms: self.delay.timeout_ms.unwrap_or(DEFAULT_DELAY_TIMEOUT_MS),
        })
    }

    /// What the profile popup offers, given the `settings` that
    /// [`Config::resolve`] made of the same `overrides`.
    ///
    /// The overrides only shaped the connection tabby started with: a
    /// profile switched to connects with its own url and secret. So that
    /// first connection is offered on its own, ahead of the profiles,
    /// whenever a `url` or `secret` override shaped it, or no profile did.
    pub fn profiles(&self, overrides: &Overrides, settings: &Settings) -> Profiles {
        // `resolve` has already failed on an unknown profile.
        let chosen = self.chosen(overrides).ok().flatten().map(|(name, _)| name);
        let overridden = non_empty(overrides.url.as_ref()).is_some()
            || non_empty(overrides.secret.as_ref()).is_some();
        let launch = match (overridden, chosen) {
            (true, _) => Some(Origin::Overrides),
            (false, None) => Some(Origin::Default),
            (false, Some(_)) => None,
        }
        .map(|origin| Choice {
            origin,
            name: settings.profile.clone(),
            url: settings.url.clone(),
            secret: settings.secret.clone(),
        });
        let current = match (&launch, chosen) {
            (None, Some(name)) => self.profiles.keys().position(|key| key == name),
            _ => None,
        }
        .unwrap_or(0);
        let choices = launch
            .into_iter()
            .chain(self.profiles.iter().map(|(name, profile)| Choice {
                origin: Origin::Profile,
                name: name.clone(),
                url: profile.url.clone(),
                secret: profile.secret.clone(),
            }))
            .collect();
        Profiles {
            choices,
            current,
            path: None,
        }
    }
}

fn non_empty(value: Option<&String>) -> Option<&str> {
    value.map(String::as_str).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r##"
default = "local"
theme = "tokyo-night"
icons = "nerd"

[delay]
url = "http://delay.test/204"
timeout_ms = 2500

[profiles.local]
url = "http://local:9090"
secret = "file-secret"

[profiles.router]
url = "http://router:9090"

[theme_overrides]
accent = "#7aa2f7"
"##;

    /// What `[theme_overrides]` of [`FILE`] says.
    fn file_overrides() -> ThemeOverrides {
        ThemeOverrides {
            accent: Some(Color::Rgb(0x7a, 0xa2, 0xf7)),
            ..ThemeOverrides::default()
        }
    }

    fn file() -> Config {
        toml::from_str(FILE).unwrap()
    }

    #[test]
    fn defaults_apply_without_file_or_overrides() {
        let settings = Config::default().resolve(&Overrides::default()).unwrap();
        assert_eq!(
            settings,
            Settings {
                profile: "default".into(),
                url: DEFAULT_URL.into(),
                secret: None,
                theme: ThemeName::Terminal,
                theme_overrides: ThemeOverrides::default(),
                icons: IconSet::Unicode,
                delay_url: DEFAULT_DELAY_URL.into(),
                delay_timeout_ms: DEFAULT_DELAY_TIMEOUT_MS,
            }
        );
    }

    #[test]
    fn file_beats_defaults() {
        let settings = file().resolve(&Overrides::default()).unwrap();
        assert_eq!(
            settings,
            Settings {
                profile: "local".into(),
                url: "http://local:9090".into(),
                secret: Some("file-secret".into()),
                theme: ThemeName::TokyoNight,
                theme_overrides: file_overrides(),
                icons: IconSet::Nerd,
                delay_url: "http://delay.test/204".into(),
                delay_timeout_ms: 2500,
            }
        );
    }

    #[test]
    fn overrides_beat_file() {
        let overrides = Overrides {
            profile: Some("router".into()),
            url: Some("http://cli:1".into()),
            secret: Some("cli-secret".into()),
            theme: Some(ThemeName::CatppuccinMocha),
        };
        let settings = file().resolve(&overrides).unwrap();
        assert_eq!(
            settings,
            Settings {
                profile: "router".into(),
                url: "http://cli:1".into(),
                secret: Some("cli-secret".into()),
                theme: ThemeName::CatppuccinMocha,
                // Not tied to the theme they were written under.
                theme_overrides: file_overrides(),
                icons: IconSet::Nerd,
                delay_url: "http://delay.test/204".into(),
                delay_timeout_ms: 2500,
            }
        );
    }

    #[test]
    fn profile_override_does_not_inherit_default_profile_secret() {
        let overrides = Overrides {
            profile: Some("router".into()),
            ..Overrides::default()
        };
        let settings = file().resolve(&overrides).unwrap();
        assert_eq!(settings.url, "http://router:9090");
        assert_eq!(settings.secret, None);
    }

    #[test]
    fn unknown_profile_is_an_error() {
        let overrides = Overrides {
            profile: Some("nope".into()),
            ..Overrides::default()
        };
        assert!(file().resolve(&overrides).is_err());
    }

    #[test]
    fn icons_are_read_by_name_and_an_unknown_one_is_an_error() {
        let icons = |text: &str| toml::from_str::<Config>(text).map(|config| config.icons);
        assert_eq!(icons("").unwrap(), IconSet::Unicode);
        assert_eq!(icons(r#"icons = "unicode""#).unwrap(), IconSet::Unicode);
        assert_eq!(icons(r#"icons = "nerd""#).unwrap(), IconSet::Nerd);
        let err = icons(r#"icons = "emoji""#).unwrap_err().to_string();
        assert!(err.contains("icons"), "{err}");
    }

    fn overrides_of(text: &str) -> Result<ThemeOverrides, String> {
        toml::from_str::<Config>(text)
            .map(|config| config.theme_overrides)
            .map_err(|err| err.to_string())
    }

    #[test]
    fn every_semantic_color_can_be_overridden_by_hex_or_ansi_name() {
        let all = overrides_of(
            r##"
[theme_overrides]
fg = "#010203"
muted = "dark-gray"
surface = "#0A0b0C"
accent = "#7aa2f7"
success = "light-green"
warning = "yellow"
error = "#FF0000"
c1 = "light-magenta"
c2 = "white"
c3 = "black"
c4 = "gray"
"##,
        )
        .unwrap();
        assert_eq!(
            all,
            ThemeOverrides {
                fg: Some(Color::Rgb(1, 2, 3)),
                muted: Some(Color::DarkGray),
                surface: Some(Color::Rgb(0x0a, 0x0b, 0x0c)),
                accent: Some(Color::Rgb(0x7a, 0xa2, 0xf7)),
                success: Some(Color::LightGreen),
                warning: Some(Color::Yellow),
                error: Some(Color::Rgb(255, 0, 0)),
                c1: Some(Color::LightMagenta),
                c2: Some(Color::White),
                c3: Some(Color::Black),
                c4: Some(Color::Gray),
            }
        );
        // Without the table, or with it empty, nothing is overridden.
        assert_eq!(overrides_of("").unwrap(), ThemeOverrides::default());
        assert_eq!(
            overrides_of("[theme_overrides]").unwrap(),
            ThemeOverrides::default()
        );
    }

    #[test]
    fn all_sixteen_ansi_names_parse_and_none_of_them_is_reset() {
        assert_eq!(ANSI_COLORS.len(), 16);
        for (name, color) in ANSI_COLORS {
            assert_eq!(parse_color(name), Some(color), "{name}");
            assert_ne!(color, Color::Reset, "{name}");
        }
        assert_eq!(parse_color("reset"), Some(Color::Reset));
    }

    #[test]
    fn a_bad_color_value_is_an_error_naming_the_key() {
        for value in [
            "#7aa2f",     // too short
            "#7aa2f7f",   // too long
            "#7aa2fg",    // not hex
            "#+aa2f7",    // `from_str_radix` would take the sign
            "7aa2f7",     // no `#`
            "#abc",       // no shorthand
            "light-gray", // not one of the 16
            "Light-Red",  // names are lowercase
            "light_red",
            "5",
            "",
        ] {
            let err = overrides_of(&format!("[theme_overrides]\nerror = \"{value}\"\n"))
                .expect_err(value);
            assert!(err.contains("`error`"), "{value}: {err}");
            assert!(err.contains("invalid color"), "{value}: {err}");
        }
        // Not even a string.
        let err = overrides_of("[theme_overrides]\naccent = 5\n").unwrap_err();
        assert!(err.contains("`accent` must be a string"), "{err}");
    }

    #[test]
    fn an_unknown_key_is_an_error_naming_it_and_the_valid_ones() {
        let err =
            overrides_of("[theme_overrides]\naccent = \"red\"\nacent = \"blue\"\n").unwrap_err();
        assert!(err.contains("`acent`"), "{err}");
        for key in OVERRIDE_KEYS {
            assert!(err.contains(key), "{key}: {err}");
        }
        // The key is what is wrong, whatever the value.
        let err = overrides_of("[theme_overrides]\nstripe = \"nope\"\n").unwrap_err();
        assert!(
            err.contains("`stripe`") && err.contains("unknown key"),
            "{err}"
        );
    }

    #[test]
    fn reset_is_a_color_but_not_a_cursor_row_background() {
        let fg = overrides_of("[theme_overrides]\nfg = \"reset\"\n").unwrap();
        assert_eq!(fg.fg, Some(Color::Reset));
        let err = overrides_of("[theme_overrides]\nsurface = \"reset\"\n").unwrap_err();
        assert!(err.contains("`surface`"), "{err}");
    }

    fn offered(config: &Config, overrides: &Overrides) -> Profiles {
        let settings = config.resolve(overrides).unwrap();
        config.profiles(overrides, &settings)
    }

    fn urls(profiles: &Profiles) -> Vec<(Origin, &str, Option<&str>)> {
        profiles
            .choices
            .iter()
            .map(|choice| (choice.origin, choice.url.as_str(), choice.secret.as_deref()))
            .collect()
    }

    #[test]
    fn url_override_is_offered_apart_from_the_profiles_it_shadows() {
        let overrides = Overrides {
            url: Some("http://cli:1".into()),
            secret: Some("cli-secret".into()),
            ..Overrides::default()
        };
        let profiles = offered(&file(), &overrides);
        assert_eq!(
            urls(&profiles),
            [
                (Origin::Overrides, "http://cli:1", Some("cli-secret")),
                // The profiles keep their own values: the override was for
                // the start only.
                (Origin::Profile, "http://local:9090", Some("file-secret")),
                (Origin::Profile, "http://router:9090", None),
            ]
        );
        assert_eq!(profiles.current, 0);
    }

    #[test]
    fn a_connection_made_by_a_profile_alone_is_that_profile() {
        let profiles = offered(&file(), &Overrides::default());
        assert_eq!(profiles.choices.len(), 2);
        assert_eq!(profiles.choices[profiles.current].name, "local");
        let overrides = Overrides {
            profile: Some("router".into()),
            ..Overrides::default()
        };
        let profiles = offered(&file(), &overrides);
        assert_eq!(profiles.choices[profiles.current].name, "router");
    }

    #[test]
    fn the_built_in_address_is_not_mistaken_for_a_profile_named_default() {
        let config: Config = toml::from_str(
            r#"
[profiles.default]
url = "http://elsewhere:9090"
"#,
        )
        .unwrap();
        let profiles = offered(&config, &Overrides::default());
        assert_eq!(
            urls(&profiles),
            [
                (Origin::Default, DEFAULT_URL, None),
                (Origin::Profile, "http://elsewhere:9090", None),
            ]
        );
        assert_eq!(profiles.current, 0);
    }
}
