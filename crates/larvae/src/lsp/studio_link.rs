/*!
The listener the Roblox Studio plugin talks to.

Studio cannot listen on a port, so the plugin is the client and larvae is the
server. It POSTs JSON to `127.0.0.1` and reads one answer per request. The
whole protocol lives in the plugin repository, under `docs/PROTOCOL.md`, and
this module implements the server half of it.

The HTTP here is written out rather than taken from a crate. The surface is
one method, one content type, four paths and one loopback address. A general
server would be a large dependency for that, and the parsing a general server
does is the part this does not need.

The listener runs on a thread of its own, because the language server blocks
on stdin. It writes into a shared store, and the server reads that store when
it next answers a request. So a tree that changed while the author was away
reaches the type checker on the next keystroke, and nothing has to interrupt
a blocking read.
*/

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::studio::{Answer, Message, Session};

/// What the listener and the server share
#[derive(Default)]
pub struct Store {
    /// The one live session. Studio runs one place at a time per link.
    pub session: Option<Session>,
    /*
    Set when the tree changed and the type checker has not seen it.

    The listener cannot call the analyzer: the analyzer lives on the server
    thread behind a `RefCell`, and it is mid-request whenever it is busy. So
    the listener raises a flag and the server lowers it when it next has the
    analyzer in hand.
    */
    pub dirty: bool,
}

pub struct Link {
    pub store: Arc<Mutex<Store>>,
    /// Cleared on drop, so the thread stops with the server
    stop: Arc<AtomicBool>,
    port: u16,
}

impl Link {
    /*
    Open the listener, or report why it did not open.

    A port already in use is the common failure, and it is not fatal: another
    larvae is running, or the user picked a port something else holds. The
    server keeps working without the link, so the caller reports the reason
    and carries on.
    */
    pub fn start(port: u16) -> std::io::Result<Self> {
        // Loopback only. The tree of a place is not something to serve a network.
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let port = listener.local_addr()?.port();

        let store = Arc::new(Mutex::new(Store::default()));
        let stop = Arc::new(AtomicBool::new(false));

        let thread_store = Arc::clone(&store);
        let thread_stop = Arc::clone(&stop);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if thread_stop.load(Ordering::Relaxed) {
                    return;
                }

                let Ok(stream) = stream else {
                    continue;
                };

                // One request at a time is the protocol's own promise, so no pool.
                serve(stream, &thread_store);
            }
        });

        Ok(Self { store, stop, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Takes the dirty flag, so the caller refreshes the definitions once
    pub fn take_dirty(&self) -> bool {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());

        std::mem::take(&mut store.dirty)
    }

    /// The declaration text for the tree, or nothing when no place is linked
    pub fn definitions(&self) -> Option<String> {
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());

        store.session.as_ref().map(super::studio::definitions)
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);

        // The thread parks in `accept`, so one connection wakes it to see the flag.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// The path prefix every endpoint sits under
const BASE: &str = "/larvae/studio/v1";

/*
Read one request, answer it, and drop the connection.

The plugin sends one request at a time and waits for the answer, so there is
no pipelining to handle and no keep alive to manage.
*/
fn serve(stream: TcpStream, store: &Arc<Mutex<Store>>) {
    let Ok(peer) = stream.try_clone() else {
        return;
    };

    let mut reader = BufReader::new(stream);
    let mut request = String::new();

    if reader.read_line(&mut request).is_err() {
        return;
    }

    // "POST /larvae/studio/v1/full HTTP/1.1"
    let path = request
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let is_post = request.starts_with("POST ");

    let mut length = 0usize;

    loop {
        let mut line = String::new();

        match reader.read_line(&mut line) {
            Ok(0) => break,

            Ok(_) => {}

            Err(_) => return,
        }

        let trimmed = line.trim_end();

        if trimmed.is_empty() {
            break;
        }

        if let Some(value) = header(trimmed, "content-length") {
            length = value.trim().parse().unwrap_or(0);
        }
    }

    /*
    A body larger than this is not a Studio message. The cap stops a stray
    client from asking larvae to hold a gigabyte, and the plugin's own chunk
    size keeps a real message far below it.
    */
    const MAX_BODY: usize = 64 * 1024 * 1024;

    if length > MAX_BODY {
        let _ = write_answer(&peer, 413, &json!({ "ok": false }));

        return;
    }

    let mut body = vec![0u8; length];

    if reader.read_exact(&mut body).is_err() {
        return;
    }

    let (status, answer) = match is_post {
        true => handle(&path, &body, store),

        // The plugin only ever posts, so anything else is not the plugin.
        false => (405, json!({ "ok": false })),
    };

    let _ = write_answer(&peer, status, &answer);
}

/// The value of one header, matched without case
fn header<'a>(line: &'a str, want: &str) -> Option<&'a str> {
    let (name, value) = line.split_once(':')?;

    name.trim().eq_ignore_ascii_case(want).then_some(value)
}

/*
Dispatch one message to the session store.

An unknown path answers 404, which the protocol gives a meaning: the plugin
reads it as a build without the Studio endpoint and waits longer between
tries. So a larvae that does not speak this version costs the user a slow
retry and not a busy loop.
*/
fn handle(path: &str, body: &[u8], store: &Arc<Mutex<Store>>) -> (u16, Value) {
    let Some(endpoint) = path.strip_prefix(BASE) else {
        return (404, json!({ "ok": false }));
    };

    let endpoint = endpoint.trim_start_matches('/');

    if !matches!(endpoint, "hello" | "full" | "delta" | "bye") {
        return (404, json!({ "ok": false }));
    }

    let Ok(message) = serde_json::from_slice::<Message>(body) else {
        return (400, json!({ "ok": false }));
    };

    let mut store = store.lock().unwrap_or_else(|e| e.into_inner());

    if endpoint == "bye" {
        store.session = None;
        store.dirty = true;

        return (200, describe(Answer::Ok));
    }

    let id = message.session().to_string();

    /*
    A hello starts the session over, whether or not larvae knew the id. The
    protocol names the case: Studio reloaded the plugin and sent a hello for
    a session larvae already holds.
    */
    let fresh = endpoint == "hello";
    let known = store.session.as_ref().is_some_and(|s| s.id() == id);

    if fresh || !known {
        /*
        A `full` or `delta` for a session larvae does not know is a restart
        of larvae, and the protocol says to ask for the whole tree again.
        */
        if !fresh {
            store.session = Some(Session::new(id));
            store.dirty = true;

            return (200, describe(Answer::Resync));
        }

        store.session = Some(Session::new(id));
    }

    let session = store.session.as_mut().expect("a session exists");
    let answer = session.apply(&message);

    store.dirty = true;

    (200, describe(answer))
}

/*
The answer body.

`server` names larvae and its version, which the plugin shows to the user, so
a stale binary is visible in Studio without reading a log.
*/
fn describe(answer: Answer) -> Value {
    let mut out = json!({
        "ok": true,
        "server": { "name": "larvae-lsp", "version": env!("CARGO_PKG_VERSION") },
    });

    if answer == Answer::Resync {
        out["resync"] = json!(true);
    }

    out
}

fn write_answer(mut stream: &TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    let text = body.to_string();

    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "Error",
    };

    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{text}",
        text.len()
    )?;

    stream.flush()
}

#[cfg(test)]
mod link {
    use super::*;
    use std::io::BufRead;

    /// Speak one request the way the plugin speaks it, and read the answer.
    fn post(port: u16, endpoint: &str, body: &str) -> (u16, Value) {
        use std::net::TcpStream;

        let mut socket = TcpStream::connect(("127.0.0.1", port)).expect("connects");

        write!(
            socket,
            "POST {BASE}/{endpoint} HTTP/1.1\r\n\
             Host: 127.0.0.1\r\n\
             Content-Type: application/json\r\n\
             X-Larvae-Session: test\r\n\
             X-Larvae-Protocol: 1\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .expect("writes");

        socket.flush().expect("flushes");

        let mut reader = BufReader::new(socket);
        let mut status_line = String::new();

        reader.read_line(&mut status_line).expect("reads a status");

        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .expect("a status code");

        let mut length = 0usize;

        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("reads a header");

            if line.trim_end().is_empty() {
                break;
            }

            if let Some(value) = header(line.trim_end(), "content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
        }

        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).expect("reads the body");

        (status, serde_json::from_slice(&body).expect("json"))
    }

    fn hello(seq: u64) -> String {
        format!(r#"{{"v":1,"kind":"hello","session":"s1","seq":{seq},"roots":[]}}"#)
    }

    /*
    A place reaches larvae over the wire, and comes back as Luau types.

    The model is tested on its own with decoded messages. This is the other
    half: the socket, the framing and the dispatch, driven the way the plugin
    drives them.
    */
    #[test]
    fn a_place_arrives_over_http() {
        // Port 0 asks the system for a free one, so a busy 3773 cannot fail this.
        let link = Link::start(0).expect("the listener opens");
        let port = link.port();

        let (status, answer) = post(port, "hello", &hello(1));

        assert_eq!(status, 200);
        assert_eq!(answer["ok"], true);
        assert_eq!(answer["server"]["name"], "larvae-lsp");

        let full = r#"{"v":1,"kind":"full","session":"s1","seq":2,"chunk":1,"final":true,
            "root":1,"classes":["DataModel","Workspace","Part"],
            "nodes":[{"i":1,"p":0,"c":1,"n":"Place"},
                     {"i":2,"p":1,"c":2,"n":"Workspace"},
                     {"i":3,"p":2,"c":3,"n":"Baseplate"}]}"#;

        let (status, answer) = post(port, "full", full);

        assert_eq!(status, 200);
        assert_eq!(
            answer["resync"],
            Value::Null,
            "a clean snapshot needs no resync"
        );

        assert!(link.take_dirty(), "the tree changed, so the flag is up");

        let text = link.definitions().expect("a session is linked");

        assert!(
            text.contains("Baseplate"),
            "the part did not reach the types: {text}"
        );
    }

    /// A snapshot for a session larvae never greeted asks for the tree again.
    #[test]
    fn an_unknown_session_is_asked_to_resync() {
        let link = Link::start(0).expect("the listener opens");

        let full = r#"{"v":1,"kind":"full","session":"ghost","seq":9,"chunk":1,"final":true,
            "root":1,"classes":["DataModel"],"nodes":[{"i":1,"p":0,"c":1,"n":"P"}]}"#;

        let (status, answer) = post(link.port(), "full", full);

        assert_eq!(status, 200);
        assert_eq!(answer["resync"], true);
    }

    /*
    A path the build does not serve answers 404, and the protocol gives that
    a meaning: the plugin waits longer between tries instead of spinning.
    */
    #[test]
    fn an_unknown_endpoint_is_a_404() {
        let link = Link::start(0).expect("the listener opens");

        assert_eq!(post(link.port(), "nonsense", "{}").0, 404);
    }

    /// A body that is not a message is refused, and the session is untouched.
    #[test]
    fn a_malformed_body_is_a_400() {
        let link = Link::start(0).expect("the listener opens");

        post(link.port(), "hello", &hello(1));

        assert_eq!(post(link.port(), "delta", "not json at all").0, 400);
    }

    /// `bye` drops the place, so the types stop describing a closed session.
    #[test]
    fn a_bye_drops_the_session() {
        let link = Link::start(0).expect("the listener opens");

        post(link.port(), "hello", &hello(1));
        post(
            link.port(),
            "bye",
            r#"{"v":1,"kind":"bye","session":"s1","seq":2}"#,
        );

        assert!(link.definitions().is_none(), "the session outlived its bye");
    }

    /// Two links cannot hold one port, and the second says so rather than panicking.
    #[test]
    fn a_taken_port_reports_rather_than_panics() {
        let first = Link::start(0).expect("the listener opens");

        assert!(
            Link::start(first.port()).is_err(),
            "the port was taken twice"
        );
    }
}
