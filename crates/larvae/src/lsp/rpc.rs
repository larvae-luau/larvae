/*!
The wire: message framing, and the conversion of a byte offset to a position.

LSP is JSON-RPC over a stream with `Content-Length` headers, and that is
small enough to write here. This keeps an async runtime and a protocol crate
out of the binary, and this module is the full set of what they would
provide.

Positions need care. LSP counts a character as a UTF-16 code unit by default,
not a byte and not a `char`. So a line that holds an emoji differs in all
three counts. An incorrect count puts every diagnostic on that line in the
wrong place, and only for users whose files hold non-ASCII text.
*/

use std::io::{BufRead, Write};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One JSON-RPC message, request or notification
#[derive(Debug, Deserialize)]
pub struct Message {
    /// Absent on a notification; a notification expects no reply
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Serialize)]
struct Response<'a> {
    jsonrpc: &'static str,
    id: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ResponseError>,
}

#[derive(Serialize)]
struct ResponseError {
    code: i64,
    message: String,
}

#[derive(Serialize)]
struct Notification<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    params: Value,
}

/*
Read one message, or `None` when the stream ends.

An editor that shuts down closes its end of the stream. So a closed stream
is a normal end and not an error.
*/
pub fn read(input: &mut impl BufRead) -> Result<Option<Message>> {
    let mut length: Option<usize> = None;

    loop {
        let mut line = String::new();

        if input.read_line(&mut line).context("reading a header")? == 0 {
            return Ok(None);
        }

        let line = line.trim_end();

        if line.is_empty() {
            break;
        }

        if let Some(value) = line
            .strip_prefix("Content-Length:")
            .or_else(|| line.strip_prefix("content-length:"))
        {
            length = Some(
                value
                    .trim()
                    .parse()
                    .context("Content-Length is not a number")?,
            );
        }
    }

    let Some(length) = length else {
        bail!("a message arrived with no Content-Length");
    };

    let mut body = vec![0u8; length];
    input
        .read_exact(&mut body)
        .context("reading a message body")?;

    /*
    The server skips a message that it cannot parse, and does not stop. An
    editor that sends unexpected data must not stop the server in the middle
    of a session, and the next message is very likely good.
    */
    Ok(serde_json::from_slice(&body).ok())
}

pub fn respond(out: &mut impl Write, id: &Value, result: Value) -> Result<()> {
    send(
        out,
        serde_json::to_value(Response {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        })?,
    )
}

pub fn respond_error(out: &mut impl Write, id: &Value, message: String) -> Result<()> {
    send(
        out,
        serde_json::to_value(Response {
            jsonrpc: "2.0",
            id,
            result: None,
            // -32603 is the internal error code, and an unhandled failure is one.
            error: Some(ResponseError {
                code: -32603,
                message,
            }),
        })?,
    )
}

pub fn notify(out: &mut impl Write, method: &str, params: Value) -> Result<()> {
    send(
        out,
        serde_json::to_value(Notification {
            jsonrpc: "2.0",
            method,
            params,
        })?,
    )
}

fn send(out: &mut impl Write, value: Value) -> Result<()> {
    let body = serde_json::to_vec(&value)?;

    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(&body)?;
    out.flush()?;

    Ok(())
}

/*
Byte offsets to LSP positions, for one document.

The table builds once for each analysis and not once for each diagnostic.
The alternative is a scan from the start of the file for every span, and a
file with a hundred findings would scan a hundred times.
*/
pub struct Lines {
    /// Byte offset where each line starts
    starts: Vec<u32>,
}

impl Lines {
    pub fn new(src: &str) -> Self {
        let mut starts = vec![0u32];

        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i as u32 + 1);
            }
        }

        Self { starts }
    }

    /// Zero based line and UTF-16 character for a byte offset
    pub fn position(&self, src: &str, byte: u32) -> (u32, u32) {
        let byte = byte.min(src.len() as u32);
        let line = self.starts.partition_point(|&s| s <= byte) - 1;
        let start = self.starts[line] as usize;

        // UTF-16 code units, the meaning of character in the protocol
        let character = src
            .get(start..byte as usize)
            .unwrap_or("")
            .chars()
            .map(char::len_utf16)
            .sum::<usize>();

        (line as u32, character as u32)
    }

    pub fn range(&self, src: &str, span: (u32, u32)) -> Value {
        let (start_line, start_char) = self.position(src, span.0);
        let (end_line, end_char) = self.position(src, span.1);

        serde_json::json!({
            "start": { "line": start_line, "character": start_char },
            "end": { "line": end_line, "character": end_char },
        })
    }

    /// A range that covers the whole document, for a full document edit
    pub fn whole(&self, src: &str) -> Value {
        self.range(src, (0, src.len() as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    fn framed(body: &str) -> String {
        format!("Content-Length: {}\r\n\r\n{body}", body.len())
    }

    #[test]
    fn a_framed_message_is_read() {
        let text = framed(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
        let mut input = BufReader::new(text.as_bytes());

        let msg = read(&mut input).unwrap().expect("a message");

        assert_eq!(msg.method, "initialize");
        assert_eq!(msg.id, Some(serde_json::json!(1)));
    }

    #[test]
    fn a_notification_has_no_id() {
        let text = framed(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#);
        let mut input = BufReader::new(text.as_bytes());

        assert_eq!(read(&mut input).unwrap().unwrap().id, None);
    }

    #[test]
    fn two_messages_read_in_order() {
        let text = format!(
            "{}{}",
            framed(r#"{"method":"one"}"#),
            framed(r#"{"method":"two"}"#)
        );

        let mut input = BufReader::new(text.as_bytes());

        assert_eq!(read(&mut input).unwrap().unwrap().method, "one");
        assert_eq!(read(&mut input).unwrap().unwrap().method, "two");
        assert!(read(&mut input).unwrap().is_none());
    }

    /// An editor that shuts down closes the stream; that is not an error
    #[test]
    fn an_empty_stream_ends_cleanly() {
        assert!(read(&mut BufReader::new(&b""[..])).unwrap().is_none());
    }

    #[test]
    fn a_content_type_header_is_ignored() {
        let body = r#"{"method":"x"}"#;
        let text = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc\r\n\r\n{body}",
            body.len()
        );

        let mut input = BufReader::new(text.as_bytes());

        assert_eq!(read(&mut input).unwrap().unwrap().method, "x");
    }

    /// One bad message must not end the session
    #[test]
    fn an_unparsable_body_is_skipped_rather_than_fatal() {
        let text = format!(
            "{}{}",
            framed("not json at all"),
            framed(r#"{"method":"ok"}"#)
        );
        let mut input = BufReader::new(text.as_bytes());

        assert!(read(&mut input).unwrap().is_none(), "the bad one");
        assert_eq!(read(&mut input).unwrap().unwrap().method, "ok");
    }

    #[test]
    fn a_response_is_framed_with_its_length() {
        let mut out = Vec::new();
        respond(
            &mut out,
            &serde_json::json!(7),
            serde_json::json!({"ok": true}),
        )
        .unwrap();

        let text = String::from_utf8(out).unwrap();
        let (headers, body) = text.split_once("\r\n\r\n").expect("framed");

        assert_eq!(
            headers,
            format!("Content-Length: {}", body.len()),
            "the length has to match the body"
        );
        assert!(body.contains(r#""id":7"#));
    }

    // --- positions ---------------------------------------------------------

    #[test]
    fn a_position_counts_lines_from_zero() {
        let src = "local a = 1\nlocal b = 2\n";
        let lines = Lines::new(src);

        assert_eq!(lines.position(src, 0), (0, 0));
        assert_eq!(lines.position(src, 6), (0, 6));
        assert_eq!(lines.position(src, 12), (1, 0));
        assert_eq!(lines.position(src, 18), (1, 6));
    }

    /*
    The important test. An emoji is four bytes, one char, and two UTF-16 code
    units. LSP means only the last count.
    */
    #[test]
    fn a_character_is_a_utf16_code_unit() {
        let src = "local s = \"🤔\" -- here\n";
        let lines = Lines::new(src);

        let after_emoji = src.find("\" --").unwrap() as u32;

        assert_eq!(
            lines.position(src, after_emoji),
            (0, 13),
            "10 for the prefix, 1 for the quote, 2 for the emoji"
        );
    }

    #[test]
    fn a_two_byte_character_is_one_code_unit() {
        let src = "local s = \"é\"\n";
        let lines = Lines::new(src);

        let close = src.rfind('"').unwrap() as u32;

        assert_eq!(lines.position(src, close), (0, 12));
    }

    #[test]
    fn an_offset_past_the_end_clamps_rather_than_panicking() {
        let src = "local a = 1\n";
        let lines = Lines::new(src);

        assert_eq!(
            lines.position(src, 9_999),
            lines.position(src, src.len() as u32)
        );
    }

    #[test]
    fn a_whole_document_range_starts_at_the_top_and_ends_at_the_bottom() {
        let src = "local a = 1\nlocal b = 2\n";
        let whole = Lines::new(src).whole(src);

        assert_eq!(whole["start"]["line"], 0);
        assert_eq!(whole["start"]["character"], 0);
        assert_eq!(whole["end"]["line"], 2, "the trailing newline opens a line");
    }

    #[test]
    fn a_span_becomes_a_range() {
        let src = "local abc = 1\n";
        let range = Lines::new(src).range(src, (6, 9));

        assert_eq!(range["start"]["character"], 6);
        assert_eq!(range["end"]["character"], 9);
    }
}
