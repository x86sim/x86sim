//! Share: the WebSocket protocol (RFC 6455) on a std TcpStream: the handshake's accept key, reading a client's
//! (masked) frames and writing the server's (unmasked) ones. Fragmented messages are joined; a message is capped.
use std::io::{Read, Write};

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
pub const MAX_MESSAGE: usize = 1 << 20;

pub fn accept_key(client_key: &str) -> String {
    let mut h = sha1_smol::Sha1::new();
    h.update(client_key.trim().as_bytes());
    h.update(GUID.as_bytes());
    base64(&h.digest().bytes())
}

pub fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub enum Msg {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong,
    Close,
}

/// Reads one message. Err on a broken connection or a protocol error (a timeout surfaces as io::ErrorKind::WouldBlock /
/// TimedOut only between messages: a frame begun is read to its end).
pub fn read(s: &mut impl Read) -> std::io::Result<Msg> {
    let mut data = Vec::new();
    let mut first_op = None;
    loop {
        let mut h = [0u8; 2];
        if first_op.is_none() {
            s.read_exact(&mut h[..1])?;
            read_full(s, &mut h[1..])?;
        } else {
            read_full(s, &mut h)?;
        }
        let fin = h[0] & 0x80 != 0;
        let op = h[0] & 0x0f;
        let masked = h[1] & 0x80 != 0;
        let bad = |m: &str| Err(std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_string()));
        // a client's frames are masked (RFC 6455 5.1); only the defined opcodes; a continuation only after a first
        // fragment, a new data frame only when none is open
        if !masked {
            return bad("unmasked client frame");
        }
        if !matches!(op, 0 | 1 | 2 | 8 | 9 | 0xa) {
            return bad("reserved opcode");
        }
        if (op == 0 && first_op.is_none()) || (matches!(op, 1 | 2) && first_op.is_some()) {
            return bad("bad fragment sequence");
        }
        let mut len = (h[1] & 0x7f) as u64;
        if len == 126 {
            let mut b = [0u8; 2];
            read_full(s, &mut b)?;
            len = u16::from_be_bytes(b) as u64;
        } else if len == 127 {
            let mut b = [0u8; 8];
            read_full(s, &mut b)?;
            len = u64::from_be_bytes(b);
        }
        if len as usize > MAX_MESSAGE || data.len() + len as usize > MAX_MESSAGE {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "message too large"));
        }
        let mut mask = [0u8; 4];
        if masked {
            read_full(s, &mut mask)?;
        }
        let mut p = vec![0u8; len as usize];
        read_full(s, &mut p)?;
        if masked {
            for (i, b) in p.iter_mut().enumerate() {
                *b ^= mask[i & 3];
            }
        }
        match op {
            0x8 => return Ok(Msg::Close),
            0x9 => return Ok(Msg::Ping(p)),
            0xa => return Ok(Msg::Pong),
            _ => {}
        }
        if op != 0 {
            first_op = Some(op);
        }
        data.extend_from_slice(&p);
        if fin {
            return Ok(match first_op {
                Some(1) => Msg::Text(String::from_utf8_lossy(&data).into_owned()),
                _ => Msg::Binary(data),
            });
        }
    }
}

/// read_exact that keeps going through read timeouts (the socket's timeout is for idle checks between messages)
fn read_full(s: &mut impl Read, mut b: &mut [u8]) -> std::io::Result<()> {
    let mut tries = 0;
    while !b.is_empty() {
        match s.read(b) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => b = &mut b[n..],
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {
                tries += 1;
                if tries > 30 {
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub fn write(s: &mut impl Write, op: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut h = Vec::with_capacity(10);
    h.push(0x80 | op);
    let n = payload.len();
    if n < 126 {
        h.push(n as u8);
    } else if n < 65536 {
        h.push(126);
        h.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        h.push(127);
        h.extend_from_slice(&(n as u64).to_be_bytes());
    }
    s.write_all(&h)?;
    s.write_all(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_accept_key() {
        // RFC 6455 section 1.3's example
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"a"), "YQ==");
    }

    #[test]
    fn masked_and_fragmented() {
        // "Hel" + "lo" as a masked text frame and a continuation (RFC 6455 5.7)
        let mut wire = vec![0x01, 0x83, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d];
        wire.extend_from_slice(&[0x80, 0x82, 0x37, 0xfa, 0x21, 0x3d, 0x5b, 0x95]);
        match read(&mut std::io::Cursor::new(wire)).unwrap() {
            Msg::Text(t) => assert_eq!(t, "Hello"),
            _ => panic!(),
        }
        // unmasked, reserved opcode, a continuation with nothing open: refused
        for w in [vec![0x81u8, 0x01, b'x'], vec![0x83, 0x80, 0, 0, 0, 0], vec![0x80, 0x80, 0, 0, 0, 0]] {
            assert!(read(&mut std::io::Cursor::new(w)).is_err());
        }
        let mut out = vec![];
        write(&mut out, 2, &vec![7u8; 300]).unwrap();
        assert_eq!(&out[..4], &[0x82, 126, 1, 44]);
    }
}
