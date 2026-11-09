//! Bakes the commit tabby is built from into `tabby --version`:
//! `TABBY_LONG_VERSION` reads `0.1.0 (4ecf936 2026-11-09)`, with `-dirty`
//! after the hash when the sources differ from that commit, and is just the
//! version outside a git checkout (a source tarball, a crate).

use std::{env, fs, path::Path, process::Command};

/// What the binary is built from: a change to one of these makes the build
/// `-dirty`, and makes cargo run this script again.
const SOURCES: [&str; 4] = ["src", "build.rs", "Cargo.toml", "Cargo.lock"];

fn main() {
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let long = match commit() {
        Some(commit) => format!("{version} ({commit})"),
        None => version,
    };
    println!("cargo::rustc-env=TABBY_LONG_VERSION={long}");
}

/// `<hash>[-dirty] <commit date>` of HEAD, or `None` outside tabby's own
/// checkout or without git.
fn commit() -> Option<String> {
    let root = env::var("CARGO_MANIFEST_DIR").ok()?;
    // Sources unpacked inside some other checkout must not report its commit.
    let top = git(&root, &["rev-parse", "--show-toplevel"])?;
    if fs::canonicalize(top).ok()? != fs::canonicalize(&root).ok()? {
        return None;
    }
    for path in SOURCES {
        println!("cargo::rerun-if-changed={path}");
    }
    // HEAD's reflog moves on every commit, checkout and reset; HEAD itself
    // when the branch changes. A missing file would rerun this every build.
    for name in ["HEAD", "logs/HEAD"] {
        if let Some(path) = git(&root, &["rev-parse", "--git-path", name])
            && Path::new(&root).join(&path).exists()
        {
            println!("cargo::rerun-if-changed={path}");
        }
    }
    let head = git(&root, &["log", "-1", "--format=%h %cs"])?;
    let (hash, date) = head.split_once(' ')?;
    let mut status = vec!["status", "--porcelain", "--"];
    status.extend(SOURCES);
    let dirty = if git(&root, &status).is_some() {
        "-dirty"
    } else {
        ""
    };
    Some(format!("{hash}{dirty} {date}"))
}

/// The trimmed output of a successful `git` run in `dir`, `None` when it
/// fails or prints nothing.
fn git(dir: &str, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    let out = String::from_utf8(output.stdout).ok()?;
    let out = out.trim();
    (output.status.success() && !out.is_empty()).then(|| out.to_owned())
}
