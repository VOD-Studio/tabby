//! The themes tabby can draw with: the built-in ones, and theme files the
//! user writes (plan §7.1).
//!
//! A theme file starts from a built-in theme and changes some of its
//! colors:
//!
//! ```toml
//! extends = "tokyo-night"   # optional, `terminal` without it
//!
//! [colors]                  # the keys and values of `[theme_overrides]`
//! accent = "#ff9e64"
//! ```

use std::{
    fs,
    path::{Path, PathBuf},
};

use color_eyre::{Result, eyre::eyre};

use crate::{
    config::{self, Palette, ThemeName},
    ui::theme::Theme,
};

/// The theme `id` names, `[theme_overrides]` not applied: a built-in one,
/// else `<dir>/<id>.toml`. A path is read as it is.
pub fn load(id: &str, dir: Option<&Path>) -> Result<Theme> {
    if let Some(name) = ThemeName::from_id(id) {
        return Ok(name.into());
    }
    let path = if config::is_theme_path(id) {
        PathBuf::from(id)
    } else {
        let path = dir.map(|dir| file_of(dir, id));
        match path {
            Some(path) if path.is_file() => path,
            _ => {
                let looked = path.map_or_else(String::new, |path| {
                    format!(", and there is no {}", path.display())
                });
                return Err(eyre!(
                    "unknown theme `{id}`: the built-in ones are {}{looked}",
                    ThemeName::ids()
                ));
            }
        }
    };
    read(&path).map_err(|err| eyre!("invalid theme file {}: {err}", path.display()))
}

fn file_of(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.toml"))
}

/// The theme file at `path`, or what is wrong with it.
pub fn read(path: &Path) -> Result<Theme, String> {
    let text = fs::read_to_string(path).map_err(|err| err.to_string())?;
    parse(&text)
}

/// A theme file: the built-in theme `extends` names (`terminal` without
/// it) with `[colors]` laid over it. Every error names the key.
fn parse(text: &str) -> Result<Theme, String> {
    let table: toml::Table = toml::from_str(text).map_err(|err| err.to_string())?;
    let mut base = ThemeName::default();
    let mut palette = Palette::default();
    for (key, value) in &table {
        match (key.as_str(), value) {
            ("extends", toml::Value::String(id)) => {
                base = ThemeName::from_id(id).ok_or_else(|| {
                    format!(
                        "`extends`: `{id}` is not a built-in theme, expected one of {}",
                        ThemeName::ids()
                    )
                })?;
            }
            ("colors", toml::Value::Table(colors)) => {
                palette = Palette::from_table(colors, "[colors]")?;
            }
            ("extends", _) => {
                return Err(format!(
                    "`extends` must be a string, found {}",
                    value.type_str()
                ));
            }
            ("colors", _) => {
                return Err(format!(
                    "`colors` must be a table, found {}",
                    value.type_str()
                ));
            }
            _ => return Err(format!("unknown key `{key}`, expected extends or [colors]")),
        }
    }
    Ok(Theme::from(base).with(&palette))
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;

    /// A directory of its own under the system's temporary one, emptied.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tabby-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_file_starts_from_what_it_extends_and_terminal_without_it() {
        let theme = parse("extends = \"dracula\"\n[colors]\naccent = \"red\"\n").unwrap();
        let dracula = Theme::from(ThemeName::Dracula);
        assert_eq!(theme.accent, Color::Red);
        assert_eq!((theme.fg, theme.surface), (dracula.fg, dracula.surface));
        assert_eq!(parse("").unwrap(), Theme::from(ThemeName::Terminal));
    }

    #[test]
    fn a_bad_file_says_which_key() {
        for (text, key) in [
            ("extends = \"mine\"\n", "`extends`"),
            ("extends = 1\n", "`extends`"),
            ("colors = \"red\"\n", "`colors`"),
            ("[colors]\nsurface = \"reset\"\n", "[colors] `surface`"),
            ("[colours]\n", "`colours`"),
        ] {
            let err = parse(text).unwrap_err();
            assert!(err.contains(key), "{text}: {err}");
        }
        // Only built-in themes can be extended.
        let err = parse("extends = \"mine\"\n").unwrap_err();
        assert!(err.contains(&ThemeName::ids()), "{err}");
    }

    #[test]
    fn a_name_is_built_in_before_it_is_a_file_and_a_path_is_read_as_it_is() {
        let dir = scratch("load");
        fs::write(dir.join("nord.toml"), "[colors]\nfg = \"red\"\n").unwrap();
        fs::write(dir.join("mine.toml"), "[colors]\nfg = \"red\"\n").unwrap();
        assert_eq!(
            load("nord", Some(&dir)).unwrap(),
            Theme::from(ThemeName::Nord)
        );
        assert_eq!(load("mine", Some(&dir)).unwrap().fg, Color::Red);
        let path = dir.join("mine.toml");
        assert_eq!(load(path.to_str().unwrap(), None).unwrap().fg, Color::Red);

        let err = load("nope", Some(&dir)).unwrap_err().to_string();
        assert!(err.contains("`nope`") && err.contains("nope.toml"), "{err}");
        assert!(err.contains(&ThemeName::ids()), "{err}");
        fs::write(dir.join("bad.toml"), "[colors]\nfg = 1\n").unwrap();
        let err = format!("{:#}", load("bad", Some(&dir)).unwrap_err());
        assert!(err.contains("bad.toml") && err.contains("`fg`"), "{err}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
