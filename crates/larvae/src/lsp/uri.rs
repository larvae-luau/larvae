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

/*
The uri of a path, for a reply that names a file the editor did not open.

A document link points at a module the user has not opened, so the server
has to build the uri rather than echo one back. The encoding is the reverse
of `path_of_uri`: percent encode the bytes a uri reserves, and leave the
rest, so a round trip through the two gives the path back.
*/
pub(super) fn uri_of_path(path: &std::path::Path) -> Option<String> {
    let text = path.to_str()?;
    let mut out = String::from("file://");

    /*
    A Windows path opens with a drive letter and no separator, and the uri
    form needs one. `path_of_uri` drops it again on the way back.
    */
    if !text.starts_with('/') {
        out.push('/');
    }

    for byte in text.bytes() {
        match byte {
            b'/' | b'-' | b'.' | b'_' | b'~' | b':' => out.push(byte as char),

            b if b.is_ascii_alphanumeric() => out.push(b as char),

            // A space, and every byte of a name that is not ASCII.
            b => out.push_str(&format!("%{b:02X}")),
        }
    }

    Some(out)
}

#[cfg(test)]
mod round_trip {
    use super::*;
    use std::path::Path;

    /// The two functions have to agree, or a link points at nothing.
    #[test]
    fn a_path_survives_the_round_trip() {
        for path in [
            "/home/a/src/main.luau",
            "/home/a/my folder/x.luau",
            "/home/a/\u{e9}t\u{e9}.luau",
        ] {
            let uri = uri_of_path(Path::new(path)).expect("encodes");

            assert_eq!(
                path_of_uri(&uri).as_deref(),
                Some(Path::new(path)),
                "{uri} did not decode back"
            );
        }
    }

    /// A space must not survive as a raw space; an editor rejects that uri.
    #[test]
    fn a_space_is_encoded() {
        let uri = uri_of_path(Path::new("/a b/c.luau")).expect("encodes");

        assert!(!uri.contains(' '), "{uri}");
        assert!(uri.contains("%20"), "{uri}");
    }
}
