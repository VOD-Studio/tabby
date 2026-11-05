use std::{collections::BTreeMap, fs, io};

use color_eyre::{
    Result,
    eyre::{WrapErr, eyre},
};
use directories::ProjectDirs;
use serde::Deserialize;

pub const DEFAULT_URL: &str = "http://127.0.0.1:9090";

/// Name shown for the connection when no profile is configured.
const FALLBACK_PROFILE: &str = "default";

/// Contents of `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    default: Option<String>,
    profiles: BTreeMap<String, Profile>,
}

/// One External Controller endpoint.
#[derive(Debug, Deserialize)]
pub struct Profile {
    url: String,
    #[serde(default)]
    secret: Option<String>,
}

/// The connection tabby will use, after applying defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub profile: String,
    pub url: String,
    pub secret: Option<String>,
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

    /// Resolves the connection settings: the `default` profile, or built-in
    /// defaults when none is set.
    pub fn resolve(&self) -> Result<Settings> {
        let Some(name) = &self.default else {
            return Ok(Settings {
                profile: FALLBACK_PROFILE.to_owned(),
                url: DEFAULT_URL.to_owned(),
                secret: None,
            });
        };
        let profile = self
            .profiles
            .get(name)
            .ok_or_else(|| eyre!("default profile `{name}` is not defined in config.toml"))?;
        Ok(Settings {
            profile: name.clone(),
            url: profile.url.clone(),
            secret: profile.secret.clone(),
        })
    }
}
