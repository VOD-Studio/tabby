use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    process::{ExitStatus, Output},
    sync::Mutex,
    thread,
};

use super::{runner::Cmd, *};

pub(crate) const SECRET: &str = "test-secret";
pub(crate) const UNIT: &str = "tabby-mihomo-test";
pub(crate) const MIHOMO: &str = r#"{"version":"v1.19.33","meta":true}"#;

pub(crate) fn scratch(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("tabby-{}-core-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

pub(crate) fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub(crate) fn answer(code: i32, stdout: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(code << 8),
        stdout: stdout.as_bytes().to_vec(),
        stderr: vec![],
    }
}

pub(crate) type Script = Box<dyn Fn(&Cmd) -> Output + Send + Sync>;

/// A runner that records every command and lets the test answer for it.
pub(crate) struct Recorder {
    calls: Mutex<Vec<Cmd>>,
    script: Script,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Recorder")
    }
}

impl Runner for Arc<Recorder> {
    fn run(&self, cmd: &Cmd) -> io::Result<Output> {
        self.calls.lock().unwrap().push(cmd.clone());
        Ok((self.script)(cmd))
    }
}

impl Recorder {
    pub(crate) fn new(script: impl Fn(&Cmd) -> Output + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(vec![]),
            script: Box::new(script),
        })
    }

    pub(crate) fn calls(&self) -> Vec<Cmd> {
        self.calls.lock().unwrap().clone()
    }

    /// The commands that ran `program`, as `arg arg …`.
    pub(crate) fn runs(&self, program: &str) -> Vec<String> {
        self.calls()
            .iter()
            .filter(|cmd| cmd.program == program)
            .map(|cmd| cmd.args.join(" "))
            .collect()
    }
}

/// A managed core in `dir` with the controller on `port`, run through
/// `recorder`. With `systemd`, a user manager is there.
pub(crate) fn core_at(
    dir: &Path,
    port: u16,
    recorder: &Arc<Recorder>,
    systemd: bool,
) -> ManagedCore {
    let state = State {
        port,
        secret: SECRET.to_owned(),
        unit: UNIT.to_owned(),
        pid: None,
    };
    let root = dir.join("core");
    fs::create_dir_all(&root).unwrap();
    state.save(&root.join("state.toml")).unwrap();
    let runtime = dir.join("run");
    if systemd {
        fs::create_dir_all(runtime.join("systemd")).unwrap();
        fs::write(runtime.join("systemd/private"), "").unwrap();
    }
    let binary = dir.join("mihomo");
    fs::write(&binary, "#!/bin/sh\nwhile :; do sleep 1; done\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let mut core = ManagedCore::with(
        Some(binary),
        dir,
        Arc::new(Arc::clone(recorder)),
        Some(runtime),
    )
    .unwrap();
    core.timing = Timing {
        ready: Duration::from_secs(3),
        poll: Duration::from_millis(20),
        stop_grace: Duration::from_secs(2),
    };
    core
}

/// What the stand-in controller says to `GET /version`.
#[derive(Clone, Copy)]
pub(crate) enum Reply {
    Mihomo,
    /// Not the secret's owner.
    Rejects,
    /// An HTTP server that is not a kernel.
    Other,
}

/// Starts a controller on `port` in a thread of its own, after `delay`.
pub(crate) fn serve(port: u16, reply: Reply, delay: Duration) {
    thread::spawn(move || {
        thread::sleep(delay);
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            thread::spawn(move || respond(stream, reply));
        }
    });
}

pub(crate) fn respond(stream: TcpStream, reply: Reply) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut authorized = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        authorized |= line
            .to_lowercase()
            .starts_with(&format!("authorization: bearer {SECRET}"));
    }
    let (status, body) = match reply {
        Reply::Mihomo if authorized => ("200 OK", MIHOMO),
        Reply::Mihomo | Reply::Rejects => ("401 Unauthorized", r#"{"message":"Unauthorized"}"#),
        Reply::Other => ("200 OK", "<html>hello</html>"),
    };
    let mut stream = stream;
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// A user manager that has the unit in `state` and starts it when asked;
/// the kernel then answers on `port` after `delay`.
pub(crate) fn systemd(port: u16, delay: Duration) -> Arc<Recorder> {
    let active = Arc::new(Mutex::new("inactive".to_owned()));
    Recorder::new(move |cmd| match (cmd.program.as_str(), cmd.args.get(1)) {
        ("systemd-run", _) => {
            serve(port, Reply::Mihomo, delay);
            *active.lock().unwrap() = "active".to_owned();
            answer(0, "")
        }
        ("systemctl", Some(verb)) if verb == "is-active" => {
            let state = active.lock().unwrap().clone();
            answer(if state == "active" { 0 } else { 3 }, &format!("{state}\n"))
        }
        _ => answer(0, ""),
    })
}
