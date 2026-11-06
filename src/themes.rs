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
    ui::{scroll::Scroll, theme::Theme},
};

/// Where a theme comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Builtin,
    /// A theme file: in the themes directory, or wherever a path put it.
    File(PathBuf),
}

/// One theme the picker offers.
#[derive(Debug, Clone)]
pub struct Entry {
    /// What `theme =` calls it; see `Settings::theme`.
    pub id: String,
    pub source: Source,
    /// The theme, `[theme_overrides]` not applied; or why it cannot be used.
    pub theme: Result<Theme, String>,
}

/// The theme picker (`C`): where it finds themes and saves the choice, and
/// what it offers.
#[derive(Debug, Default)]
pub struct Picker {
    /// The themes directory; `None` when the home directory is unknown.
    pub dir: Option<PathBuf>,
    /// `config.toml`, where a picked theme is saved.
    pub config: Option<PathBuf>,
    /// What the picker offers; scanned again every time it opens.
    pub entries: Vec<Entry>,
    /// Index into `entries` of the row the cursor is on.
    pub cursor: usize,
    pub scroll: Scroll,
    /// The theme in use when the picker opened, overrides applied, while it
    /// is open: the cursor previews other themes, `Esc` brings it back.
    pub before: Option<Theme>,
}

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

/// Every theme the picker offers: the built-in ones, then each `*.toml` of
/// `dir` by name, then `current` when it is a file found neither way.
///
/// A file that cannot be used is offered all the same, with the reason:
/// a broken file the user is editing should say what is wrong with it.
pub fn scan(dir: Option<&Path>, current: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = ThemeName::ALL
        .into_iter()
        .map(|name| Entry {
            id: name.id().to_owned(),
            source: Source::Builtin,
            theme: Ok(name.into()),
        })
        .collect();
    let mut files: Vec<(String, PathBuf)> = dir
        .and_then(|dir| fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "toml" || !path.is_file() {
                return None;
            }
            let id = path.file_stem()?.to_str()?.to_owned();
            // `theme =` could not name it.
            (!id.starts_with('.') && !config::is_theme_path(&id)).then_some((id, path))
        })
        .collect();
    files.sort();
    for (id, path) in files {
        let theme = if ThemeName::from_id(&id).is_some() {
            Err("named like a built-in theme, which wins".to_owned())
        } else {
            read(&path)
        };
        entries.push(Entry {
            id,
            source: Source::File(path),
            theme,
        });
    }
    if config::is_theme_path(current) && !entries.iter().any(|entry| entry.id == current) {
        let path = PathBuf::from(current);
        entries.push(Entry {
            id: current.to_owned(),
            theme: read(&path),
            source: Source::File(path),
        });
    }
    entries
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

    #[test]
    fn the_picker_offers_every_theme_file_and_says_what_is_wrong_with_one() {
        let dir = scratch("scan");
        fs::write(dir.join("zed.toml"), "").unwrap();
        fs::write(dir.join("alpha.toml"), "[colors]\nfg = \"pink\"\n").unwrap();
        fs::write(dir.join("dracula.toml"), "").unwrap();
        fs::write(dir.join("notes.txt"), "").unwrap();
        fs::write(dir.join(".hidden.toml"), "").unwrap();
        let elsewhere = dir.join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let outside = elsewhere.join("outside.toml");
        fs::write(&outside, "").unwrap();
        let outside = outside.to_str().unwrap();

        let entries = scan(Some(&dir), outside);
        let builtins = ThemeName::ALL.len();
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(&ids[..builtins], ThemeName::ALL.map(ThemeName::id));
        assert_eq!(&ids[builtins..], ["alpha", "dracula", "zed", outside]);
        let usable: Vec<bool> = entries[builtins..]
            .iter()
            .map(|e| e.theme.is_ok())
            .collect();
        // A broken file, and one named like a built-in theme, are offered
        // but cannot be used.
        assert_eq!(usable, [false, false, true, true]);
        // The theme in use is not offered twice; without a directory there
        // are the built-in themes alone.
        assert_eq!(scan(Some(&dir), "zed").len(), builtins + 3);
        assert_eq!(scan(None, "nord").len(), builtins);
        fs::remove_dir_all(&dir).unwrap();
    }
}
