//! The control channel: how agents and scripts drive the board a person has open in the desktop
//! app, instead of a headless session of their own.
//!
//! The app listens on a TCP port of the loopback interface, off unless it is started with
//! `--control`. Messages are the JSON-RPC lines of the MCP server in this crate, one per line,
//! answered by [`crate::handle_line`] against the app's own session. Before anything else a
//! caller proves it may drive the app: its first line is
//!
//! ```json
//! {"jsonrpc":"2.0","id":1,"method":"control/authenticate","params":{"token":"…"}}
//! ```
//!
//! with the token the app wrote, on starting, to its control file ([`control_file`]), which only
//! the user can read. Any other first line is answered with an error and the connection closed.
//! The app's answer carries a second value from the file, so a caller knows it reached the app
//! that wrote the file and not a program that took the port after the app ended.
//!
//! What this guards against (ROADMAP.md, decided 2026-11-14):
//!
//! - Other machines: the app binds a loopback address only, and drops a connection from any
//!   other address.
//! - Other users of the same machine, who can reach a loopback port but can't read the file.
//! - Web pages, which can make a browser send an HTTP request to a loopback port: its first line
//!   is not the authentication, so the connection is closed before any of it is read as a
//!   command.
//! - Another user's program on the port of an app that has ended: it can't give the answer, so
//!   `mcp --connect` doesn't pass the agent's messages to it.
//!
//! It doesn't guard against programs running as the same user. They can read the token, but they
//! can also edit the design files on disk, so the channel gives them nothing new.
//!
//! Threads: a thread accepts connections and one per connection reads its lines. Commands run on
//! the UI thread, which owns the session: each request is queued for it, the app is woken
//! ([`Waker`]), and the connection waits for the answer before it reads its next line. So the
//! queue never holds more than one request per connection, and connections are limited too.
//!
//! In the browser [`Server::start`] says the channel is not available, so the code behind it is
//! built there but never used.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

use circuitcraft_io::storage::{self, DiskStorage, Storage as _};
use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Where the app listens unless told otherwise.
pub const DEFAULT_ADDRESS: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 7979));

/// The method of the first message on a connection.
pub const AUTHENTICATE: &str = "control/authenticate";

/// The most connections open at once, authenticated or not. One more is told the app is busy and
/// closed.
pub const MAX_CONNECTIONS: usize = 8;

/// The most requests waiting for the UI thread. Each connection waits for its answer before it
/// reads on, so this is never reached; it bounds the queue should that change.
pub const MAX_QUEUED: usize = MAX_CONNECTIONS;

/// The longest first line, in bytes: an unauthenticated caller can't make the app hold more.
pub const MAX_HELLO_BYTES: u64 = 4096;

/// How long a new connection has to authenticate, all told. A caller that hasn't by then is
/// closed, so slow or idle connections can't keep out the others for long.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// Random bytes in a token.
const TOKEN_BYTES: usize = 32;

/// The longest control file read, in bytes.
const MAX_CONTROL_FILE_BYTES: u64 = 4096;

/// What the control file says it is.
const CONTROL_FORMAT: &str = "circuitcraft-control";

/// JSON-RPC error codes of our own (the range -32000 to -32099 is for servers).
mod code {
    /// The first message wasn't a valid authentication.
    pub const UNAUTHORIZED: i64 = -32001;
    /// Too many connections or requests.
    pub const BUSY: i64 = -32002;
    /// A message longer than [`crate::MAX_MESSAGE_BYTES`].
    pub const TOO_LONG: i64 = -32003;
}

/// The control channel failed.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// The browser has no sockets.
    #[error("the control channel is not available in the browser")]
    NotInBrowser,
    /// Listening on, or connecting to, an address another machine could reach is refused.
    #[error("the control channel uses the loopback interface only (127.0.0.1 or ::1), not {0}")]
    NotLoopback(SocketAddr),
    /// A socket or the control file failed.
    #[error("control channel: {0}")]
    Io(#[from] io::Error),
    /// The system gave no random bytes for a token.
    #[error("control channel: no random bytes for a token: {0}")]
    NoRandomness(String),
    /// The control file can't be read as one.
    #[error("the app's control file {0}")]
    ControlFile(String),
    /// The app answered the authentication with an error.
    #[error("the app refused the connection: {0}")]
    Refused(String),
    /// What listens at the address authenticated the caller but didn't give the answer in the
    /// control file: it is not the app that wrote the file.
    #[error("what listens on {0} is not the CircuitCraft that wrote the control file; is the app still running?")]
    NotTheApp(SocketAddr),
    /// The app closed the connection while the caller still had something to say.
    #[error("the app closed the connection")]
    Closed,
}

/// Wakes the UI thread when a request is queued: egui's `Context::request_repaint`, in the app.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A request waiting for the UI thread.
#[derive(Debug)]
pub struct Request {
    line: String,
    reply: SyncSender<Option<Value>>,
}

impl Request {
    /// The JSON-RPC message, as it came.
    pub fn line(&self) -> &str {
        &self.line
    }

    /// Sends the answer back: the reply [`crate::handle_line`] gave, `None` for a notification.
    pub fn answer(self, reply: Option<Value>) {
        // The caller may have gone meanwhile; then nobody is waiting for it.
        let _ = self.reply.send(reply);
    }
}

/// What the control file holds besides the address, both [`TOKEN_BYTES`] random bytes in
/// hexadecimal: the token a caller proves itself with, and the answer the app proves itself with.
#[derive(Clone, PartialEq, Eq)]
pub struct Secrets {
    /// What a caller sends first.
    pub token: String,
    /// What the app sends back.
    pub answer: String,
}

impl std::fmt::Debug for Secrets {
    // Secrets stay out of logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secrets(…)")
    }
}

impl Secrets {
    fn new() -> Result<Secrets, ControlError> {
        Ok(Secrets { token: new_token()?, answer: new_token()? })
    }
}

/// Where the running app writes its address and token: `control.json` in the user's folder for
/// CircuitCraft ([`storage::user_folder`]).
pub fn control_file() -> Option<PathBuf> {
    storage::user_folder().map(|folder| folder.join("control.json"))
}

/// Reads a control file: the address the app listens on and its secrets.
pub fn read_control_file(path: &Path) -> Result<(SocketAddr, Secrets), ControlError> {
    let bytes = DiskStorage::with_recovery(None)
        .read(path, MAX_CONTROL_FILE_BYTES)
        .map_err(|e| ControlError::ControlFile(format!("{}: {e}", path.display())))?;
    let bad = |why: &str| ControlError::ControlFile(format!("{}: {why}", path.display()));
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| bad("not JSON"))?;
    if v["format"] != CONTROL_FORMAT || v["version"] != 1 {
        return Err(bad("not a control file this version reads"));
    }
    let address = v["address"].as_str().and_then(|a| a.parse::<SocketAddr>().ok()).ok_or_else(|| bad("no address"))?;
    let secret = |name: &str| v[name].as_str().filter(|t| t.len() == 2 * TOKEN_BYTES).map(str::to_owned).ok_or_else(|| bad(&format!("no {name}")));
    Ok((address, Secrets { token: secret("token")?, answer: secret("answer")? }))
}

/// What the accepting thread and the connections share with the server.
#[derive(Debug, Default)]
struct Shared {
    /// Set when the server is dropped.
    stop: AtomicBool,
    /// Every open connection, to close them all when the server is dropped.
    streams: Mutex<Vec<(u64, TcpStream)>>,
    /// The id of the next connection.
    next_id: AtomicU64,
    /// Connections that have authenticated and are open.
    agents: AtomicUsize,
}

impl Shared {
    fn streams(&self) -> MutexGuard<'_, Vec<(u64, TcpStream)>> {
        // The list stays valid if a thread panicked while holding it.
        self.streams.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The app's end of the control channel: listening from [`Server::start`] until it is dropped.
pub struct Server {
    address: SocketAddr,
    secrets: Secrets,
    requests: Receiver<Request>,
    shared: Arc<Shared>,
    /// The control file this server wrote, removed when it is dropped.
    file: Option<PathBuf>,
}

impl std::fmt::Debug for Server {
    // The token stays out of logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").field("address", &self.address).field("file", &self.file).finish_non_exhaustive()
    }
}

impl Server {
    /// Listens on `address`, which must be a loopback address (port 0 picks a free port), and
    /// writes the address and new secrets to `file`, readable by the user alone. Nothing is
    /// written unless the port is ours, so a program already listening there never learns them.
    /// `waker` is called whenever a request is queued.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start(address: SocketAddr, file: Option<&Path>, waker: Waker) -> Result<Server, ControlError> {
        if !address.ip().is_loopback() {
            return Err(ControlError::NotLoopback(address));
        }
        let listener = TcpListener::bind(address)?;
        let address = listener.local_addr()?;
        let secrets = Secrets::new()?;
        if let Some(file) = file {
            write_control_file(file, address, &secrets)?;
        }
        let (queue, requests) = sync_channel(MAX_QUEUED);
        let shared = Arc::new(Shared::default());
        let server = Server { address, secrets: secrets.clone(), requests, shared: shared.clone(), file: file.map(Path::to_path_buf) };
        std::thread::Builder::new().name("control-accept".to_owned()).spawn(move || accept(&listener, &secrets, &queue, &waker, &shared))?;
        Ok(server)
    }

    /// The browser has no sockets.
    #[cfg(target_arch = "wasm32")]
    pub fn start(_address: SocketAddr, _file: Option<&Path>, _waker: Waker) -> Result<Server, ControlError> {
        Err(ControlError::NotInBrowser)
    }

    /// Where it listens.
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// The secrets in its control file, for a caller that has them some other way (a test).
    pub const fn secrets(&self) -> &Secrets {
        &self.secrets
    }

    /// How many callers have authenticated and are connected.
    pub fn agents(&self) -> usize {
        self.shared.agents.load(Ordering::Relaxed)
    }

    /// The next request waiting for the UI thread, if there is one.
    pub fn next(&self) -> Option<Request> {
        self.requests.try_recv().ok()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        for (_, stream) in self.shared.streams().drain(..) {
            let _ = stream.shutdown(Shutdown::Both);
        }
        // Wakes the accepting thread, which then sees `stop` and closes the listener.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(200));
        // Removed only while it still holds this server's token: another app may have written its
        // own since.
        if let Some(file) = &self.file
            && read_control_file(file).is_ok_and(|(_, secrets)| secrets == self.secrets)
        {
            let _ = DiskStorage::with_recovery(None).remove(file);
        }
    }
}

/// A new token: [`TOKEN_BYTES`] random bytes from the system, in hexadecimal.
fn new_token() -> Result<String, ControlError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| ControlError::NoRandomness(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn write_control_file(file: &Path, address: SocketAddr, secrets: &Secrets) -> Result<(), ControlError> {
    if let Some(folder) = file.parent() {
        DiskStorage::with_recovery(None).create_folder(folder)?;
    }
    let text = json!({ "format": CONTROL_FORMAT, "version": 1, "address": address.to_string(), "token": secrets.token, "answer": secrets.answer })
        .to_string();
    storage::write_private(file, text.as_bytes())?;
    Ok(())
}

/// Whether two strings are equal, taking as long whatever they hold, so a caller can't learn the
/// token one character at a time from how fast it is refused.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// Accepts connections until the server is dropped.
fn accept(listener: &TcpListener, secrets: &Secrets, queue: &SyncSender<Request>, waker: &Waker, shared: &Arc<Shared>) {
    for stream in listener.incoming() {
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        let stream = match stream {
            Ok(stream) => stream,
            Err(_) => {
                // Out of file handles, say: wait a little instead of spinning.
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        // The listener is on a loopback address, so this is a second lock on the same door.
        if !stream.peer_addr().is_ok_and(|a| a.ip().is_loopback()) {
            continue;
        }
        let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut streams = shared.streams();
            if streams.len() >= MAX_CONNECTIONS {
                drop(streams);
                let mut stream = stream;
                let _ = writeln!(stream, "{}", error(Value::Null, code::BUSY, "busy: too many connections"));
                // Holds up the next connection for a moment, while every place is taken anyway.
                close_gently(stream);
                continue;
            }
            match stream.try_clone() {
                Ok(clone) => streams.push((id, clone)),
                Err(_) => continue,
            }
        }
        let (secrets, queue, waker, ours) = (secrets.clone(), queue.clone(), waker.clone(), shared.clone());
        let spawned = std::thread::Builder::new().name("control-connection".to_owned()).spawn(move || {
            let closing = stream.try_clone();
            let _ = serve_connection(stream, &secrets, &queue, &waker, &ours);
            if let Ok(stream) = closing {
                close_gently(stream);
            }
            ours.streams().retain(|(i, _)| *i != id);
        });
        if spawned.is_err() {
            shared.streams().retain(|(i, _)| *i != id);
        }
    }
}

/// A connection that gives up reading at a deadline, however its reads are spread out: a
/// timeout on the socket alone starts again with every read, so a caller sending a byte now and
/// then would never reach it. Windows leaves a socket in an undefined state after a read times
/// out, so a connection is always closed after one, never read again.
struct Timed {
    stream: TcpStream,
    /// When reading gives up; `None` reads without a limit.
    deadline: Option<Instant>,
}

impl Read for Timed {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(deadline) = self.deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(ErrorKind::TimedOut.into());
            }
            self.stream.set_read_timeout(Some(left))?;
        }
        self.stream.read(buf)
    }
}

/// Closes a connection so that the caller gets what was last sent. Closing a socket with input
/// still unread makes the system reset the connection, which can throw away the last answer (the
/// refusal, say) before the caller reads it; so the input is read and dropped first, for a moment
/// that a caller who keeps sending can't stretch.
fn close_gently(stream: TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
    let mut timed = Timed { stream, deadline: Some(Instant::now() + Duration::from_millis(500)) };
    let _ = io::copy(&mut timed, &mut io::sink());
}

/// One line read from a connection.
enum Line {
    /// The text of the line.
    Text(String),
    /// Longer than allowed.
    TooLong,
    /// The caller closed the connection.
    End,
}

/// Reads one line of at most `limit` bytes.
fn read_line(reader: &mut impl BufRead, limit: u64) -> io::Result<Line> {
    let mut bytes = Vec::new();
    let n = reader.take(limit + 1).read_until(b'\n', &mut bytes)?;
    Ok(if n == 0 {
        Line::End
    } else if n as u64 > limit {
        Line::TooLong
    } else {
        Line::Text(String::from_utf8_lossy(&bytes).into_owned())
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// The id of a JSON-RPC message, or null.
fn id_of(line: &str) -> Value {
    serde_json::from_str::<Value>(line).ok().and_then(|v| v.get("id").cloned()).unwrap_or(Value::Null)
}

/// Checks the first message: `Ok(id)` when it is the authentication with the right token.
fn authenticate(line: &str, token: &str) -> Result<Value, Value> {
    let msg: Value = serde_json::from_str(line).unwrap_or(Value::Null);
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let given = msg.get("params").and_then(|p| p.get("token")).and_then(Value::as_str);
    if msg.get("method").and_then(Value::as_str) == Some(AUTHENTICATE) && given.is_some_and(|g| same(g, token)) {
        Ok(id)
    } else {
        Err(error(id, code::UNAUTHORIZED, "unauthorized: the first message must be control/authenticate with the token in the app's control file"))
    }
}

/// Counts an authenticated connection while it is open.
struct Agent<'a>(&'a AtomicUsize);

impl<'a> Agent<'a> {
    fn new(count: &'a AtomicUsize) -> Agent<'a> {
        count.fetch_add(1, Ordering::Relaxed);
        Agent(count)
    }
}

impl Drop for Agent<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Serves one connection: the authentication, then requests, one at a time, until it closes.
fn serve_connection(stream: TcpStream, secrets: &Secrets, queue: &SyncSender<Request>, waker: &Waker, shared: &Shared) -> io::Result<()> {
    let mut reader = BufReader::new(Timed { stream: stream.try_clone()?, deadline: Some(Instant::now() + HELLO_TIMEOUT) });
    let mut writer = stream;
    let mut send = |reply: &Value| -> io::Result<()> {
        writeln!(writer, "{reply}")?;
        writer.flush()
    };
    let hello = match read_line(&mut reader, MAX_HELLO_BYTES)? {
        Line::Text(hello) => hello,
        Line::End => return Ok(()),
        Line::TooLong => return send(&error(Value::Null, code::UNAUTHORIZED, "unauthorized: the first message is too long")),
    };
    match authenticate(&hello, &secrets.token) {
        Ok(id) => send(&json!({ "jsonrpc": "2.0", "id": id, "result": {
            "serverInfo": { "name": "circuitcraft", "version": env!("CARGO_PKG_VERSION") },
            "answer": secrets.answer,
        } }))?,
        Err(refusal) => return send(&refusal),
    }
    // Lines already read past the authentication stay in the reader.
    reader.get_mut().deadline = None;
    reader.get_ref().stream.set_read_timeout(None)?;
    let _agent = Agent::new(&shared.agents);
    loop {
        let line = match read_line(&mut reader, crate::MAX_MESSAGE_BYTES)? {
            Line::Text(line) => line,
            Line::End => return Ok(()),
            // What follows can't be told apart from the rest of the long line, so the
            // connection ends.
            Line::TooLong => return send(&error(Value::Null, code::TOO_LONG, &format!("message longer than {} bytes", crate::MAX_MESSAGE_BYTES))),
        };
        if line.trim().is_empty() {
            continue;
        }
        let (reply, answer) = sync_channel(1);
        match queue.try_send(Request { line, reply }) {
            Ok(()) => waker(),
            Err(TrySendError::Full(request)) => {
                send(&error(id_of(&request.line), code::BUSY, "busy: too many requests waiting"))?;
                continue;
            }
            // The app is closing.
            Err(TrySendError::Disconnected(_)) => return Ok(()),
        }
        match answer.recv() {
            Ok(Some(reply)) => send(&reply)?,
            Ok(None) => {}
            Err(_) => return Ok(()),
        }
    }
}

/// Connects standard input and output (`input`, `output`) to the app's control channel at
/// `address`: authenticates with the token in `secrets` and checks the app's answer, then passes
/// everything on as it is, both ways, until `input` ends. This is `circuitcraft-cli mcp
/// --connect`: an MCP client that starts it talks to the running app as if it were the headless
/// server.
pub fn bridge(address: SocketAddr, secrets: &Secrets, input: impl Read + Send + 'static, mut output: impl Write) -> Result<(), ControlError> {
    if !address.ip().is_loopback() {
        return Err(ControlError::NotLoopback(address));
    }
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(Timed { stream, deadline: Some(Instant::now() + HELLO_TIMEOUT) });
    writeln!(writer, "{}", json!({ "jsonrpc": "2.0", "id": 0, "method": AUTHENTICATE, "params": { "token": secrets.token } }))?;
    let Line::Text(hello) = read_line(&mut reader, MAX_HELLO_BYTES)? else {
        return Err(ControlError::Closed);
    };
    let hello: Value = serde_json::from_str(&hello).unwrap_or(Value::Null);
    let Some(answer) = hello.get("result").map(|r| r.get("answer").and_then(Value::as_str).unwrap_or_default()) else {
        let why = hello.pointer("/error/message").and_then(Value::as_str).unwrap_or("no answer to the authentication");
        return Err(ControlError::Refused(why.to_owned()));
    };
    if !same(answer, &secrets.answer) {
        return Err(ControlError::NotTheApp(address));
    }
    reader.get_mut().deadline = None;
    reader.get_ref().stream.set_read_timeout(None)?;
    // What the client sends goes to the app as it comes; when the client is done, the app is told
    // so, answers what it still has and closes.
    let done = Arc::new(AtomicBool::new(false));
    let done_writing = done.clone();
    std::thread::Builder::new().name("control-bridge".to_owned()).spawn(move || {
        let mut input = input;
        let _ = io::copy(&mut input, &mut writer);
        done_writing.store(true, Ordering::Relaxed);
        let _ = writer.shutdown(Shutdown::Write);
    })?;
    // The app has proved itself, so what it sends is passed on without a limit: a picture can be
    // large. Each piece is flushed, so the client sees an answer as soon as it comes.
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return if done.load(Ordering::Relaxed) { Ok(()) } else { Err(ControlError::Closed) };
        }
        output.write_all(buf.get(..n).unwrap_or_default())?;
        output.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use circuitcraft_engine::Session;
    use std::io::Cursor;

    fn quiet() -> Waker {
        Arc::new(|| {})
    }

    fn server() -> Server {
        Server::start(SocketAddr::from(([127, 0, 0, 1], 0)), None, quiet()).expect("start")
    }

    /// A raw connection: a line writer and a line reader.
    fn connect(server: &Server) -> (TcpStream, BufReader<TcpStream>) {
        let stream = TcpStream::connect(server.address()).expect("connect");
        stream.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
        let reader = BufReader::new(stream.try_clone().expect("clone"));
        (stream, reader)
    }

    fn reply(reader: &mut BufReader<TcpStream>) -> Option<Value> {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => serde_json::from_str(&line).ok(),
        }
    }

    fn hello(token: &str) -> String {
        format!("{}\n", json!({ "jsonrpc": "2.0", "id": 1, "method": AUTHENTICATE, "params": { "token": token } }))
    }

    /// Plays the UI thread: answers requests against `session` until `client` has finished.
    fn pump<T>(server: &Server, session: &mut Session, client: std::thread::JoinHandle<T>) -> T {
        while !client.is_finished() {
            match server.next() {
                Some(request) => {
                    let reply = crate::handle_line(session, request.line());
                    request.answer(reply);
                }
                None => std::thread::sleep(Duration::from_millis(1)),
            }
        }
        client.join().expect("client")
    }

    #[test]
    fn an_authenticated_caller_drives_the_session() {
        let woken = Arc::new(AtomicUsize::new(0));
        let count = woken.clone();
        let server =
            Server::start(SocketAddr::from(([127, 0, 0, 1], 0)), None, Arc::new(move || _ = count.fetch_add(1, Ordering::Relaxed))).expect("start");
        let (mut stream, mut reader) = connect(&server);
        let token = server.secrets().token.to_owned();
        let client = std::thread::spawn(move || {
            stream.write_all(hello(&token).as_bytes()).expect("write");
            let hello = reply(&mut reader);
            stream
                .write_all(
                    concat!(
                        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                        "\n\n",
                        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"execute","arguments":{"command":"track.add","params":{"layer":"Top Copper","from":[0,0],"to":[5,0]}}}}"#,
                        "\n",
                        "not json\n",
                        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"execute","arguments":{"command":"board.info"}}}"#,
                        "\n",
                    )
                    .as_bytes(),
                )
                .expect("write");
            (hello, reply(&mut reader), reply(&mut reader), reply(&mut reader))
        });
        let mut session = Session::in_memory();
        let (hello, track, garbage, info) = pump(&server, &mut session, client);
        assert_eq!(hello.as_ref().map(|h| &h["result"]["serverInfo"]["name"]), Some(&json!("circuitcraft")));
        assert_eq!(hello.as_ref().map(|h| &h["result"]["answer"]), Some(&json!(server.secrets().answer)), "the app proves itself");
        assert_eq!(track.as_ref().map(|t| &t["result"]["isError"]), Some(&json!(false)), "{track:?}");
        assert_eq!(garbage.as_ref().map(|g| &g["error"]["code"]), Some(&json!(-32700)), "a bad line is answered, not fatal");
        assert_eq!(info.as_ref().map(|i| &i["id"]), Some(&json!(3)));
        assert_eq!(session.board().tracks.len(), 1, "the track is on the session's board");
        assert!(woken.load(Ordering::Relaxed) >= 3, "the UI is woken for every request");
    }

    /// Sends `first` as the first line; returns the answer and whether the connection closed.
    fn refused(server: &Server, first: &[u8]) -> (Option<Value>, bool) {
        let (mut stream, mut reader) = connect(server);
        let _ = stream.write_all(first);
        let answer = reply(&mut reader);
        // Anything sent after a refused authentication is not read as a command.
        let _ = stream.write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/call\",\"params\":{\"name\":\"execute\",\"arguments\":{\"command\":\"board.new\"}}}\n",
        );
        (answer, reply(&mut reader).is_none())
    }

    #[test]
    fn an_unauthenticated_caller_is_refused_and_nothing_runs() {
        let server = server();
        let wrong = "0".repeat(2 * TOKEN_BYTES);
        let execute_first = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"execute","arguments":{"command":"board.new"}}}"#;
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("wrong token", hello(&wrong).into_bytes()),
            ("short token", hello(&server.secrets().token[..10]).into_bytes()),
            ("no token", format!("{}\n", json!({ "jsonrpc": "2.0", "id": 1, "method": AUTHENTICATE })).into_bytes()),
            (
                "token as a number",
                format!("{}\n", json!({ "jsonrpc": "2.0", "id": 1, "method": AUTHENTICATE, "params": { "token": 5 } })).into_bytes(),
            ),
            ("a command first", format!("{execute_first}\n").into_bytes()),
            // What a web page can make a browser send to a loopback port.
            ("an HTTP request", format!("POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: text/plain\r\n\r\n{execute_first}\n").into_bytes()),
            ("not UTF-8", b"\xff\xfe\x00\n".to_vec()),
            ("a long first line", vec![b'x'; 2 * MAX_HELLO_BYTES as usize]),
        ];
        for (what, first) in cases {
            let (answer, closed) = refused(&server, &first);
            assert_eq!(answer.as_ref().map(|a| &a["error"]["code"]), Some(&json!(code::UNAUTHORIZED)), "{what}: {answer:?}");
            assert!(!answer.as_ref().is_some_and(|a| a.to_string().contains(&server.secrets().token)), "{what}: the token is never told");
            assert!(closed, "{what}: the connection is closed");
        }
        assert!(server.next().is_none(), "nothing reached the UI thread");
        assert_eq!(server.agents(), 0);
    }

    #[test]
    fn an_overlong_message_ends_the_connection() {
        let server = server();
        let (mut stream, mut reader) = connect(&server);
        stream.write_all(hello(&server.secrets().token).as_bytes()).expect("write");
        assert!(reply(&mut reader).is_some_and(|r| r.get("result").is_some()));
        let writer = std::thread::spawn(move || {
            let chunk = vec![b'x'; 1 << 20];
            for _ in 0..=(crate::MAX_MESSAGE_BYTES >> 20) {
                if stream.write_all(&chunk).is_err() {
                    break;
                }
            }
        });
        let answer = reply(&mut reader);
        assert_eq!(answer.as_ref().map(|a| &a["error"]["code"]), Some(&json!(code::TOO_LONG)), "{answer:?}");
        assert!(reply(&mut reader).is_none(), "and the connection closes");
        let _ = writer.join();
        assert!(server.next().is_none());
    }

    #[test]
    fn connections_are_limited() {
        let server = server();
        let open: Vec<_> = (0..MAX_CONNECTIONS).map(|_| connect(&server)).collect();
        // The accepting thread registers each connection before it accepts the next, so the
        // one after them is the one too many.
        let (_extra, mut reader) = connect(&server);
        let answer = reply(&mut reader);
        assert_eq!(answer.as_ref().map(|a| &a["error"]["code"]), Some(&json!(code::BUSY)), "{answer:?}");
        drop(open);
    }

    #[test]
    fn only_loopback_addresses() {
        let anywhere = SocketAddr::from(([0, 0, 0, 0], 0));
        assert!(matches!(Server::start(anywhere, None, quiet()), Err(ControlError::NotLoopback(_))));
        let remote = SocketAddr::from(([192, 0, 2, 1], 7979));
        let secrets = Secrets { token: "t".to_owned(), answer: "a".to_owned() };
        assert!(matches!(bridge(remote, &secrets, Cursor::new(Vec::new()), Vec::new()), Err(ControlError::NotLoopback(_))));
    }

    #[test]
    fn the_bridge_carries_mcp_to_the_app() {
        let server = server();
        let (address, secrets) = (server.address(), server.secrets().clone());
        let client = std::thread::spawn(move || {
            let input = concat!(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"execute","arguments":{"command":"view.camera","params":{"center":[1,2],"pixelsPerMm":10}}}}"#,
                "\n",
            );
            let mut output = Vec::new();
            let outcome = bridge(address, &secrets, Cursor::new(input.as_bytes().to_vec()), &mut output);
            (outcome.map_err(|e| e.to_string()), String::from_utf8_lossy(&output).into_owned())
        });
        let mut session = Session::in_memory();
        let (outcome, output) = pump(&server, &mut session, client);
        assert_eq!(outcome, Ok(()));
        let replies: Vec<Value> = output.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        assert_eq!(replies.len(), 2, "{output}");
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "circuitcraft");
        assert_eq!(replies[1]["result"]["isError"], false, "{output}");
        assert!(session.camera().is_some(), "the camera moved in the app's session");

        let wrong = Secrets { token: "f".repeat(2 * TOKEN_BYTES), answer: server.secrets().answer.clone() };
        let refused = bridge(server.address(), &wrong, Cursor::new(Vec::new()), Vec::new());
        assert!(matches!(refused, Err(ControlError::Refused(_))), "{refused:?}");
    }

    #[test]
    fn the_bridge_talks_only_to_the_app_that_wrote_the_file() {
        // A program that took the port after the app ended: it lets anyone in, but can't give
        // the answer from the control file.
        let impostor = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).expect("bind");
        let address = impostor.local_addr().expect("address");
        let heard = std::thread::spawn(move || {
            let (stream, _) = impostor.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut hello = String::new();
            reader.read_line(&mut hello).expect("hello");
            let mut writer = stream;
            writeln!(writer, r#"{{"jsonrpc":"2.0","id":0,"result":{{"answer":"{}"}}}}"#, "0".repeat(2 * TOKEN_BYTES)).expect("write");
            // Whatever the bridge sends next would be the agent's.
            let mut rest = String::new();
            let _ = reader.read_line(&mut rest);
            rest
        });
        let secrets = Secrets { token: "1".repeat(2 * TOKEN_BYTES), answer: "2".repeat(2 * TOKEN_BYTES) };
        let input = Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n".to_vec());
        let outcome = bridge(address, &secrets, input, Vec::new());
        assert!(matches!(outcome, Err(ControlError::NotTheApp(_))), "{outcome:?}");
        assert_eq!(heard.join().expect("impostor"), "", "the agent's messages never reached it");
    }

    #[test]
    fn a_deadline_holds_however_slowly_bytes_come() {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).expect("bind");
        let mut client = TcpStream::connect(listener.local_addr().expect("address")).expect("connect");
        let (stream, _) = listener.accept().expect("accept");
        // A byte every 50 ms, each well within any timeout on a single read.
        let drip = std::thread::spawn(move || {
            for _ in 0..40 {
                if client.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let started = Instant::now();
        let mut reader = BufReader::new(Timed { stream, deadline: Some(started + Duration::from_millis(300)) });
        let outcome = read_line(&mut reader, MAX_HELLO_BYTES);
        let took = started.elapsed();
        assert!(outcome.is_err(), "gave up at the deadline");
        assert!(took < Duration::from_millis(1500), "took {took:?}, not the two seconds the bytes kept coming");
        drop(reader);
        let _ = drip.join();
    }

    /// A folder of our own under the system temp folder, removed when dropped.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            TempDir(std::env::temp_dir().join(format!("circuitcraft-{tag}-{}-{n}", std::process::id())))
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_control_file_says_where_and_goes_with_the_server() {
        let dir = TempDir::new("control-file");
        let file = dir.0.join("nested").join("control.json");
        let server = Server::start(SocketAddr::from(([127, 0, 0, 1], 0)), Some(&file), quiet()).expect("start");
        let (address, secrets) = read_control_file(&file).expect("read");
        assert_eq!((address, &secrets), (server.address(), server.secrets()));
        assert_ne!(secrets.token, secrets.answer);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&file).expect("meta").permissions().mode() & 0o777, 0o600);
        }
        drop(server);
        assert!(!file.exists(), "removed when the server stops");

        // A file another app has written since is left alone.
        let server = Server::start(SocketAddr::from(([127, 0, 0, 1], 0)), Some(&file), quiet()).expect("start");
        let other = Secrets { token: "a".repeat(2 * TOKEN_BYTES), answer: "b".repeat(2 * TOKEN_BYTES) };
        write_control_file(&file, server.address(), &other).expect("write");
        drop(server);
        assert!(file.exists());

        for bad in ["", "{}", r#"{"format":"circuitcraft-control","version":2,"address":"127.0.0.1:1","token":"x"}"#, "\u{0}"] {
            std::fs::write(&file, bad).expect("write");
            assert!(matches!(read_control_file(&file), Err(ControlError::ControlFile(_))), "{bad:?}");
        }
    }

    #[test]
    fn tokens_are_random_and_compared_whole() {
        let (a, b) = (new_token().expect("token"), new_token().expect("token"));
        assert_eq!(a.len(), 2 * TOKEN_BYTES);
        assert_ne!(a, b);
        assert!(same(&a, &a.clone()));
        assert!(!same(&a, &b) && !same(&a, &a[..10]) && !same("", "x"));
    }

    #[test]
    fn the_secrets_stay_out_of_debug_output() {
        let server = server();
        let shown = format!("{server:?} {:?}", server.secrets());
        assert!(!shown.contains(&server.secrets().token) && !shown.contains(&server.secrets().answer));
    }
}
