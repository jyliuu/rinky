//! Frames of calibre's Smart Device App protocol.
//!
//! Every message is `<length>[opcode, {json}]`: the decimal byte length of the JSON text
//! (UTF-8), then the JSON array itself. Book contents travel as raw bytes after an `OK`
//! to `SEND_BOOK` (calibre to device) or after `OK {fileLength}` to
//! `GET_BOOK_FILE_SEGMENT` (device to calibre); they are not framed.

use ink_plugin::serde_json::{self, json, Value};

/// Opcodes (calibre `smart_device_app/driver.py`, KOReader `wireless.lua`).
pub mod op {
    /// The opcode constants and [`name`], from one table.
    macro_rules! opcodes {
        ($($(#[$doc:meta])* $name:ident = $value:literal,)*) => {
            $($(#[$doc])* pub const $name: i64 = $value;)*

            /// Name of an opcode for logs.
            pub fn name(op: i64) -> &'static str {
                match op {
                    $($name => stringify!($name),)*
                    _ => "?",
                }
            }
        };
    }

    opcodes! {
        /// Answer.
        OK = 0,
        /// calibre stores its drive info on the device.
        SET_CALIBRE_DEVICE_INFO = 1,
        /// calibre sets the device name (unused).
        SET_CALIBRE_DEVICE_NAME = 2,
        /// calibre asks for the device info.
        GET_DEVICE_INFORMATION = 3,
        /// Total bytes of the books folder's file system.
        TOTAL_SPACE = 4,
        /// Free bytes of the books folder's file system.
        FREE_SPACE = 5,
        /// calibre asks for the book list.
        GET_BOOK_COUNT = 6,
        /// calibre sends its book lists (metadata follows).
        SEND_BOOKLISTS = 7,
        /// calibre sends a book.
        SEND_BOOK = 8,
        /// First message of calibre after the connection.
        GET_INITIALIZATION_INFO = 9,
        /// End of a book (unused with binary streaming).
        BOOK_DONE = 11,
        /// Keep-alive, metadata requests, eject.
        NOOP = 12,
        /// calibre deletes books.
        DELETE_BOOK = 13,
        /// calibre fetches a book from the device.
        GET_BOOK_FILE_SEGMENT = 14,
        /// calibre asks for one book's metadata (unused by calibre today).
        GET_BOOK_METADATA = 15,
        /// Updated metadata of one book.
        SEND_BOOK_METADATA = 16,
        /// A message for the user; kind 1 = wrong password.
        DISPLAY_MESSAGE = 17,
        /// calibre is busy.
        CALIBRE_BUSY = 18,
        /// Name and id of calibre's current library.
        SET_LIBRARY_INFO = 19,
        /// An error (both directions, calibre 4.18 and newer).
        ERROR = 20,
    }
}

/// Largest JSON frame accepted (book metadata with a cover thumbnail is well below).
pub const MAX_FRAME: usize = 32 << 20;
/// Most digits of a length prefix (`MAX_FRAME` has 8).
pub const MAX_DIGITS: usize = 10;

/// One decoded message.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Opcode ([`op`]).
    pub op: i64,
    /// The argument object (`{}` when calibre sent something else).
    pub arg: Value,
}

/// A frame that cannot be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The length prefix is not `<digits>[`: the stream is out of step; close it.
    BadPrefix(String),
    /// The length is larger than [`MAX_FRAME`]; close the stream.
    TooLarge(u64),
    /// The frame had a valid length but its JSON is broken or not `[int, ...]`. The
    /// frame is skipped (`len` bytes consumed); the stream stays usable.
    BadJson {
        /// Bytes to drop (prefix + JSON).
        len: usize,
        /// What was wrong.
        why: String,
    },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::BadPrefix(s) => write!(f, "bad frame length prefix {s:?}"),
            FrameError::TooLarge(n) => write!(f, "frame of {n} bytes is larger than {MAX_FRAME}"),
            FrameError::BadJson { why, .. } => write!(f, "bad JSON frame: {why}"),
        }
    }
}

/// Encodes `[op, data]` with its length prefix.
pub fn encode(op: i64, data: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(&json!([op, data])).unwrap_or_else(|_| format!("[{op},{{}}]").into_bytes());
    let mut out = body.len().to_string().into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Decodes the frame at the start of `buf`: `Ok(None)` when more bytes are needed,
/// `Ok(Some((frame, used)))` when one is complete.
pub fn decode(buf: &[u8]) -> Result<Option<(Frame, usize)>, FrameError> {
    let digits = buf.iter().take_while(|b| b.is_ascii_digit()).count();
    let shown = || String::from_utf8_lossy(&buf[..buf.len().min(MAX_DIGITS + 2)]).into_owned();
    if digits > MAX_DIGITS {
        return Err(FrameError::BadPrefix(shown()));
    }
    match buf.get(digits) {
        None => return Ok(None),
        Some(b'[') if digits > 0 => {}
        Some(_) => return Err(FrameError::BadPrefix(shown())),
    }
    let len: u64 = std::str::from_utf8(&buf[..digits]).ok().and_then(|s| s.parse().ok()).ok_or_else(|| FrameError::BadPrefix(shown()))?;
    if len > MAX_FRAME as u64 {
        return Err(FrameError::TooLarge(len));
    }
    let len = len as usize;
    if buf.len() < digits + len {
        return Ok(None);
    }
    let used = digits + len;
    let v: Value = serde_json::from_slice(&buf[digits..used]).map_err(|e| FrameError::BadJson { len: used, why: e.to_string() })?;
    let Value::Array(mut a) = v else { return Err(FrameError::BadJson { len: used, why: "not an array".into() }) };
    let Some(op) = a.first().and_then(Value::as_i64) else { return Err(FrameError::BadJson { len: used, why: "no opcode".into() }) };
    let arg = if a.len() > 1 { a.swap_remove(1) } else { Value::Null };
    let arg = if arg.is_object() { arg } else { json!({}) };
    Ok(Some((Frame { op, arg }, used)))
}

/// Lowercase hex SHA-1 of `text`.
pub fn sha1_hex(text: &str) -> String {
    let d = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, text.as_bytes());
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// `passwordHash` of the init info: SHA-1 of password + challenge, `""` without a
/// password or challenge (KOReader `getInitInfo`).
pub fn password_hash(password: &str, challenge: &str) -> String {
    if password.is_empty() || challenge.is_empty() {
        String::new()
    } else {
        sha1_hex(&format!("{password}{challenge}"))
    }
}

/// Parses calibre's answer to a discovery broadcast,
/// `calibre wireless device client (on HOST);OPDS_PORT,SOCKET_PORT`: `(HOST, SOCKET_PORT)`.
pub fn parse_discovery(reply: &str) -> Option<(String, u16)> {
    let reply = reply.trim();
    let rest = reply.strip_prefix("calibre wireless device client (on ")?;
    let (host, ports) = rest.rsplit_once(");")?;
    let (_, port) = ports.split_once(',')?;
    Some((host.to_string(), port.trim().parse().ok()?))
}

/// True when `lpath` is a safe relative path inside the books folder: not empty, not
/// absolute, no `..` or `.` parts, no NUL, no backslash, no empty parts.
pub fn safe_lpath(lpath: &str) -> bool {
    !lpath.is_empty()
        && lpath.len() <= 1024
        && !lpath.starts_with('/')
        && !lpath.contains(['\\', '\0'])
        && lpath.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}
