//! The debugging endpoint refuses what a web page in a browser on the same
//! machine could send it (TASK-103.29): a foreign `Host` (DNS rebinding), a
//! cross-site `Origin`, a guessed target id. It also bounds the size of a
//! reassembled WebSocket message and listens only on loopback unless told
//! otherwise.

use agnijs_js_cdp::{ws, CdpOptions, CdpServer};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

fn connect(addr: SocketAddr) -> TcpStream {
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    s
}

/// Send a raw request head and return the status line and, unless the
/// connection was upgraded (and stays open), the rest.
fn request(addr: SocketAddr, head: &str) -> (String, String) {
    let mut s = connect(addr);
    s.write_all(head.as_bytes()).unwrap();
    let mut reader = BufReader::new(s);
    let mut status = String::new();
    let _ = reader.read_line(&mut status);
    let mut rest = String::new();
    if !status.starts_with("HTTP/1.1 101") {
        let _ = reader.read_to_string(&mut rest);
    }
    (status.trim_end().to_string(), rest)
}

fn get(addr: SocketAddr, path: &str, headers: &str) -> (String, String) {
    request(addr, &format!("GET {path} HTTP/1.1\r\n{headers}\r\n"))
}

fn upgrade_head(path: &str, headers: &str) -> String {
    format!(
        "GET {path} HTTP/1.1\r\n{headers}Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
}

fn ws_path(server: &CdpServer) -> String {
    let url = server.websocket_url();
    let addr = server.local_addr().to_string();
    url.split_once(&addr).unwrap().1.to_string()
}

fn listen() -> CdpServer {
    CdpServer::listen("127.0.0.1:0", "t").unwrap()
}

#[test]
fn loopback_hosts_are_served() {
    let server = listen();
    let addr = server.local_addr();
    let port = addr.port();
    for host in [
        addr.to_string(),
        "127.0.0.1".to_string(),
        format!("localhost:{port}"),
        "LOCALHOST".to_string(),
        format!("[::1]:{port}"),
        // Any IP literal: DNS rebinding cannot produce one.
        format!("192.168.1.20:{port}"),
    ] {
        let (status, body) = get(addr, "/json/list", &format!("Host: {host}\r\n"));
        assert!(status.starts_with("HTTP/1.1 200"), "{host}: {status}");
        assert!(body.contains("webSocketDebuggerUrl"), "{host}: {body}");
    }
}

#[test]
fn a_wrong_host_is_refused() {
    let server = listen();
    let addr = server.local_addr();
    let path = ws_path(&server);
    for host in [
        "Host: evil.example\r\n".to_string(),
        format!("Host: evil.example:{}\r\n", addr.port()),
        "Host: localhost.evil.example\r\n".to_string(),
        "Host: 127.0.0.1.nip.io\r\n".to_string(),
        "Host: [::1\r\n".to_string(),
        "Host: 127.0.0.1:x\r\n".to_string(),
        // No Host at all.
        String::new(),
    ] {
        for route in ["/json/list", "/json/version", "/json"] {
            let (status, body) = get(addr, route, &host);
            assert!(
                status.starts_with("HTTP/1.1 403"),
                "{host:?} {route}: {status}"
            );
            assert!(!body.contains("webSocketDebuggerUrl"), "{body}");
        }
        let (status, _) = request(addr, &upgrade_head(&path, &host));
        assert!(
            status.starts_with("HTTP/1.1 403"),
            "{host:?} upgrade: {status}"
        );
    }
}

#[test]
fn a_cross_site_origin_is_refused() {
    let server = listen();
    let addr = server.local_addr();
    let path = ws_path(&server);
    let host = format!("Host: {addr}\r\n");
    for origin in [
        "https://evil.example",
        "http://localhost:8080",
        format!("http://{addr}").as_str(),
        "null",
    ] {
        let headers = format!("{host}Origin: {origin}\r\n");
        let (status, _) = get(addr, "/json/list", &headers);
        assert!(status.starts_with("HTTP/1.1 403"), "{origin}: {status}");
        let (status, _) = request(addr, &upgrade_head(&path, &headers));
        assert!(
            status.starts_with("HTTP/1.1 403"),
            "{origin} upgrade: {status}"
        );
    }
    // The DevTools front end's own origins are not a web page.
    for origin in ["devtools://devtools", "chrome-devtools://devtools"] {
        let headers = format!("{host}Origin: {origin}\r\n");
        let (status, _) = get(addr, "/json/version", &headers);
        assert!(status.starts_with("HTTP/1.1 200"), "{origin}: {status}");
    }
}

#[test]
fn an_embedder_can_allow_an_origin() {
    let options = CdpOptions::new().allow_origin("https://ide.example");
    let server = CdpServer::listen_with("127.0.0.1:0", "t", options).unwrap();
    let addr = server.local_addr();
    let path = ws_path(&server);
    let ok = format!("Host: {addr}\r\nOrigin: https://ide.example\r\n");
    let (status, _) = get(addr, "/json/list", &ok);
    assert!(status.starts_with("HTTP/1.1 200"), "{status}");
    let (status, _) = request(addr, &upgrade_head(&path, &ok));
    assert!(status.starts_with("HTTP/1.1 101"), "{status}");
    let other = format!("Host: {addr}\r\nOrigin: https://evil.example\r\n");
    let (status, _) = get(addr, "/json/list", &other);
    assert!(status.starts_with("HTTP/1.1 403"), "{status}");
}

#[test]
fn the_target_id_is_random_and_a_guessed_one_is_refused() {
    let a = listen();
    let b = listen();
    let (pa, pb) = (ws_path(&a), ws_path(&b));
    assert_ne!(pa, pb);
    for (server, path) in [(&a, &pa), (&b, &pb)] {
        let port = server.local_addr().port().to_string();
        assert!(!path.contains(&port), "{path}");
        // At least 122 random bits, written as a UUID.
        let id = path.trim_start_matches('/');
        assert_eq!(id.len(), 36, "{id}");
    }
    let addr = a.local_addr();
    let host = format!("Host: {addr}\r\n");
    for guess in [
        format!("/agnijs-{}", addr.port()),
        "/".to_string(),
        pb.clone(),
        format!("{pa}x"),
    ] {
        let (status, _) = request(addr, &upgrade_head(&guess, &host));
        assert!(status.starts_with("HTTP/1.1 404"), "{guess}: {status}");
    }
    let (status, _) = request(addr, &upgrade_head(&pa, &host));
    assert!(status.starts_with("HTTP/1.1 101"), "{status}");
}

/// Open a WebSocket on `server` and return the stream past the handshake.
fn open(server: &CdpServer) -> TcpStream {
    let addr = server.local_addr();
    let mut s = connect(addr);
    s.write_all(upgrade_head(&ws_path(server), &format!("Host: {addr}\r\n")).as_bytes())
        .unwrap();
    let mut reader = BufReader::new(s.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.starts_with("HTTP/1.1 101"), "{line}");
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line.trim().is_empty() {
            break;
        }
    }
    s
}

fn write_fragment(s: &mut TcpStream, opcode: u8, fin: bool, payload: &[u8]) -> std::io::Result<()> {
    // write_frame always sets FIN; build the header here to send fragments.
    let mask = [3u8, 1, 4, 1];
    let mut out = vec![if fin { 0x80 } else { 0 } | opcode];
    assert!(payload.len() <= u16::MAX as usize);
    if payload.len() < 126 {
        out.push(0x80 | payload.len() as u8);
    } else {
        out.push(0x80 | 126);
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    s.write_all(&out)
}

/// Read until the server's close frame; return its status code.
fn close_code(s: &mut TcpStream) -> u16 {
    loop {
        let f = ws::read_frame(s).expect("the server closes with a close frame");
        if f.opcode == ws::OP_CLOSE {
            return u16::from_be_bytes([f.payload[0], f.payload[1]]);
        }
    }
}

#[test]
fn an_oversized_fragmented_message_closes_the_connection() {
    let options = CdpOptions::new().max_message_size(1000);
    let server = CdpServer::listen_with("127.0.0.1:0", "t", options).unwrap();
    let mut s = open(&server);
    let chunk = [b'x'; 400];
    // 400 + 400 + 400: every frame is small, the message is not.
    write_fragment(&mut s, ws::OP_TEXT, false, &chunk).unwrap();
    write_fragment(&mut s, ws::OP_CONTINUATION, false, &chunk).unwrap();
    let _ = write_fragment(&mut s, ws::OP_CONTINUATION, true, &chunk);
    // 1009: message too big (RFC 6455 section 7.4.1).
    assert_eq!(close_code(&mut s), 1009);
    // The slot is free again for a well-behaved client.
    std::thread::sleep(Duration::from_millis(100));
    let mut s = open(&server);
    write_fragment(&mut s, ws::OP_TEXT, false, &chunk).unwrap();
    write_fragment(&mut s, ws::OP_CONTINUATION, true, &chunk[..100]).unwrap();
    // 500 bytes is under the limit: the connection stays open.
    write_fragment(&mut s, ws::OP_PING, true, b"hi").unwrap();
    let pong = ws::read_frame(&mut s).unwrap();
    assert_eq!(
        (pong.opcode, pong.payload.as_slice()),
        (ws::OP_PONG, &b"hi"[..])
    );
}

#[test]
fn a_single_frame_over_the_limit_closes_the_connection() {
    let options = CdpOptions::new().max_message_size(1000);
    let server = CdpServer::listen_with("127.0.0.1:0", "t", options).unwrap();
    let mut s = open(&server);
    let _ = write_fragment(&mut s, ws::OP_TEXT, true, &[b'x'; 1001]);
    assert_eq!(close_code(&mut s), 1009);
}

#[test]
fn a_non_loopback_address_needs_an_explicit_opt_in() {
    let err = CdpServer::listen("0.0.0.0:0", "t").err().expect("refused");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
    let err = CdpServer::listen("[::]:0", "t").err().expect("refused");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
    // Loopback names and literals need nothing.
    CdpServer::listen("localhost:0", "t").unwrap();
    CdpServer::listen("[::1]:0", "t").ok();
    // The opt-in itself is tested without binding a public interface (a
    // firewall prompt on developer machines): see `is_loopback` users.
    assert!(!CdpOptions::new().allows_remote());
    assert!(CdpOptions::new().allow_remote(true).allows_remote());
}
