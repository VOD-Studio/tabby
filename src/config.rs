use std::{collections::BTreeMap, fs, io};

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

/// Contents of `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    default: Option<String>,
    theme: Option<ThemeName>,
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
    pub delay_url: String,
    pub delay_timeout_ms: u32,
}

impl Config {
    /// Loads `config.toml` from the platform config directory.
    ///
    /// A missing file (or an unknown home directory) yields the defaults.
    pub fn load() -> Result<Self> {
        let Some(dirs) = ProjectDirs::from("", "", "tabby") else {
            return Ok(Self::default());
        };
        let path = dirs.config_dir().join("config.toml");
        match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .wrap_err_with(|| format!("invalid config file {}", path.display())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => {
                Err(err).wrap_err_with(|| format!("cannot read config file {}", path.display()))
            }
        }
    }

    /// Merges `overrides` over the file over the built-in defaults.
    ///
    /// The profile is `overrides.profile`, else the file's `default`, else none;
    /// an explicit `url` / `secret` override beats the profile's own value.
    /// An empty override (`TABBY_SECRET=`) counts as unset.
    pub fn resolve(&self, overrides: &Overrides) -> Result<Settings> {
        let name = non_empty(overrides.profile.as_ref()).or(self.default.as_deref());
        let profile = name
            .map(|name| {
                self.profiles
                    .get(name)
                    .ok_or_else(|| eyre!("profile `{name}` is not defined in config.toml"))
            })
            .transpose()?;
        Ok(Settings {
            profile: name.unwrap_or(FALLBACK_PROFILE).to_owned(),
            url: non_empty(overrides.url.as_ref())
                .or_else(|| profile.map(|p| p.url.as_str()))
                .unwrap_or(DEFAULT_URL)
                .to_owned(),
            secret: non_empty(overrides.secret.as_ref())
                .or_else(|| profile.and_then(|p| p.secret.as_deref()))
                .map(str::to_owned),
            theme: overrides.theme.or(self.theme).unwrap_or_default(),
            delay_url: self
                .delay
                .url
                .clone()
                .filter(|url| !url.is_empty())
                .unwrap_or_else(|| DEFAULT_DELAY_URL.to_owned()),
            delay_timeout_ms: self.delay.timeout_ms.unwrap_or(DEFAULT_DELAY_TIMEOUT_MS),
        })
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
}
