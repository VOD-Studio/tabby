use std::{
    fmt::Write as _,
    fs::{self, File},
    io::{self, Read},
    net::TcpListener,
    path::Path,
};

use serde::{Deserialize, Serialize};

use super::files::write_private;

/// What tabby keeps in `state.toml` about the kernel it manages: where its
/// controller listens and how it is told apart from other kernels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The controller's port on 127.0.0.1.
    pub port: u16,
    /// The controller's secret. It only ever goes into the kernel's config
    /// file: a command line is readable by every user.
    pub secret: String,
    /// The systemd unit the kernel runs as; the suffix keeps the units of
    /// different data directories apart.
    pub unit: String,
    /// The kernel's process id, when it runs without systemd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

impl State {
    /// Reads `path`; where there is no file yet, makes a state with a free
    /// port, a random secret and a random unit name, and saves it.
    ///
    /// A file that cannot be read is an error, not a reason to start over:
    /// a kernel already running keeps the secret it was given.
    pub fn load_or_create(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let state = Self {
                    port: free_port()?,
                    secret: random_hex(32)?,
                    unit: format!("tabby-mihomo-{}", random_hex(4)?),
                    pid: None,
                };
                state.save(path)?;
                Ok(state)
            }
            Err(err) => Err(err),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
        write_private(path, &text)
    }
}

/// A port nothing listens on right now.
fn free_port() -> io::Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

/// `bytes` random bytes from the system, as lowercase hex.
fn random_hex(bytes: usize) -> io::Result<String> {
    let mut buffer = vec![0; bytes];
    File::open("/dev/urandom")?.read_exact(&mut buffer)?;
    Ok(buffer.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tabby-{}-state-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("state.toml")
    }

    #[test]
    fn a_first_state_is_random_private_and_what_the_next_start_reads() {
        let path = scratch("create");
        let first = State::load_or_create(&path).unwrap();
        assert_eq!(first.secret.len(), 64);
        assert!(first.unit.starts_with("tabby-mihomo-") && first.unit.len() == 21);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        assert_eq!(State::load_or_create(&path).unwrap(), first);

        // Another data directory gets its own secret and unit.
        let other = State::load_or_create(&scratch("create-other")).unwrap();
        assert_ne!(other.secret, first.secret);
        assert_ne!(other.unit, first.unit);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_broken_state_file_is_an_error_not_a_fresh_secret() {
        let path = scratch("broken");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "port = \"not a number\"").unwrap();
        let err = State::load_or_create(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        // Left as it was, for the user to look at.
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "port = \"not a number\""
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
