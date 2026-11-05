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
use serde::Deserialize;

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

/// Contents of `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    default: Option<String>,
    theme: Option<ThemeName>,
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

    const FILE: &str = r#"
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
"#;

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
