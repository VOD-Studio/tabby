//! The operating system tabby runs on, as a value: code for every system is
//! compiled everywhere and picked at run time, so that the macOS paths are
//! tested on Linux too (plan `2026-11-11-macos.md` §2).

/// What tabby does differently from one system to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// Linux, and any other system treated like it.
    Linux,
    Macos,
}

impl Os {
    /// The system tabby was built for.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }
}
