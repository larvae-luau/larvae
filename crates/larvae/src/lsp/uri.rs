/*!
The uri of the protocol as a path, and back out of a request.
*/

use std::path::PathBuf;

use serde_json::Value;

pub(super) fn uri_of(params: &Value) -> String {
    params["textDocument"]["uri"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/*
A `file://` uri as a path.

Only the plain form, with percent decoding for the characters that an editor
escapes. A full uri parser would be a dependency for the one scheme that
matters. A path that fails here only means that the server does not find the
project config; no other function breaks.
*/
pub(super) fn path_of_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;

    let mut bytes = Vec::with_capacity(rest.len());
    let mut it = rest.bytes();

    while let Some(b) = it.next() {
        if b != b'%' {
            bytes.push(b);
            continue;
        }

        let (hi, lo) = (it.next()?, it.next()?);

        match u8::from_str_radix(std::str::from_utf8(&[hi, lo]).ok()?, 16) {
            Ok(byte) => bytes.push(byte),
            Err(_) => bytes.extend([b'%', hi, lo]),
        }
    }

    // Decode first, then the drive check sees the real colon.
    let mut out = String::from_utf8(bytes).ok()?;

    if out.len() >= 3 && out.as_bytes()[0] == b'/' && out.as_bytes()[2] == b':' {
        out.remove(0);
    }

    Some(PathBuf::from(out))
}
