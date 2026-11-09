//! The parts of RFC 6455 (WebSocket) a single-client debugging endpoint
//! needs: the opening handshake's accept key (SHA-1 and base64, written out
//! here to keep the crate free of dependencies) and text, ping, pong and
//! close frames.

use std::io::{self, Read, Write};

/// The GUID the handshake appends to the client's key (RFC 6455 section 1.3).
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`.
pub fn accept_key(client_key: &str) -> String {
    let mut input = client_key.trim().as_bytes().to_vec();
    input.extend_from_slice(GUID.as_bytes());
    base64_encode(&sha1(&input))
}

/// SHA-1 (FIPS 180-4). Used only for the handshake, not for security.
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

/// Standard base64 with padding.
pub fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub const OP_CONTINUATION: u8 = 0x0;
pub const OP_TEXT: u8 = 0x1;
pub const OP_BINARY: u8 = 0x2;
pub const OP_CLOSE: u8 = 0x8;
pub const OP_PING: u8 = 0x9;
pub const OP_PONG: u8 = 0xA;

/// One frame as read off the wire (unmasked).
pub struct Frame {
    pub fin: bool,
    pub opcode: u8,
    pub payload: Vec<u8>,
}

/// Largest message accepted from a client.
const MAX_PAYLOAD: u64 = 64 << 20;

/// Read one frame. Masked payloads (every client frame) are unmasked.
pub fn read_frame(r: &mut impl Read) -> io::Result<Frame> {
    let mut head = [0u8; 2];
    r.read_exact(&mut head)?;
    let fin = head[0] & 0x80 != 0;
    let opcode = head[0] & 0x0F;
    let masked = head[1] & 0x80 != 0;
    let len = match head[1] & 0x7F {
        126 => {
            let mut b = [0u8; 2];
            r.read_exact(&mut b)?;
            u16::from_be_bytes(b) as u64
        }
        127 => {
            let mut b = [0u8; 8];
            r.read_exact(&mut b)?;
            u64::from_be_bytes(b)
        }
        n => n as u64,
    };
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut mask = [0u8; 4];
    if masked {
        r.read_exact(&mut mask)?;
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    if masked {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
    }
    Ok(Frame {
        fin,
        opcode,
        payload,
    })
}

/// Write one final frame; `mask` is the client-side masking key (servers
/// send unmasked frames).
pub fn write_frame(
    w: &mut impl Write,
    opcode: u8,
    payload: &[u8],
    mask: Option<[u8; 4]>,
) -> io::Result<()> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | opcode);
    let mask_bit = if mask.is_some() { 0x80 } else { 0 };
    match payload.len() {
        n if n < 126 => out.push(mask_bit | n as u8),
        n if n <= u16::MAX as usize => {
            out.push(mask_bit | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(mask_bit | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    match mask {
        Some(m) => {
            out.extend_from_slice(&m);
            out.extend(payload.iter().enumerate().map(|(i, b)| b ^ m[i % 4]));
        }
        None => out.extend_from_slice(payload),
    }
    w.write_all(&out)?;
    w.flush()
}

/// Read frames until a complete text message, answering pings. `None`
/// when the peer closed the connection.
pub fn read_message(stream: &mut (impl Read + Write)) -> io::Result<Option<String>> {
    let mut message: Vec<u8> = Vec::new();
    loop {
        let frame = read_frame(stream)?;
        match frame.opcode {
            OP_TEXT | OP_BINARY | OP_CONTINUATION => {
                message.extend_from_slice(&frame.payload);
                if frame.fin {
                    return String::from_utf8(message)
                        .map(Some)
                        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"));
                }
            }
            OP_PING => write_frame(stream, OP_PONG, &frame.payload, None)?,
            OP_PONG => {}
            OP_CLOSE => {
                let _ = write_frame(stream, OP_CLOSE, &frame.payload, None);
                return Ok(None);
            }
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown opcode")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rfc_6455_handshake_example_accepts() {
        // RFC 6455 section 1.3.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn sha1_and_base64_match_known_vectors() {
        // FIPS 180-4 example "abc"; RFC 4648 section 10 vectors.
        let hex: String = sha1(b"abc").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "a9993e364706816aba3e25717850c26c9cd0d89d");
        for (input, out) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(input.as_bytes()), out);
        }
    }

    #[test]
    fn masked_frames_round_trip_at_every_length_encoding() {
        for len in [0usize, 5, 125, 126, 65535, 65536] {
            let payload: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let mut wire = Vec::new();
            write_frame(&mut wire, OP_TEXT, &payload, Some([1, 2, 3, 4])).unwrap();
            let f = read_frame(&mut wire.as_slice()).unwrap();
            assert!(f.fin);
            assert_eq!(f.opcode, OP_TEXT);
            assert_eq!(f.payload, payload);
        }
    }
}
