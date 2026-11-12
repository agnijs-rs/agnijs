//! A debugging endpoint over TCP: the HTTP discovery routes DevTools
//! clients probe (`/json/list`, `/json/version`) and one WebSocket
//! connection at a time carrying the protocol.
//!
//! Sockets are served on background threads; messages reach the engine
//! thread through a channel that the [`Session`](crate::Session) drains via
//! [`Transport`]. `Debugger.pause` is also acted on by the socket thread
//! directly (through a [`PauseRequester`]) so it can interrupt a script that
//! is busy running and not reading messages.
//!
//! The endpoint has no authentication, so it refuses what a web page in a
//! browser on the same machine could send it (TASK-103.29, the class of
//! Node's CVE-2018-7160):
//!
//! - a `Host` header that is not an IP literal or `localhost` (DNS
//!   rebinding needs a name), on every route;
//! - an `Origin` header, which only browsers send, unless it is the DevTools
//!   front end's or one the embedder allows ([`CdpOptions::allow_origin`]);
//! - a WebSocket path other than the target id, which is random per server.
//!
//! It listens only on loopback addresses unless the embedder opts in
//! ([`CdpOptions::allow_remote`]), and bounds the size of a reassembled
//! message ([`CdpOptions::max_message_size`]).

use crate::ws;
use crate::{Incoming, Transport};
use agnijs_js_debugger::PauseRequester;
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime};

/// Lock a mutex, recovering it if a thread panicked while holding it. The
/// guarded values (an optional socket and an optional pause flag) have no
/// invariant a panic could break, and this server runs inside a host that
/// must never see a panic escape from the debugging endpoint.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

enum Event {
    Message(String),
    Disconnected,
}

/// The default for [`CdpOptions::max_message_size`]: 64 MiB.
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 64 << 20;

/// The longest HTTP request head (request line and headers) accepted.
const MAX_REQUEST_HEAD: u64 = 64 << 10;

/// Origins the DevTools front end itself uses. They are not web pages.
const DEVTOOLS_ORIGINS: [&str; 2] = ["devtools://devtools", "chrome-devtools://devtools"];

/// How a [`CdpServer`] guards its endpoint. The defaults suit a local
/// debugging session: loopback only, no browser origins beyond the DevTools
/// front end, messages up to [`DEFAULT_MAX_MESSAGE_SIZE`].
#[derive(Clone, Debug)]
pub struct CdpOptions {
    allow_remote: bool,
    allowed_origins: Vec<String>,
    max_message_size: usize,
}

impl Default for CdpOptions {
    fn default() -> Self {
        CdpOptions {
            allow_remote: false,
            allowed_origins: Vec::new(),
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        }
    }
}

impl CdpOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow listening on an address other than loopback. Anyone who can
    /// reach it can run code with every capability the host granted: only
    /// do this on a network you trust.
    pub fn allow_remote(mut self, allow: bool) -> Self {
        self.allow_remote = allow;
        self
    }

    pub fn allows_remote(&self) -> bool {
        self.allow_remote
    }

    /// Accept requests whose `Origin` header is exactly `origin` (for
    /// example a hosted DevTools front end). Every page served from that
    /// origin can then drive the engine.
    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        self.allowed_origins.push(origin.into());
        self
    }

    /// The largest message, after reassembling its fragments, a client may
    /// send. A larger one closes the connection (status 1009).
    pub fn max_message_size(mut self, bytes: usize) -> Self {
        self.max_message_size = bytes;
        self
    }
}

/// A target id: a random (version 4) UUID, so a page cannot guess the
/// WebSocket path. The bits come from SipHash keyed by the standard
/// library's random per-process keys, over a counter and the clock.
fn random_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let hi = RandomState::new().hash_one((n, now, 0u8));
    let lo = RandomState::new().hash_one((n, now, 1u8));
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&hi.to_be_bytes());
    b[8..].copy_from_slice(&lo.to_be_bytes());
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// A `Host` header naming an IP literal or `localhost`, with an optional
/// port. DNS rebinding needs a name that resolves to this machine, so it
/// cannot produce one of these.
fn is_allowed_host(host: &str) -> bool {
    let port_ok = |p: &str| p.is_empty() || (p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()));
    if let Some(rest) = host.strip_prefix('[') {
        let Some((ip, after)) = rest.split_once(']') else {
            return false;
        };
        let port = match after.strip_prefix(':') {
            Some("") => return false,
            Some(p) => p,
            None if after.is_empty() => "",
            None => return false,
        };
        return ip.parse::<Ipv6Addr>().is_ok() && port_ok(port);
    }
    let (name, port) = match host.split_once(':') {
        Some((_, "")) => return false,
        Some(parts) => parts,
        None => (host, ""),
    };
    port_ok(port) && (name.parse::<Ipv4Addr>().is_ok() || name.eq_ignore_ascii_case("localhost"))
}

/// Whether `ip` is a loopback address (including IPv4-mapped IPv6).
pub fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

struct Shared {
    addr: SocketAddr,
    id: String,
    title: String,
    options: CdpOptions,
    /// The connected client's socket, for writing.
    writer: Mutex<Option<TcpStream>>,
    requester: Mutex<Option<PauseRequester>>,
}

/// A listening debugging endpoint. It is the [`Transport`] of a session.
pub struct CdpServer {
    shared: Arc<Shared>,
    incoming: Receiver<Event>,
}

impl CdpServer {
    /// Listen on `addr` (port 0 picks a free port) with the default
    /// [`CdpOptions`]. `title` names the target in `/json/list`. An
    /// address that is not loopback is refused (`PermissionDenied`).
    pub fn listen(addr: impl ToSocketAddrs, title: &str) -> io::Result<CdpServer> {
        Self::listen_with(addr, title, CdpOptions::default())
    }

    /// Listen on `addr` with `options`. Unless they allow remote clients,
    /// every address `addr` resolves to must be loopback.
    pub fn listen_with(
        addr: impl ToSocketAddrs,
        title: &str,
        options: CdpOptions,
    ) -> io::Result<CdpServer> {
        let addrs: Vec<SocketAddr> = addr.to_socket_addrs()?.collect();
        if !options.allow_remote {
            if let Some(a) = addrs.iter().find(|a| !is_loopback(a.ip())) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "refusing to listen on {a}, which is not a loopback address: \
                         anyone who can reach it could run code (CdpOptions::allow_remote)"
                    ),
                ));
            }
        }
        let listener = TcpListener::bind(&addrs[..])?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            addr,
            id: random_id(),
            title: title.to_string(),
            options,
            writer: Mutex::new(None),
            requester: Mutex::new(None),
        });
        let (tx, incoming) = mpsc::channel();
        let s = shared.clone();
        thread::Builder::new()
            .name("cdp-accept".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (s, tx) = (s.clone(), tx.clone());
                    let _ = thread::Builder::new()
                        .name("cdp-connection".into())
                        .spawn(move || serve_connection(stream, &s, &tx));
                }
            })?;
        Ok(CdpServer { shared, incoming })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.shared.addr
    }

    /// The URL a client connects its WebSocket to.
    pub fn websocket_url(&self) -> String {
        format!("ws://{}/{}", self.shared.addr, self.shared.id)
    }
}

impl Transport for CdpServer {
    fn send(&self, message: &str) {
        let mut writer = lock(&self.shared.writer);
        if let Some(stream) = writer.as_mut() {
            if ws::write_frame(stream, ws::OP_TEXT, message.as_bytes(), None).is_err() {
                *writer = None;
            }
        }
    }

    fn recv(&self, wait: bool) -> Incoming {
        let event = if wait {
            // Wake up now and then so a dead accept thread is noticed.
            loop {
                match self.incoming.recv_timeout(Duration::from_millis(500)) {
                    Ok(e) => break Ok(e),
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break Err(()),
                }
            }
        } else {
            match self.incoming.try_recv() {
                Ok(e) => Ok(e),
                Err(mpsc::TryRecvError::Empty) => return Incoming::Empty,
                Err(mpsc::TryRecvError::Disconnected) => Err(()),
            }
        };
        match event {
            Ok(Event::Message(m)) => Incoming::Message(m),
            Ok(Event::Disconnected) => Incoming::Disconnected,
            Err(()) => Incoming::Closed,
        }
    }

    fn set_pause_requester(&self, requester: PauseRequester) {
        *lock(&self.shared.requester) = Some(requester);
    }
}

struct Request {
    path: String,
    websocket_key: Option<String>,
    host: Option<String>,
    origin: Option<String>,
}

fn read_request(reader: &mut impl BufRead) -> io::Result<Request> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    let mut websocket_key = None;
    let mut upgrade = false;
    let mut host = None;
    let mut origin = None;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            match name.trim().to_ascii_lowercase().as_str() {
                "sec-websocket-key" => websocket_key = Some(value.to_string()),
                "upgrade" => upgrade = value.eq_ignore_ascii_case("websocket"),
                "host" => host = Some(value.to_string()),
                "origin" => origin = Some(value.to_string()),
                _ => {}
            }
        }
    }
    Ok(Request {
        path,
        websocket_key: websocket_key.filter(|_| upgrade),
        host,
        origin,
    })
}

fn http_response(stream: &mut TcpStream, status: &str, body: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=UTF-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

fn serve_connection(stream: TcpStream, shared: &Shared, tx: &Sender<Event>) {
    let _ = stream.set_nodelay(true);
    let Ok(read_half) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read_half);
    let mut stream = stream;
    let Ok(request) = read_request(&mut (&mut reader).take(MAX_REQUEST_HEAD)) else {
        return;
    };
    if !request.host.as_deref().is_some_and(is_allowed_host) {
        let body = r#"{"error":"the Host header must be an IP address or localhost"}"#;
        let _ = http_response(&mut stream, "403 Forbidden", body);
        return;
    }
    if let Some(origin) = &request.origin {
        let allowed = DEVTOOLS_ORIGINS.contains(&origin.as_str())
            || shared.options.allowed_origins.iter().any(|o| o == origin);
        if !allowed {
            let body = r#"{"error":"this Origin is not allowed"}"#;
            let _ = http_response(&mut stream, "403 Forbidden", body);
            return;
        }
    }
    let path = request.path.split('?').next().unwrap_or("").to_string();
    let Some(key) = request.websocket_key else {
        let ws_url = format!("ws://{}/{}", shared.addr, shared.id);
        let body = match path.trim_end_matches('/') {
            "/json" | "/json/list" => serde_json::json!([{
                "description": "agnijs instance",
                "id": shared.id,
                "title": shared.title,
                "type": "node",
                "url": "file://",
                "faviconUrl": "",
                "devtoolsFrontendUrl": format!("devtools://devtools/bundled/js_app.html?experiments=true&v8only=true&ws={}/{}", shared.addr, shared.id),
                "webSocketDebuggerUrl": ws_url,
            }])
            .to_string(),
            "/json/version" => serde_json::json!({
                "Browser": "agnijs",
                "Protocol-Version": "1.3",
            })
            .to_string(),
            _ => {
                let _ = http_response(&mut stream, "404 Not Found", "{}");
                return;
            }
        };
        let _ = http_response(&mut stream, "200 OK", &body);
        return;
    };
    if path != format!("/{}", shared.id) {
        let _ = http_response(&mut stream, "404 Not Found", "{}");
        return;
    }
    let handshake = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        ws::accept_key(&key)
    );
    if stream.write_all(handshake.as_bytes()).is_err() {
        return;
    }
    {
        let mut writer = lock(&shared.writer);
        if writer.is_some() {
            // One client at a time.
            drop(writer);
            let _ = ws::write_frame(&mut stream, ws::OP_CLOSE, &1013u16.to_be_bytes(), None);
            return;
        }
        *writer = stream.try_clone().ok();
    }
    let max = shared.options.max_message_size as u64;
    let too_big = || {
        // 1009: message too big (RFC 6455 section 7.4.1).
        if let Some(w) = lock(&shared.writer).as_mut() {
            let _ = ws::write_frame(w, ws::OP_CLOSE, &1009u16.to_be_bytes(), None);
        }
    };
    let mut message: Vec<u8> = Vec::new();
    loop {
        // A frame may carry no more than what is left of the message's
        // limit, and a control frame always fits (at most 125 bytes).
        let room = max.saturating_sub(message.len() as u64).max(125);
        let frame = match ws::read_frame_limited(&mut reader, room) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                too_big();
                break;
            }
            Err(_) => break,
        };
        match frame.opcode {
            ws::OP_TEXT | ws::OP_BINARY | ws::OP_CONTINUATION => {
                if message.len() as u64 + frame.payload.len() as u64 > max {
                    too_big();
                    break;
                }
                message.extend_from_slice(&frame.payload);
                if !frame.fin {
                    continue;
                }
                let Ok(text) = String::from_utf8(std::mem::take(&mut message)) else {
                    break;
                };
                if is_pause_request(&text) {
                    if let Some(r) = lock(&shared.requester).as_ref() {
                        r.request();
                    }
                }
                if tx.send(Event::Message(text)).is_err() {
                    break;
                }
            }
            ws::OP_PING => {
                if let Some(w) = lock(&shared.writer).as_mut() {
                    let _ = ws::write_frame(w, ws::OP_PONG, &frame.payload, None);
                }
            }
            ws::OP_PONG => {}
            _ => {
                if let Some(w) = lock(&shared.writer).as_mut() {
                    let _ = ws::write_frame(w, ws::OP_CLOSE, &frame.payload, None);
                }
                break;
            }
        }
    }
    *lock(&shared.writer) = None;
    let _ = tx.send(Event::Disconnected);
}

fn is_pause_request(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(|m| m == "Debugger.pause"))
        .unwrap_or(false)
}
