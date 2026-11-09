//! A debugging endpoint over TCP: the HTTP discovery routes DevTools
//! clients probe (`/json/list`, `/json/version`) and one WebSocket
//! connection at a time carrying the protocol.
//!
//! Sockets are served on background threads; messages reach the engine
//! thread through a channel that the [`Session`](crate::Session) drains via
//! [`Transport`]. `Debugger.pause` is also acted on by the socket thread
//! directly (through a [`PauseRequester`]) so it can interrupt a script that
//! is busy running and not reading messages.

use crate::ws;
use crate::{Incoming, Transport};
use agnijs_js_debugger::PauseRequester;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

enum Event {
    Message(String),
    Disconnected,
}

struct Shared {
    addr: SocketAddr,
    id: String,
    title: String,
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
    /// Listen on `addr` (port 0 picks a free port). `title` names the
    /// target in `/json/list`.
    pub fn listen(addr: impl ToSocketAddrs, title: &str) -> io::Result<CdpServer> {
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            addr,
            id: format!("agnijs-{}", addr.port()),
            title: title.to_string(),
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
        let mut writer = self.shared.writer.lock().unwrap();
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
        *self.shared.requester.lock().unwrap() = Some(requester);
    }
}

struct Request {
    path: String,
    websocket_key: Option<String>,
}

fn read_request(reader: &mut impl BufRead) -> io::Result<Request> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    let mut websocket_key = None;
    let mut upgrade = false;
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
                _ => {}
            }
        }
    }
    Ok(Request {
        path,
        websocket_key: websocket_key.filter(|_| upgrade),
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
    let Ok(request) = read_request(&mut reader) else { return };
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
        let mut writer = shared.writer.lock().unwrap();
        if writer.is_some() {
            // One client at a time.
            drop(writer);
            let _ = ws::write_frame(&mut stream, ws::OP_CLOSE, &1013u16.to_be_bytes(), None);
            return;
        }
        *writer = stream.try_clone().ok();
    }
    let mut message: Vec<u8> = Vec::new();
    while let Ok(frame) = ws::read_frame(&mut reader) {
        match frame.opcode {
            ws::OP_TEXT | ws::OP_BINARY | ws::OP_CONTINUATION => {
                message.extend_from_slice(&frame.payload);
                if !frame.fin {
                    continue;
                }
                let Ok(text) = String::from_utf8(std::mem::take(&mut message)) else {
                    break;
                };
                if is_pause_request(&text) {
                    if let Some(r) = shared.requester.lock().unwrap().as_ref() {
                        r.request();
                    }
                }
                if tx.send(Event::Message(text)).is_err() {
                    break;
                }
            }
            ws::OP_PING => {
                if let Some(w) = shared.writer.lock().unwrap().as_mut() {
                    let _ = ws::write_frame(w, ws::OP_PONG, &frame.payload, None);
                }
            }
            ws::OP_PONG => {}
            _ => {
                if let Some(w) = shared.writer.lock().unwrap().as_mut() {
                    let _ = ws::write_frame(w, ws::OP_CLOSE, &frame.payload, None);
                }
                break;
            }
        }
    }
    *shared.writer.lock().unwrap() = None;
    let _ = tx.send(Event::Disconnected);
}

fn is_pause_request(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(|m| m == "Debugger.pause"))
        .unwrap_or(false)
}
