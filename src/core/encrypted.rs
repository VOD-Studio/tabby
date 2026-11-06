//! Subscriptions encrypted with age (plan §7.1 step 1): what is one, and
//! how it is read with the key the user gives.

use age::x25519::Identity;

/// How a binary age file begins (the version line of its header).
const BINARY: &[u8] = b"age-encryption.org/v1";

/// How an armored one begins.
const ARMOR: &[u8] = b"-----BEGIN AGE ENCRYPTED FILE-----";

/// Why an encrypted subscription could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// What was given is not an age secret key (`AGE-SECRET-KEY-1…`).
    NotAKey,
    /// It is a key, and not the one the file was encrypted to.
    WrongKey,
    /// The file is damaged, cut short, or of a format this does not read.
    Damaged,
}

/// Whether `bytes` is an age file, binary or armored.
pub fn is_encrypted(bytes: &[u8]) -> bool {
    let start = bytes.trim_ascii_start();
    start.starts_with(BINARY) || start.starts_with(ARMOR)
}

/// The key `text` holds, as the one line `age-keygen` writes it in: a whole
/// key file pasted (with its `# created` and `# public key` comments) is
/// as good as the line.
pub fn key_line(text: &str) -> Result<String, Fault> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .ok_or(Fault::NotAKey)?;
    line.parse::<Identity>().map_err(|_| Fault::NotAKey)?;
    Ok(line.to_owned())
}

/// What `bytes` says, read with `key` (as [`key_line`] returned it).
pub fn decrypt(bytes: &[u8], key: &str) -> Result<Vec<u8>, Fault> {
    let identity: Identity = key.parse().map_err(|_| Fault::NotAKey)?;
    age::decrypt(&identity, bytes).map_err(|err| match err {
        age::DecryptError::NoMatchingKeys | age::DecryptError::KeyDecryptionFailed => {
            Fault::WrongKey
        }
        _ => Fault::Damaged,
    })
}

#[cfg(test)]
mod tests {
    use age::secrecy::ExposeSecret;

    use super::*;

    fn pair() -> (String, Identity) {
        let identity = Identity::generate();
        (identity.to_string().expose_secret().to_owned(), identity)
    }

    #[test]
    fn both_forms_of_an_age_file_are_known_and_a_config_is_not() {
        let (_, identity) = pair();
        let binary = age::encrypt(&identity.to_public(), b"mixed-port: 7890\n").unwrap();
        let armored = age::encrypt_and_armor(&identity.to_public(), b"x: 1\n").unwrap();
        assert!(is_encrypted(&binary));
        assert!(is_encrypted(armored.as_bytes()));
        assert!(is_encrypted(format!("\n  {armored}").as_bytes()));
        assert!(!is_encrypted(b"proxies: []\n"));
        assert!(!is_encrypted(b""));
        // What merely mentions it later is not one.
        assert!(!is_encrypted(b"# age-encryption.org/v1\nproxies: []\n"));
    }

    #[test]
    fn the_right_key_reads_either_form_and_the_wrong_one_does_not() {
        let (key, identity) = pair();
        let (other, _) = pair();
        let binary = age::encrypt(&identity.to_public(), b"mixed-port: 7890\n").unwrap();
        let armored = age::encrypt_and_armor(&identity.to_public(), b"mixed-port: 7890\n").unwrap();

        assert_eq!(decrypt(&binary, &key).unwrap(), b"mixed-port: 7890\n");
        assert_eq!(
            decrypt(armored.as_bytes(), &key).unwrap(),
            b"mixed-port: 7890\n"
        );
        assert_eq!(decrypt(&binary, &other), Err(Fault::WrongKey));
        assert_eq!(decrypt(&binary, "not a key"), Err(Fault::NotAKey));
    }

    #[test]
    fn a_file_cut_short_is_damaged_not_a_wrong_key() {
        let (key, identity) = pair();
        let binary = age::encrypt(&identity.to_public(), &vec![b'x'; 200_000]).unwrap();

        assert_eq!(
            decrypt(&binary[..binary.len() - 10], &key),
            Err(Fault::Damaged)
        );
        assert_eq!(decrypt(&binary[..30], &key), Err(Fault::Damaged));
    }

    #[test]
    fn a_whole_key_file_as_age_keygen_writes_it_gives_its_key() {
        let (key, identity) = pair();
        let file = format!(
            "# created: 2026-11-06T12:00:00+08:00\n# public key: {}\n{key}\n",
            identity.to_public()
        );

        assert_eq!(key_line(&file).unwrap(), key);
        assert_eq!(key_line(&format!("  {key}  ")).unwrap(), key);
        assert_eq!(key_line(""), Err(Fault::NotAKey));
        assert_eq!(key_line("# only a comment"), Err(Fault::NotAKey));
        // The public half is not a key to decrypt with.
        assert_eq!(
            key_line(&identity.to_public().to_string()),
            Err(Fault::NotAKey)
        );
    }
}
