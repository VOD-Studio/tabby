use std::{
    fs,
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
/// `recorder`, its kernel the user's own script in `dir`. With `systemd`, a
/// user manager is there.
pub(crate) fn core_at(
    dir: &Path,
    port: u16,
    recorder: &Arc<Recorder>,
    systemd: bool,
) -> ManagedCore {
    core_in(dir, port, recorder, systemd, true)
}

/// Like [`core_at`], but with no `[core].binary`: the kernel is the one
/// tabby downloads, which is not there yet.
pub(crate) fn core_without_binary(dir: &Path, port: u16, recorder: &Arc<Recorder>) -> ManagedCore {
    core_in(dir, port, recorder, false, false)
}

fn core_in(
    dir: &Path,
    port: u16,
    recorder: &Arc<Recorder>,
    systemd: bool,
    user_binary: bool,
) -> ManagedCore {
    let state = State {
        port,
        secret: SECRET.to_owned(),
        unit: UNIT.to_owned(),
        pid: None,
        pid_start: None,
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
        user_binary.then_some(binary),
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

/// Starts a controller on `port` in a thread of its own, after `delay`. With
/// none, it is listening when this returns, not whenever the thread gets to
/// it.
pub(crate) fn serve(port: u16, reply: Reply, delay: Duration) {
    let accept = move |listener: TcpListener| {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            thread::spawn(move || respond(stream, reply, port));
        }
    };
    if delay.is_zero() {
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        thread::spawn(move || accept(listener));
    } else {
        thread::spawn(move || {
            thread::sleep(delay);
            accept(TcpListener::bind(("127.0.0.1", port)).unwrap());
        });
    }
}

/// The request lines every stand-in controller was sent, by port.
static REQUESTS: Mutex<Vec<(u16, String)>> = Mutex::new(Vec::new());

/// What the stand-in controller on `port` was asked, as `METHOD /path`.
pub(crate) fn requests_to(port: u16) -> Vec<String> {
    REQUESTS
        .lock()
        .unwrap()
        .iter()
        .filter(|(at, _)| *at == port)
        .map(|(_, line)| line.clone())
        .collect()
}

pub(crate) fn respond(stream: TcpStream, reply: Reply, port: u16) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut authorized = false;
    let mut first = true;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if std::mem::take(&mut first) {
            let request: Vec<&str> = line.split_whitespace().take(2).collect();
            REQUESTS.lock().unwrap().push((port, request.join(" ")));
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

pub(crate) const TAG: &str = "v1.2.3";
pub(crate) const ASSET: &str = "mihomo-linux-amd64-v1-v1.2.3.gz";
pub(crate) const KERNEL: &[u8] = b"#!/bin/sh\necho kernel\n";

pub(crate) fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    use std::fmt::Write as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// A release server: the API lists [`ASSET`] of [`TAG`] with `digest`, and
/// the download serves `served`.
pub(crate) async fn release_server(
    digest: Option<String>,
    served: Vec<u8>,
) -> wiremock::MockServer {
    use wiremock::{
        Mock, ResponseTemplate,
        matchers::{method, path},
    };
    let mock = wiremock::MockServer::start().await;
    let release = serde_json::json!({
        "tag_name": TAG,
        "assets": [{
            "name": ASSET,
            "browser_download_url": format!("{}/dl/{ASSET}", mock.uri()),
            "digest": digest,
        }],
    });
    Mock::given(method("GET"))
        .and(path(super::install::LATEST_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(release))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/dl/{ASSET}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(served))
        .mount(&mock)
        .await;
    mock
}

/// A managed core under a user manager with no kernel of its own and none
/// downloaded, looking releases up at `release_api` as an x86-64 machine.
pub(crate) fn core_downloading(
    dir: &Path,
    port: u16,
    recorder: &Arc<Recorder>,
    release_api: &str,
) -> ManagedCore {
    let mut core = core_in(dir, port, recorder, true, false);
    core.release_api = release_api.to_owned();
    core.target = Target {
        arch: "x86_64".to_owned(),
        cpuinfo: String::new(),
    };
    core
}
