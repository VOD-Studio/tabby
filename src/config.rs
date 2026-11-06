use std::{
    collections::BTreeMap,
    fs, io,
    path::{self, Path, PathBuf},
};

use clap::Args;
use color_eyre::{
    Result,
    eyre::{WrapErr, eyre},
};
use directories::ProjectDirs;
use ratatui::style::Color;
use serde::{Deserialize, Deserializer, de};

pub const DEFAULT_URL: &str = "http://127.0.0.1:9090";

/// Page fetched through a node to measure its delay.
pub const DEFAULT_DELAY_URL: &str = "https://www.gstatic.com/generate_204";

/// How long a delay test may take before the node counts as failed.
pub const DEFAULT_DELAY_TIMEOUT_MS: u32 = 5000;

/// Name shown for the connection when no profile is selected.
const FALLBACK_PROFILE: &str = "default";

/// A theme built into tabby. User themes are files; see `crate::themes`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThemeName {
    #[default]
    Terminal,
    TokyoNight,
    CatppuccinMocha,
    CatppuccinLatte,
    GruvboxDark,
    Nord,
    Dracula,
}

impl ThemeName {
    /// Every built-in theme, in the order the theme picker lists them.
    pub const ALL: [Self; 7] = [
        Self::Terminal,
        Self::TokyoNight,
        Self::CatppuccinMocha,
        Self::CatppuccinLatte,
        Self::GruvboxDark,
        Self::Nord,
        Self::Dracula,
    ];

    /// What `theme =` and `--theme` call it.
    pub fn id(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::TokyoNight => "tokyo-night",
            Self::CatppuccinMocha => "catppuccin-mocha",
            Self::CatppuccinLatte => "catppuccin-latte",
            Self::GruvboxDark => "gruvbox-dark",
            Self::Nord => "nord",
            Self::Dracula => "dracula",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|name| name.id() == id)
    }

    /// Every id, comma-separated, for error messages.
    pub fn ids() -> String {
        Self::ALL.map(Self::id).join(", ")
    }
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

/// Single semantic colors (plan §7.1) laid over a theme: the `[colors]`
/// table of a theme file, and `[theme_overrides]` over whichever theme is
/// active. A key that is not there leaves the color underneath.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Palette {
    pub fg: Option<Color>,
    pub muted: Option<Color>,
    /// The cursor row's background; see `Theme::with`.
    pub surface: Option<Color>,
    /// Background of every other table row; `reset` for none.
    pub stripe: Option<Color>,
    /// Background of the whole screen; `reset` for the terminal's own.
    pub bg: Option<Color>,
    pub accent: Option<Color>,
    pub success: Option<Color>,
    pub warning: Option<Color>,
    pub error: Option<Color>,
    pub c1: Option<Color>,
    pub c2: Option<Color>,
    pub c3: Option<Color>,
    pub c4: Option<Color>,
}

/// The keys of a [`Palette`] table.
const PALETTE_KEYS: [&str; 13] = [
    "fg", "muted", "surface", "stripe", "bg", "accent", "success", "warning", "error", "c1", "c2",
    "c3", "c4",
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

impl Palette {
    fn slot(&mut self, key: &str) -> Option<&mut Option<Color>> {
        // Exhaustive, so a color added to the struct cannot be left out.
        let Self {
            fg,
            muted,
            surface,
            stripe,
            bg,
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
            "stripe" => stripe,
            "bg" => bg,
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

    /// Reads `table`, which error messages call `context`. Every error names
    /// the key it is about.
    pub fn from_table(table: &toml::Table, context: &str) -> Result<Self, String> {
        let mut palette = Self::default();
        for (key, value) in table {
            let Some(slot) = palette.slot(key) else {
                return Err(format!(
                    "unknown key `{key}` in {context}, expected one of {}",
                    PALETTE_KEYS.join(", ")
                ));
            };
            let toml::Value::String(text) = value else {
                return Err(format!(
                    "{context} `{key}` must be a string, found {}",
                    value.type_str()
                ));
            };
            let color = parse_color(text).filter(|color| {
                // A background that is the terminal's own would hide the
                // cursor row.
                key != "surface" || *color != Color::Reset
            });
            let Some(color) = color else {
                let names = ANSI_COLORS.map(|(name, _)| name).join(", ");
                let reset = if key == "surface" { "" } else { ", reset" };
                return Err(format!(
                    "{context} `{key}`: invalid color `{text}`, expected #rrggbb or one of {names}{reset}"
                ));
            };
            *slot = Some(color);
        }
        Ok(palette)
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

/// Contents of `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    default: Option<String>,
    theme: Option<String>,
    #[serde(deserialize_with = "theme_overrides")]
    theme_overrides: Palette,
    icons: IconSet,
    delay: DelayConfig,
    profiles: BTreeMap<String, Profile>,
    /// The directory the file was read from: a relative theme path in it
    /// is relative to there.
    #[serde(skip)]
    dir: Option<PathBuf>,
}

/// Reads `[theme_overrides]`, naming the key in every error: the position
/// TOML reports for a table value is not the line.
fn theme_overrides<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Palette, D::Error> {
    let table = toml::Table::deserialize(deserializer)?;
    Palette::from_table(&table, "[theme_overrides]").map_err(de::Error::custom)
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
    /// Color theme: a built-in name, the name of a file in the themes
    /// directory, or the path of a theme file
    #[arg(long, env = "TABBY_THEME")]
    pub theme: Option<String>,
}

/// The connection and look tabby will use, after applying all overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub profile: String,
    pub url: String,
    pub secret: Option<String>,
    /// The theme as `theme =` names it: a built-in name, a theme in the
    /// themes directory, or the absolute path of a theme file.
    pub theme: String,
    /// Whether `--theme` or `TABBY_THEME` chose the theme, so a theme saved
    /// to `config.toml` does not apply at the next start.
    pub theme_pinned: bool,
    pub theme_overrides: Palette,
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

/// Where user themes live, next to `config.toml`; `None` when the home
/// directory is unknown.
pub fn themes_dir() -> Option<PathBuf> {
    ProjectDirs::from("", "", "tabby").map(|dirs| dirs.config_dir().join("themes"))
}

/// Whether the theme `id` is the path of a file rather than a name.
pub fn is_theme_path(id: &str) -> bool {
    id.contains('/') || id.contains(path::MAIN_SEPARATOR) || is_toml(Path::new(id))
}

/// Whether `path` names a `.toml` file, in any case.
pub fn is_toml(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
}

/// `id` as [`Settings::theme`] keeps it: a name as it is, a path made
/// absolute against `base` (the current directory when `None`), so it
/// means the same wherever it is saved.
fn theme_id(id: &str, base: Option<&Path>) -> String {
    if !is_theme_path(id) {
        return id.to_owned();
    }
    let joined = base.map_or_else(|| PathBuf::from(id), |base| base.join(id));
    path::absolute(&joined)
        .unwrap_or(joined)
        .to_string_lossy()
        .into_owned()
}

/// Sets `theme = "<id>"` in the `config.toml` at `path`, creating the file
/// if there is none. Everything else, comments included, stays as written.
pub fn save_theme(path: &Path, id: &str) -> Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(err).wrap_err_with(|| format!("cannot read {}", path.display()));
        }
    };
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .wrap_err_with(|| format!("invalid config file {}", path.display()))?;
    match document
        .get_mut("theme")
        .and_then(toml_edit::Item::as_value_mut)
    {
        // The comment after the old value stays after the new one.
        Some(value) => {
            let decor = value.decor().clone();
            *value = id.into();
            *value.decor_mut() = decor;
        }
        None => document["theme"] = toml_edit::value(id),
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).wrap_err_with(|| format!("cannot create {}", dir.display()))?;
    }
    fs::write(path, document.to_string())
        .wrap_err_with(|| format!("cannot write {}", path.display()))
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
                .map(|config| Self {
                    dir: path.parent().map(Path::to_owned),
                    ..config
                })
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
    /// An empty override (`TABBY_SECRET=`) counts as unset. A theme path is
    /// made absolute: against the current directory when it is an
    /// override, against the file's directory when the file names it.
    pub fn resolve(&self, overrides: &Overrides) -> Result<Settings> {
        let chosen = self.chosen(overrides)?;
        let profile = chosen.map(|(_, profile)| profile);
        let (theme, theme_pinned) = match non_empty(overrides.theme.as_ref()) {
            Some(id) => (theme_id(id, None), true),
            None => (
                self.theme.as_deref().map_or_else(
                    || ThemeName::default().id().to_owned(),
                    |id| theme_id(id, self.dir.as_deref()),
                ),
                false,
            ),
        };
        Ok(Settings {
            profile: chosen.map_or(FALLBACK_PROFILE, |(name, _)| name).to_owned(),
            url: non_empty(overrides.url.as_ref())
                .or_else(|| profile.map(|p| p.url.as_str()))
                .unwrap_or(DEFAULT_URL)
                .to_owned(),
            secret: non_empty(overrides.secret.as_ref())
                .or_else(|| profile.and_then(|p| p.secret.as_deref()))
                .map(str::to_owned),
            theme,
            theme_pinned,
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
    fn file_overrides() -> Palette {
        Palette {
            accent: Some(Color::Rgb(0x7a, 0xa2, 0xf7)),
            ..Palette::default()
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
                theme: "terminal".into(),
                theme_pinned: false,
                theme_overrides: Palette::default(),
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
                theme: "tokyo-night".into(),
                theme_pinned: false,
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
            theme: Some("catppuccin-mocha".into()),
        };
        let settings = file().resolve(&overrides).unwrap();
        assert_eq!(
            settings,
            Settings {
                profile: "router".into(),
                url: "http://cli:1".into(),
                secret: Some("cli-secret".into()),
                theme: "catppuccin-mocha".into(),
                theme_pinned: true,
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

    fn overrides_of(text: &str) -> Result<Palette, String> {
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
stripe = "reset"
bg = "#101112"
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
            Palette {
                fg: Some(Color::Rgb(1, 2, 3)),
                muted: Some(Color::DarkGray),
                surface: Some(Color::Rgb(0x0a, 0x0b, 0x0c)),
                stripe: Some(Color::Reset),
                bg: Some(Color::Rgb(0x10, 0x11, 0x12)),
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
        assert_eq!(overrides_of("").unwrap(), Palette::default());
        assert_eq!(
            overrides_of("[theme_overrides]").unwrap(),
            Palette::default()
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
        for key in PALETTE_KEYS {
            assert!(err.contains(key), "{key}: {err}");
        }
        // The key is what is wrong, whatever the value.
        let err = overrides_of("[theme_overrides]\nborder = \"nope\"\n").unwrap_err();
        assert!(
            err.contains("`border`") && err.contains("unknown key"),
            "{err}"
        );
    }

    #[test]
    fn reset_is_a_color_but_not_a_cursor_row_background() {
        let palette =
            overrides_of("[theme_overrides]\nfg = \"reset\"\nstripe = \"reset\"\nbg = \"reset\"\n")
                .unwrap();
        assert_eq!(
            (palette.fg, palette.stripe, palette.bg),
            (Some(Color::Reset), Some(Color::Reset), Some(Color::Reset))
        );
        let err = overrides_of("[theme_overrides]\nsurface = \"reset\"\n").unwrap_err();
        assert!(err.contains("`surface`"), "{err}");
    }

    #[test]
    #[cfg(unix)]
    fn a_theme_path_is_made_absolute_where_it_was_written_and_a_name_is_kept() {
        let config = |theme: &str| Config {
            theme: Some(theme.to_owned()),
            dir: Some(PathBuf::from("/etc/tabby")),
            ..Config::default()
        };
        let theme = |config: &Config, overrides: &Overrides| {
            let settings = config.resolve(overrides).unwrap();
            (settings.theme, settings.theme_pinned)
        };
        let none = Overrides::default();
        // Relative to `config.toml`, whatever the current directory.
        assert_eq!(
            theme(&config("mine/dark.toml"), &none),
            ("/etc/tabby/mine/dark.toml".to_owned(), false)
        );
        assert_eq!(
            theme(&config("/abs/x.toml"), &none),
            ("/abs/x.toml".to_owned(), false)
        );
        assert_eq!(theme(&config("nord"), &none), ("nord".to_owned(), false));
        // From the command line, relative to the current directory.
        let cli = Overrides {
            theme: Some("dark.toml".into()),
            ..Overrides::default()
        };
        let cwd = std::env::current_dir().unwrap().join("dark.toml");
        assert_eq!(
            theme(&config("nord"), &cli),
            (cwd.to_string_lossy().into_owned(), true)
        );
        // An empty override is no override.
        let empty = Overrides {
            theme: Some(String::new()),
            ..Overrides::default()
        };
        assert_eq!(theme(&config("nord"), &empty), ("nord".to_owned(), false));
    }

    /// A directory of its own under the system's temporary one, emptied.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tabby-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn saving_a_theme_changes_that_value_alone() {
        let dir = scratch("save-theme");
        let path = dir.join("config.toml");
        // No file, not even its directory: both are made.
        save_theme(&path, "nord").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "theme = \"nord\"\n");

        let written = "# mine\ndefault = \"a\"\ntheme = \"terminal\"   # was\n\n[profiles.a]\nurl = \"http://a:1\"   # router\n";
        fs::write(&path, written).unwrap();
        save_theme(&path, "/home/me/my theme.toml").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            written.replace("\"terminal\"", "\"/home/me/my theme.toml\"")
        );
        // The file reads back with the new theme.
        let config = Config::load(Some(&path)).unwrap();
        assert_eq!(config.theme.as_deref(), Some("/home/me/my theme.toml"));

        // Without a `theme`, it goes among the top-level keys, before the
        // tables.
        fs::write(&path, "[profiles.a]\nurl = \"http://a:1\"\n").unwrap();
        save_theme(&path, "dracula").unwrap();
        let config = Config::load(Some(&path)).unwrap();
        assert_eq!(config.theme.as_deref(), Some("dracula"));
        assert_eq!(config.profiles.len(), 1);

        // A file that does not parse is left alone.
        fs::write(&path, "theme = \n").unwrap();
        assert!(save_theme(&path, "nord").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "theme = \n");
        fs::remove_dir_all(&dir).unwrap();
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
