//! Chrome native messaging wire format — length-prefixed UTF-8 JSON on stdio.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::{Read, Write};

const MAX_MESSAGE_BYTES: u32 = 1024 * 1024;

/// Read one native messaging frame from `reader`.
pub fn read_native_message<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, String> {
    let mut len_buf = [0u8; 4];
    reader
        .read_exact(&mut len_buf)
        .map_err(|e| format!("read message length: {e}"))?;
    let len = u32::from_ne_bytes(len_buf);
    if len == 0 {
        return Err("empty native message".to_string());
    }
    if len > MAX_MESSAGE_BYTES {
        return Err(format!("native message too large: {len} bytes"));
    }
    let mut buf = vec![0u8; len as usize];
    reader
        .read_exact(&mut buf)
        .map_err(|e| format!("read message body: {e}"))?;
    let text = String::from_utf8(buf).map_err(|e| format!("message not UTF-8: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("invalid JSON: {e}"))
}

/// Write one native messaging frame to `writer`.
pub fn write_native_message<W: Write, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), String> {
    let json = serde_json::to_vec(value).map_err(|e| format!("serialize JSON: {e}"))?;
    let len = u32::try_from(json.len())
        .map_err(|_| format!("message too large: {} bytes", json.len()))?;
    if len > MAX_MESSAGE_BYTES {
        return Err(format!("native message too large: {len} bytes"));
    }
    writer
        .write_all(&len.to_ne_bytes())
        .and_then(|_| writer.write_all(&json))
        .and_then(|_| writer.flush())
        .map_err(|e| format!("write native message: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::io::Cursor;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Ping {
        msg: String,
    }

    #[test]
    fn round_trip_native_message() {
        let ping = Ping {
            msg: "hello".to_string(),
        };
        let mut buf = Vec::new();
        write_native_message(&mut buf, &ping).unwrap();

        let mut cursor = Cursor::new(buf);
        let back: Ping = read_native_message(&mut cursor).unwrap();
        assert_eq!(back, ping);
    }
}
