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

#[cfg(test)]
mod decode {
    use super::*;

    /*
    The Windows forms an editor sends, decoded to a usable path.

    The root uri is what the server joins `larvae.toml` onto, so a
    drive letter it decodes wrong is a project whose whole config goes
    unread. VS Code percent encodes the colon and lowercases the
    drive; other clients send it raw. Both are the same path.
    */
    #[test]
    fn a_windows_root_decodes_to_a_drive_path() {
        for uri in [
            "file:///c%3A/Users/me/project",
            "file:///C:/Users/me/project",
        ] {
            let found = path_of_uri(uri).expect("decodes");
            let text = found.to_string_lossy().to_ascii_lowercase();

            assert_eq!(text, "c:/users/me/project", "{uri} decoded to {found:?}");
        }

        // A space in a Windows path survives the same way.
        let found = path_of_uri("file:///c%3A/Users/me/My%20Game").expect("decodes");

        assert_eq!(
            found.to_string_lossy().to_ascii_lowercase(),
            "c:/users/me/my game"
        );
    }
}
