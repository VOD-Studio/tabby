use std::{
    io,
    process::{Command, Output, Stdio},
};

/// One external command: a program and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
}

impl Cmd {
    pub fn new<I, S>(program: &str, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            program: program.to_owned(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }
}

/// How the managed core runs the programs it leans on (`systemd-run`,
/// `systemctl`, `journalctl`, `kill`, …). One seam, so tests can record
/// what would have run and answer for it.
pub trait Runner: Send + Sync + std::fmt::Debug {
    /// Runs `cmd` to its end with stdin closed and its output captured.
    fn run(&self, cmd: &Cmd) -> io::Result<Output>;
}

/// Runs the programs for real.
#[derive(Debug, Default)]
pub struct System;

impl Runner for System {
    fn run(&self, cmd: &Cmd) -> io::Result<Output> {
        Command::new(&cmd.program)
            .args(&cmd.args)
            .stdin(Stdio::null())
            .output()
    }
}
