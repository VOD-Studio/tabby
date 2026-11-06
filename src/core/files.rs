use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
};

/// Writes `contents` to `path` so that only its owner can read it, through
/// a temporary file next to it: a reader sees the old file or the new one,
/// and a failed write leaves the old one alone.
///
/// For files that hold a secret: the controller's, or a subscription URL's.
pub fn write_private(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".tmp");
    let temporary = path.with_file_name(name);
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&temporary).and_then(|mut file| {
        file.write_all(contents.as_bytes())?;
        file.sync_all()
    });
    match written.and_then(|()| fs::rename(&temporary, path)) {
        Ok(()) => Ok(()),
        Err(err) => {
            // The write is what failed; a leftover is no better.
            let _ = fs::remove_file(&temporary);
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tabby-{}-files-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_private_file_is_owner_only_even_when_it_replaces_a_looser_one() {
        let dir = scratch("private");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.toml");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        write_private(&path, "new").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // No temporary left behind.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_parent_directories_are_created() {
        let dir = scratch("parents");
        let path = dir.join("a/b/config.yaml");
        write_private(&path, "x").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "x");
        fs::remove_dir_all(&dir).unwrap();
    }
}
