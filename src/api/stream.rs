use std::{collections::HashMap, time::Duration};

use color_eyre::{
    Result,
    eyre::{bail, eyre},
};
use futures_util::StreamExt;
use reqwest::Url;
use serde::Deserialize;
use tokio::{
    net::TcpStream,
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

use super::{
    backoff,
    models::{ConnectionSnapshot, Memory, Traffic},
};
use crate::app::Msg;

/// How long one connection attempt, TLS and upgrade included, may take.
/// Without a limit a controller that drops packets would hold a stream in one
/// attempt for as long as the OS lets a connect hang.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The streaming endpoints of the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stream {
    Traffic,
    /// mihomo only.
    Memory,
    Logs,
    Connections,
}

impl Stream {
    fn path(self) -> &'static str {
        match self {
            Self::Traffic => "traffic",
            Self::Memory => "memory",
            Self::Logs => "logs",
            Self::Connections => "connections",
        }
    }

    /// Fixed query parameter of the endpoint.
    fn query(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Logs => Some(("level", "info")),
            _ => None,
        }
    }
}

/// What a stream task reports.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// A connection was established.
    Connected,
    /// The connection that had been reported as established ended. Failed
    /// attempts while down are not reported again.
    Disconnected,
    Traffic(Traffic),
    Connections(ConnectionSnapshot),
    Memory(Memory),
}

/// The WebSocket URL of `stream` on the controller at `base`.
///
/// WebSocket handshakes from a browser cannot carry an `Authorization`
/// header, so the controller takes the secret as a `token` query parameter;
/// it is percent-encoded like any other value (R2).
fn url(base: &str, secret: Option<&str>, stream: Stream) -> Result<Url> {
    let mut url = Url::parse(base)?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        other => bail!("controller url `{base}` must use http or https, not {other}"),
    };
    url.set_scheme(scheme)
        .map_err(|()| eyre!("cannot use {scheme} for `{base}`"))?;
    url.set_query(None);
    url.set_fragment(None);
    // `pop_if_empty` drops the empty segment a trailing `/` leaves behind.
    url.path_segments_mut()
        .map_err(|()| eyre!("controller url `{base}` cannot carry a path"))?
        .pop_if_empty()
        .push(stream.path());
    let pairs: Vec<(&str, &str)> = stream
        .query()
        .into_iter()
        .chain(secret.map(|secret| ("token", secret)))
        .collect();
    if !pairs.is_empty() {
        url.query_pairs_mut().extend_pairs(pairs);
    }
    Ok(url)
}

/// The stream tasks of one controller. Dropping it stops them all, which is
/// how a profile switch replaces the whole set.
#[derive(Debug)]
pub struct Streams {
    base: String,
    secret: Option<String>,
    tx: mpsc::Sender<Msg>,
    /// Ticked to cut short the backoff wait of every stream.
    retry: watch::Sender<()>,
    tasks: HashMap<Stream, JoinHandle<()>>,
}

impl Streams {
    pub fn new(base: &str, secret: Option<&str>, tx: mpsc::Sender<Msg>) -> Self {
        Self {
            base: base.to_owned(),
            secret: secret.map(str::to_owned),
            tx,
            retry: watch::Sender::new(()),
            tasks: HashMap::new(),
        }
    }

    /// Starts the task of `stream`, unless it is already running.
    pub fn start(&mut self, stream: Stream) {
        if self.tasks.contains_key(&stream) {
            return;
        }
        let url = match url(&self.base, self.secret.as_deref(), stream) {
            Ok(url) => url,
            Err(err) => {
                tracing::error!(?stream, "cannot subscribe: {err}");
                return;
            }
        };
        let task = tokio::spawn(run(stream, url, self.tx.clone(), self.retry.subscribe()));
        self.tasks.insert(stream, task);
    }

    /// Makes every stream that is waiting out a backoff reconnect now.
    pub fn retry_now(&self) {
        self.retry.send_replace(());
    }

    #[cfg(test)]
    pub fn started(&self, stream: Stream) -> bool {
        self.tasks.contains_key(&stream)
    }
}

impl Drop for Streams {
    fn drop(&mut self) {
        for task in self.tasks.values() {
            task.abort();
        }
    }
}

/// Connects, forwards frames, and reconnects after a backoff, for ever.
async fn run(stream: Stream, url: Url, tx: mpsc::Sender<Msg>, mut retry: watch::Receiver<()>) {
    let mut failures = 0;
    loop {
        match timeout(CONNECT_TIMEOUT, connect_async(url.as_str())).await {
            Ok(Ok((mut socket, _))) => {
                failures = 0;
                tracing::info!(?stream, "stream connected");
                if !emit(&tx, stream, Event::Connected).await
                    || !pump(stream, &mut socket, &tx).await
                    || !emit(&tx, stream, Event::Disconnected).await
                {
                    return;
                }
                tracing::info!(?stream, "stream disconnected");
            }
            // Retries while the controller is down would flood the log at
            // any level people leave on.
            Ok(Err(err)) => tracing::debug!(?stream, "cannot connect: {err}"),
            Err(_) => tracing::debug!(?stream, "connect timed out"),
        }
        wait(backoff(failures), &mut retry).await;
        failures = failures.saturating_add(1);
    }
}

/// Forwards the frames of `socket` until the connection ends. Returns `false`
/// when the app is gone and nobody is listening any more.
async fn pump(stream: Stream, socket: &mut Socket, tx: &mpsc::Sender<Msg>) -> bool {
    while let Some(frame) = socket.next().await {
        match frame {
            Ok(Message::Text(text)) => {
                if let Some(event) = decode(stream, &text)
                    && !emit(tx, stream, event).await
                {
                    return false;
                }
            }
            Ok(Message::Close(_)) => break,
            // Pings are answered by the library; the kernel sends no binary.
            Ok(_) => {}
            Err(err) => {
                tracing::debug!(?stream, "connection failed: {err}");
                break;
            }
        }
    }
    true
}

/// Reads a text frame. A frame that does not parse is logged and skipped:
/// one odd frame is no reason to drop the connection.
///
/// Log frames are not read yet; that stream counts as live for as long as it
/// delivers.
fn decode(stream: Stream, text: &str) -> Option<Event> {
    match stream {
        Stream::Traffic => parse(stream, text).map(Event::Traffic),
        Stream::Connections => parse(stream, text).map(Event::Connections),
        Stream::Memory => parse(stream, text).map(Event::Memory),
        Stream::Logs => None,
    }
}

fn parse<'a, T: Deserialize<'a>>(stream: Stream, text: &'a str) -> Option<T> {
    match serde_json::from_str(text) {
        Ok(frame) => Some(frame),
        Err(err) => {
            tracing::warn!(?stream, "unreadable frame: {err}");
            None
        }
    }
}

/// Reports to the app. `false` when it has hung up.
async fn emit(tx: &mpsc::Sender<Msg>, stream: Stream, event: Event) -> bool {
    tx.send(Msg::Stream { stream, event }).await.is_ok()
}

/// Sleeps for `delay`, or until [`Streams::retry_now`] is called meanwhile.
/// A call made before this started waiting does not count: the stream was
/// connected or connecting then, and it must not skip the backoff of a later
/// failure.
async fn wait(delay: Duration, retry: &mut watch::Receiver<()>) {
    retry.borrow_and_update();
    tokio::select! {
        () = sleep(delay) => {}
        // `Err` means no `Streams` is left to ask; keep sleeping.
        Ok(()) = retry.changed() => {}
    }
}

#[cfg(test)]
mod tests {
    use futures_util::SinkExt;
    use tokio::{net::TcpListener, time::Instant};
    use tokio_tungstenite::{
        accept_async, accept_hdr_async,
        tungstenite::handshake::server::{Request, Response},
    };

    use super::*;

    fn build(base: &str, secret: Option<&str>, stream: Stream) -> Url {
        url(base, secret, stream).unwrap()
    }

    #[test]
    fn scheme_follows_the_controller_scheme() {
        let plain = build("http://127.0.0.1:9090", None, Stream::Traffic);
        assert_eq!(plain.as_str(), "ws://127.0.0.1:9090/traffic");
        let tls = build("https://router.lan:9090", None, Stream::Traffic);
        assert_eq!(tls.as_str(), "wss://router.lan:9090/traffic");
        assert!(url("ftp://router.lan", None, Stream::Traffic).is_err());
    }

    #[test]
    fn path_is_joined_with_or_without_a_trailing_slash() {
        for base in ["http://h:9090", "http://h:9090/"] {
            assert_eq!(build(base, None, Stream::Memory).path(), "/memory");
        }
        // A controller behind a reverse proxy lives under a prefix.
        for base in ["http://h/clash", "http://h/clash/"] {
            assert_eq!(build(base, None, Stream::Memory).path(), "/clash/memory");
        }
    }

    #[test]
    fn only_authenticated_urls_carry_a_token() {
        let open = build("http://h:9090", None, Stream::Traffic);
        assert_eq!(open.query(), None);
        let logs = build("http://h:9090", None, Stream::Logs);
        assert_eq!(logs.query(), Some("level=info"));
        let secured = build("http://h:9090", Some("s3cret"), Stream::Logs);
        assert_eq!(secured.query(), Some("level=info&token=s3cret"));
    }

    #[test]
    fn secret_is_encoded_so_it_cannot_escape_the_token_value() {
        for secret in ["a&b=c", "x#y", "1+1", "has space", "密码", "%41", "a/b?c"] {
            let url = build("http://h:9090", Some(secret), Stream::Connections);
            let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
            assert_eq!(pairs, [("token".to_owned(), secret.to_owned())]);
            assert_eq!(url.fragment(), None, "{secret}");
            assert_eq!(url.path(), "/connections", "{secret}");
        }
        // `+` means space in a query, so a literal one must be escaped.
        let url = build("http://h", Some("1+1 2"), Stream::Traffic);
        assert_eq!(url.query(), Some("token=1%2B1+2"));
    }

    #[test]
    fn query_and_fragment_of_the_base_are_dropped() {
        let url = build("http://h:9090/?x=1#top", Some("k"), Stream::Traffic);
        assert_eq!(url.as_str(), "ws://h:9090/traffic?token=k");
    }

    #[test]
    fn each_stream_decodes_its_own_frames() {
        let connections = include_str!("../../tests/fixtures/mihomo_connections.json");
        let memory = include_str!("../../tests/fixtures/mihomo_memory.json");
        let Some(Event::Connections(snapshot)) = decode(Stream::Connections, connections) else {
            panic!("connections frame not decoded");
        };
        assert_eq!(snapshot.stats().active, 5);
        assert_eq!(
            decode(Stream::Memory, memory),
            Some(Event::Memory(Memory { inuse: 188_715_008 }))
        );
        // Logs are not read, and an unreadable frame is skipped, not fatal.
        assert_eq!(decode(Stream::Logs, memory), None);
        assert_eq!(decode(Stream::Connections, "not json"), None);
        assert_eq!(decode(Stream::Memory, r#"{"inuse":"lots"}"#), None);
    }

    /// With the clock paused, sleeping costs no real time and `elapsed` is
    /// exactly the virtual time that passed.
    #[tokio::test(start_paused = true)]
    async fn retry_cuts_a_backoff_short() {
        let (tick, mut retry) = watch::channel(());
        let started = Instant::now();
        let waiting = tokio::spawn(async move { wait(Duration::from_secs(30), &mut retry).await });
        // Let the task reach its sleep before asking.
        tokio::task::yield_now().await;
        tick.send_replace(());
        waiting.await.unwrap();
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_asked_before_waiting_does_not_skip_the_backoff() {
        let (tick, mut retry) = watch::channel(());
        tick.send_replace(());
        let started = Instant::now();
        wait(Duration::from_secs(30), &mut retry).await;
        assert_eq!(started.elapsed(), Duration::from_secs(30));
    }

    /// Accepts one connection and reports its request target.
    // The error type is fixed by tungstenite's `Callback` bound.
    #[allow(clippy::result_large_err)]
    async fn accept_recording(
        listener: &TcpListener,
        targets: &mpsc::UnboundedSender<String>,
    ) -> WebSocketStream<TcpStream> {
        let (conn, _) = listener.accept().await.unwrap();
        accept_hdr_async(conn, |request: &Request, response: Response| {
            targets.send(request.uri().to_string()).unwrap();
            Ok(response)
        })
        .await
        .unwrap()
    }

    /// A controller that serves one frame on the first connection, hangs up,
    /// and keeps the second. Reports the request target of each handshake.
    async fn flaky_controller(listener: TcpListener, targets: mpsc::UnboundedSender<String>) {
        let mut socket = accept_recording(&listener, &targets).await;
        socket
            .send(Message::text(r#"{"up":10,"down":20}"#))
            .await
            .unwrap();
        socket.close(None).await.unwrap();
        // Drive the closing handshake to its end.
        while socket.next().await.is_some() {}

        let mut socket = accept_recording(&listener, &targets).await;
        // Hold the connection until the client goes away.
        while socket.next().await.is_some() {}
    }

    #[tokio::test]
    async fn reconnects_after_the_server_hangs_up() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let (targets_tx, mut targets) = mpsc::unbounded_channel();
        let server = tokio::spawn(flaky_controller(listener, targets_tx));

        let (tx, mut rx) = mpsc::channel(8);
        let mut streams = Streams::new(&base, Some("p&w #+密"), tx);
        streams.start(Stream::Traffic);

        // The first backoff is 1 s; the bound only keeps a bug from hanging.
        let mut seen = vec![];
        while seen.len() < 4 {
            let msg = timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("stream went quiet")
                .unwrap();
            let Msg::Stream { stream, event } = msg else {
                panic!("unexpected message {msg:?}");
            };
            assert_eq!(stream, Stream::Traffic);
            seen.push(event);
        }
        assert_eq!(
            seen,
            [
                Event::Connected,
                Event::Traffic(Traffic { up: 10, down: 20 }),
                Event::Disconnected,
                Event::Connected,
            ]
        );

        // Both handshakes asked for the same path with the decoded token.
        for _ in 0..2 {
            let target = Url::parse(&format!("ws://h{}", targets.recv().await.unwrap())).unwrap();
            assert_eq!(target.path(), "/traffic");
            let pairs: Vec<_> = target.query_pairs().into_owned().collect();
            assert_eq!(pairs, [("token".to_owned(), "p&w #+密".to_owned())]);
        }

        // Dropping `Streams` stops its tasks, which releases their senders and
        // so closes the channel; a task left running would keep it open.
        drop(streams);
        let closed = timeout(Duration::from_secs(10), rx.recv()).await;
        assert!(matches!(closed, Ok(None)), "{closed:?}");
        server.abort();
    }

    #[tokio::test]
    async fn a_garbled_frame_is_skipped_without_dropping_the_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (conn, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(conn).await.unwrap();
            socket.send(Message::text("not json")).await.unwrap();
            socket.send(Message::text(r#"{"down":5}"#)).await.unwrap();
            while socket.next().await.is_some() {}
        });

        let (tx, mut rx) = mpsc::channel(8);
        let mut streams = Streams::new(&base, None, tx);
        streams.start(Stream::Traffic);
        let mut events = vec![];
        while events.len() < 2 {
            let msg = timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("stream went quiet")
                .unwrap();
            let Msg::Stream { event, .. } = msg else {
                panic!("unexpected message {msg:?}");
            };
            events.push(event);
        }
        assert_eq!(
            events,
            [Event::Connected, Event::Traffic(Traffic { up: 0, down: 5 })]
        );
    }
}
