/*!
A worm that is an ordinary executable, spoken to over a pipe.

The third form, beside the embedded Luau VM and the wasm interpreter, and the
one to reach for when a worm does enough real work that an interpreter's tax on
it matters. It buys native speed and costs three things worth stating plainly:
an artifact per platform, no sandbox, and a process to keep alive.

**No sandbox.** A wasm worm cannot read your SSH keys; this one runs with
everything you can reach. That is why `wasm` stays the default and this is opt
in per worm, for code a project actually trusts.

## The protocol

A 4 byte little endian length, then that many bytes of JSON, in both
directions. Not the LSP's text headers: this is a private channel between two
programs that ship together, so there is nothing to negotiate and a fixed
prefix is less to get wrong.

Requests are one per file, never one per node. That is the whole reason this is
affordable, and it is the same shape the wasm side is moving to: the measured
cost of the old per node protocol was 24µs a crossing, and a rule worm paid it
120 times per file.

## Concurrency

One process per instance, and instances are already per rayon worker because
`mlua::Lua` is `!Send`. So a worker owns its child outright and no request ever
overlaps another on the same pipe. That is why there are no request ids here:
the design question in the plan, one process per worker or ids over one pipe,
answers itself once you notice the pool is already thread local.
*/

use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::proto;

/// What the host asks a worm to do
#[derive(Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request<'a> {
    /// Settings and enabled rules, once, before any file. `doc_version` is
    /// the layout contract the host speaks; a worm that never formats is
    /// free to ignore it.
    Init {
        config: &'a str,
        rules: &'a str,
        doc_version: u32,
    },
    /// Turn a claimed file into Luau
    Transform { source: &'a str },
    /// Lay a claimed file out, replying with a document to render
    Format { source: &'a str },
    /// Report a claimed file's problems, severity left to the host
    Lint { source: &'a str },
}

/// What comes back, one struct for every op since the fields are disjoint
#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    ok: bool,
    /// Present when `ok`, for `transform`
    #[serde(default)]
    output: Option<String>,
    /// Present when not `ok`
    #[serde(default)]
    error: Option<String>,
    /// The doc version a `format` reply speaks
    #[serde(default)]
    doc: Option<u32>,
    /// A `format` reply's document
    #[serde(default)]
    document: Option<proto::WireDoc>,
    /// A `lint` reply's findings
    #[serde(default)]
    findings: Option<Vec<proto::WireFinding>>,
    /// Comment spans, from `format` (the survival backstop) and `lint`
    /// (suppression)
    #[serde(default)]
    comments: Option<Vec<(u32, u32)>>,
}

pub struct NativeWorm {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// Named in every error, since the user configured it and we did not
    name: String,
}

impl NativeWorm {
    /// Spawn the worm and hold the pipes open
    pub fn load(entry: &Path, name: &str) -> Result<Self> {
        let mut child = spawn(entry).with_context(|| {
            format!(
                "cannot start worm `{name}`, {} is not runnable",
                crate::ui::rel(entry)
            )
        })?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            name: name.to_string(),
        })
    }

    /// Settings and rules, handed over once before any file
    pub fn init(&mut self, config: &str, rules: &str) -> Result<()> {
        self.call(&Request::Init {
            config,
            rules,
            doc_version: proto::DOC_VERSION,
        })?;

        Ok(())
    }

    /// A claimed file turned into Luau
    pub fn transform(&mut self, source: &str) -> Result<String> {
        let response = self.call(&Request::Transform { source })?;

        response
            .output
            .with_context(|| format!("worm `{}` returned no output", self.name))
    }

    /// A claimed file's layout, for the host to render
    pub fn format(&mut self, source: &str) -> Result<proto::FormatReply> {
        let response = self.call(&Request::Format { source })?;

        let doc = response
            .doc
            .with_context(|| format!("worm `{}` did not say which doc version it speaks", self.name))?;

        let document = response
            .document
            .with_context(|| format!("worm `{}` returned no document", self.name))?;

        Ok(proto::FormatReply {
            doc,
            document,
            comments: response.comments.unwrap_or_default(),
        })
    }

    /// A claimed file's problems, severity left to the host
    pub fn lint(&mut self, source: &str) -> Result<proto::LintReply> {
        let response = self.call(&Request::Lint { source })?;

        Ok(proto::LintReply {
            findings: response.findings.unwrap_or_default(),
            comments: response.comments.unwrap_or_default(),
        })
    }

    /*
    One round trip.

    A worm that dies mid call is reported against the file being processed
    rather than ending the run, which is the property the wasm form has and
    this one has to match: one pathological file must not end a watch session.
    The pool drops a failed instance, so the next file on this worker spawns a
    fresh child.
    */
    fn call(&mut self, request: &Request<'_>) -> Result<Response> {
        let body = serde_json::to_vec(request).expect("a request always serialises");

        self.write(&body)
            .with_context(|| format!("worm `{}` closed its input", self.name))?;

        let reply = self
            .read()
            .with_context(|| format!("worm `{}` closed its output", self.name))?;

        let response: Response = serde_json::from_slice(&reply)
            .with_context(|| format!("worm `{}` sent a reply we cannot read", self.name))?;

        if !response.ok {
            let why = response.error.unwrap_or_else(|| "no reason given".into());

            bail!("worm `{}` failed, {why}", self.name);
        }

        Ok(response)
    }

    fn write(&mut self, body: &[u8]) -> Result<()> {
        let len = u32::try_from(body.len()).context("a message longer than 4GB")?;

        self.stdin.write_all(&len.to_le_bytes())?;
        self.stdin.write_all(body)?;
        self.stdin.flush()?;

        Ok(())
    }

    fn read(&mut self) -> Result<Vec<u8>> {
        let mut len = [0u8; 4];
        self.stdout.read_exact(&mut len)?;

        let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
        self.stdout.read_exact(&mut body)?;

        Ok(body)
    }
}

/*
Start the process, retrying while the file is still held open for writing.

`ETXTBSY`, "text file busy", is what Linux returns when something exec's a file
another thread still has open for writing. It is reachable here without anybody
doing anything wrong: larvae unpacks a worm's binary and then spawns it, and if
a `fork` lands in the window where the unpacking write descriptor is open, the
child inherits it and its own `exec` fails.

Nothing in the process can see whose descriptor it is, so waiting is the only
answer. The window is a syscall wide, so a few short retries close it, and
anything still busy after that is a real problem worth reporting.
*/
fn spawn(entry: &Path) -> std::io::Result<Child> {
    // 26 is ETXTBSY, which has no named constant in std
    const BUSY: i32 = 26;

    for attempt in 0..5 {
        let result = Command::new(entry)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stderr is left alone so a worm can log without corrupting the channel
            .stderr(Stdio::inherit())
            .spawn();

        match result {
            Err(e) if e.raw_os_error() == Some(BUSY) && attempt < 4 => {
                std::thread::sleep(std::time::Duration::from_millis(2 << attempt));
            }

            other => return other,
        }
    }

    unreachable!("the loop returns on its last attempt")
}

/*
Kill the child when the instance goes.

Without this a worm outlives the build that started it. `kill` is right rather
than harsh: the protocol has no shutdown message because there is nothing for a
worm to flush, and waiting on a wedged child would hang the run.
*/
impl Drop for NativeWorm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worm written in shell, which is enough to exercise the framing
    fn echo_worm(dir: &Path, body: &str) -> std::path::PathBuf {
        let path = dir.join("worm.py");
        std::fs::write(
            &path,
            format!(
                r#"#!/usr/bin/env python3
import sys, json, struct

def read():
    n = sys.stdin.buffer.read(4)
    if len(n) < 4: sys.exit(0)
    return json.loads(sys.stdin.buffer.read(struct.unpack("<I", n)[0]))

def send(obj):
    b = json.dumps(obj).encode()
    sys.stdout.buffer.write(struct.pack("<I", len(b)) + b)
    sys.stdout.buffer.flush()

while True:
    req = read()
{body}
"#
            ),
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        path
    }

    fn worm_that(body: &str) -> (tempfile::TempDir, NativeWorm) {
        let dir = tempfile::tempdir().unwrap();
        let path = echo_worm(dir.path(), body);
        let worm = NativeWorm::load(&path, "test").expect("spawns");

        (dir, worm)
    }

    #[test]
    fn a_worm_answers_init_and_transform() {
        let (_dir, mut worm) = worm_that(
            r#"    if req["op"] == "init":
        send({"ok": True})
    else:
        send({"ok": True, "output": req["source"].upper()})"#,
        );

        worm.init("a = 1", "").expect("init");

        assert_eq!(worm.transform("hello").unwrap(), "HELLO");
    }

    /// Several files over one process, which is the point of keeping it alive
    #[test]
    fn one_process_answers_many_files() {
        let (_dir, mut worm) =
            worm_that(r#"    send({"ok": True, "output": req.get("source", "")[::-1]})"#);

        worm.init("", "").unwrap();

        for text in ["abc", "defg", "hi"] {
            let reversed: String = text.chars().rev().collect();

            assert_eq!(worm.transform(text).unwrap(), reversed);
        }
    }

    #[test]
    fn a_worm_reporting_failure_is_reported_by_name() {
        let (_dir, mut worm) =
            worm_that(r#"    send({"ok": False, "error": "line 3 is not markup"})"#);

        let err = worm.transform("x").expect_err("should fail");
        let text = format!("{err:#}");

        assert!(text.contains("test"), "{text}");
        assert!(text.contains("line 3 is not markup"), "{text}");
    }

    /// A worm that dies is an error against this file, not a panic
    #[test]
    fn a_worm_that_exits_is_an_error_rather_than_a_hang() {
        let (_dir, mut worm) = worm_that(r#"    sys.exit(1)"#);

        let err = worm.transform("x").expect_err("should fail");

        assert!(format!("{err:#}").contains("test"));
    }

    #[test]
    fn a_reply_that_is_not_json_is_reported_rather_than_parsed() {
        let (_dir, mut worm) = worm_that(
            r#"    b = b"not json"
    sys.stdout.buffer.write(struct.pack("<I", len(b)) + b)
    sys.stdout.buffer.flush()"#,
        );

        let err = worm.transform("x").expect_err("should fail");

        assert!(format!("{err:#}").contains("cannot read"), "{err:#}");
    }

    #[test]
    fn a_binary_that_does_not_exist_is_reported_at_load() {
        let Err(err) = NativeWorm::load(Path::new("/nonexistent/worm"), "ghost") else {
            panic!("there is no binary there to spawn")
        };

        assert!(format!("{err:#}").contains("ghost"));
    }

    /// Content with newlines and non-ASCII has to survive the framing intact
    #[test]
    fn the_framing_is_byte_exact() {
        let (_dir, mut worm) =
            worm_that(r#"    send({"ok": True, "output": req.get("source", "")})"#);

        worm.init("", "").unwrap();

        let awkward = "line one\nline two\r\n\ttabbed \u{1F600} héllo \"quoted\" \\slash";

        assert_eq!(worm.transform(awkward).unwrap(), awkward);
    }

    #[test]
    fn a_worm_answers_format_with_a_document() {
        let (_dir, mut worm) = worm_that(
            r#"    if req["op"] == "init":
        send({"ok": True})
    else:
        send({"ok": True, "doc": 1, "document": {"src": [0, 2]}, "comments": [[0, 1]]})"#,
        );

        worm.init("", "").unwrap();

        let reply = worm.format("ab").unwrap();

        assert_eq!(reply.doc, 1);
        assert_eq!(reply.document, proto::WireDoc::Src(0, 2));
        assert_eq!(reply.comments, vec![(0, 1)]);
    }

    #[test]
    fn a_format_reply_missing_its_doc_version_is_refused() {
        let (_dir, mut worm) = worm_that(r#"    send({"ok": True, "document": "nil"})"#);

        let err = worm.format("x").expect_err("no doc field");

        assert!(format!("{err:#}").contains("doc version"), "{err:#}");
    }

    #[test]
    fn a_worm_answers_lint_with_findings() {
        let (_dir, mut worm) = worm_that(
            r#"    send({"ok": True, "findings": [{"span": [0, 1], "lint": "tidy", "message": "untidy"}]})"#,
        );

        let reply = worm.lint("x").unwrap();

        assert_eq!(reply.findings.len(), 1);
        assert_eq!(reply.findings[0].lint, "tidy");
        assert_eq!(reply.findings[0].help, None);
        assert!(reply.comments.is_empty());
    }

    /// The worm learns our layout contract at init, so it can refuse early
    #[test]
    fn init_carries_the_doc_version() {
        let (_dir, mut worm) = worm_that(
            r#"    send({"ok": False, "error": "saw doc v%d" % req.get("doc_version", -1)})"#,
        );

        let err = worm.init("", "").expect_err("the worm refuses everything");

        assert!(format!("{err:#}").contains("saw doc v1"), "{err:#}");
    }
}
