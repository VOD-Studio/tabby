use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    process,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT_TEMPORARY: AtomicUsize = AtomicUsize::new(0);

/// Writes `contents` to `path` so that only its owner can read it, through
/// a temporary file next to it: a reader sees the old file or the new one,
/// and a failed write leaves the old one alone.
///
/// Each writer owns its temporary file, even when replacing the same path.
pub fn write_private(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let (temporary, mut file) = loop {
        let mut name = path.file_name().unwrap_or_default().to_owned();
        let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        name.push(format!(".{}-{sequence}.tmp", process::id()));
        let temporary = path.with_file_name(name);
        match options.open(&temporary) {
            Ok(file) => break (temporary, file),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    };
    let written = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
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
    use std::{os::unix::fs::PermissionsExt, sync::Barrier, thread};

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

    #[test]
    fn concurrent_writers_leave_one_complete_file_and_no_temporary_files() {
        let dir = scratch("private-concurrent");
        let path = dir.join("state.toml");
        let contents = ["a".repeat(128 * 1024), "b".repeat(64 * 1024)];
        let barrier = Barrier::new(contents.len());
        thread::scope(|scope| {
            for text in &contents {
                let (path, barrier) = (&path, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    write_private(path, text).unwrap();
                });
            }
        });
        assert!(contents.contains(&fs::read_to_string(&path).unwrap()));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
}
