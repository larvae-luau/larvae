/*!
A worm that is an ordinary executable. Larvae speaks to it over a pipe.

This is the third form, beside the embedded Luau VM and the wasm interpreter.
An author selects it when a worm does enough work that the interpreter
overhead matters. The form gives native speed. It costs three things: an
artifact per platform, no sandbox, and a process the host must keep alive.

**No sandbox.** A wasm worm cannot read the SSH keys of the user. This form
runs with the full access of the user. For this reason `wasm` stays the
default, and the user must opt in per worm, for code that a project trusts.

## The protocol

Each message is a 4 byte little endian length, then that many bytes of JSON.
This applies in both directions. The protocol does not use the text headers of
the LSP. This is a private channel between two programs that ship together.
Thus there is nothing to negotiate, and a fixed prefix has fewer error modes.

Requests are one per file, never one per node. This limit is the reason the
cost is acceptable, and the wasm side moves to the same shape. The measured
cost of the old per node protocol was 24 µs per crossing, and a rule worm paid
it 120 times per file.

## Concurrency

Each instance owns one process, and instances are already per rayon worker
because `mlua::Lua` is `!Send`. Thus a worker owns its child fully, and no
request overlaps an other request on the same pipe. For this reason there are
no request ids here. The design question in the plan, one process per worker
or ids over one pipe, is answered by the fact that the pool is thread local.
*/

use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::proto;

/// The operations the host asks a worm to do
#[derive(Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request<'a> {
    /// The settings and enabled rules, sent once before the first file.
    /// `doc_version` is the layout contract the host speaks. A worm that
    /// does not format can ignore it.
    Init {
        config: &'a str,
        rules: &'a str,
        doc_version: u32,
        /// The resolved `[fmt]` table of the project, as JSON. A worm reads
        /// it to lay its own constructs out in the style of the project.
        fmt: &'a str,
        /// The resolved lint levels of the project, as JSON
        lint: &'a str,
    },
    /// Turn a claimed file into Luau
    Transform {
        source: &'a str,
    },
    /// Format a claimed file and reply with a document to render
    Format {
        source: &'a str,
    },
    /// Report the problems of a claimed file. The host decides the severity.
    Lint {
        source: &'a str,
    },
    /*
    Run the enabled rules over the matched nodes of one file.

    This is the batched shape from the module docs: one crossing per file
    and never one per node. A crossing costs 24 µs, and a rule worm visits
    about 120 nodes per file, so the batch is what keeps this form fast.
    */
    Rules {
        source: &'a str,
        rules: &'a [proto::RuleCall],
    },
    /*
    Ask the worm for its own `worm.toml`.

    A worm that arrives through cargo is a bare binary, because `cargo
    install` ships no data files. The manifest therefore travels inside the
    binary, and larvae asks for it once at install time.
    */
    Manifest,
}

/// The reply. One struct serves every op, because the fields do not overlap.
#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    ok: bool,
    /// Present when `ok` is true, for `transform`
    #[serde(default)]
    output: Option<String>,
    /// Present when `ok` is false
    #[serde(default)]
    error: Option<String>,
    /// The doc version that a `format` reply speaks
    #[serde(default)]
    doc: Option<u32>,
    /// The document of a `format` reply
    #[serde(default)]
    document: Option<proto::WireDoc>,
    /// The findings of a `lint` reply
    #[serde(default)]
    findings: Option<Vec<proto::WireFinding>>,
    /// The comment spans, from `format` (the survival backstop) and `lint`
    /// (suppression)
    #[serde(default)]
    comments: Option<Vec<(u32, u32)>>,
    /// The Luau shadow of a `lint` reply, for the inherited lints
    #[serde(default)]
    luau: Option<String>,
    /// The Luau regions of a `format` reply, for a worm that lays out nothing
    #[serde(default)]
    spans: Option<Vec<(u32, u32)>>,
    /// The `worm.toml` text of a `manifest` reply
    #[serde(default)]
    manifest: Option<String>,
    /// The edits of a `rules` reply
    #[serde(default)]
    edits: Option<Vec<(u32, u32, String)>>,
}

/// Ask a worm binary for the `worm.toml` it carries, in one short lived process
pub fn manifest_of(entry: &Path, name: &str) -> Result<String> {
    NativeWorm::load(entry, name)?.manifest()
}

pub struct NativeWorm {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// Every error names the worm, because the user configured it and larvae did not
    name: String,
}

impl NativeWorm {
    /// Start the worm process and hold the pipes open
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

    /// Send the settings and rules once, before the first file
    pub fn init(&mut self, config: &str, rules: &str, settings: &super::Settings) -> Result<()> {
        self.call(&Request::Init {
            config,
            rules,
            doc_version: proto::DOC_VERSION,
            fmt: &settings.fmt,
            lint: &settings.lint,
        })?;

        Ok(())
    }

    /// Ask the worm for the `worm.toml` text that it carries
    pub fn manifest(&mut self) -> Result<String> {
        let response = self.call(&Request::Manifest)?;

        response.manifest.with_context(|| {
            format!(
                "worm `{}` did not return a manifest; a worm installs through cargo only when it answers the manifest op",
                self.name
            )
        })
    }

    /// Turn a claimed file into Luau
    pub fn transform(&mut self, source: &str) -> Result<String> {
        let response = self.call(&Request::Transform { source })?;

        response
            .output
            .with_context(|| format!("worm `{}` returned no output", self.name))
    }

    /// Get the layout of a claimed file, for the host to render
    pub fn format(&mut self, source: &str) -> Result<proto::FormatReply> {
        let response = self.call(&Request::Format { source })?;

        let doc = response.doc.with_context(|| {
            format!(
                "worm `{}` did not say which doc version it speaks",
                self.name
            )
        })?;

        Ok(proto::FormatReply {
            doc,
            document: response.document,
            spans: response.spans.unwrap_or_default(),
            comments: response.comments.unwrap_or_default(),
        })
    }

    /*
    Run the enabled rules of one file in one crossing, and get the edits.

    Every edit span must lie on the source, with the same rules as a format
    span. The check runs here, before an edit reaches the splice, because a
    span off the wire is untrusted in the same way as a `src` span of a
    format reply.
    */
    pub fn rules(
        &mut self,
        source: &str,
        rules: &[proto::RuleCall],
    ) -> Result<Vec<crate::rules::edits::Edit>> {
        let response = self.call(&Request::Rules { source, rules })?;
        let edits = response.edits.unwrap_or_default();

        for (start, end, _) in &edits {
            if !span_ok(source, *start, *end) {
                bail!(
                    "worm `{}` returned the edit span {start}..{end}, which does not lie on the source",
                    self.name
                );
            }
        }

        Ok(edits)
    }

    /// Get the problems of a claimed file. The host decides the severity.
    pub fn lint(&mut self, source: &str) -> Result<proto::LintReply> {
        let response = self.call(&Request::Lint { source })?;

        Ok(proto::LintReply {
            findings: response.findings.unwrap_or_default(),
            comments: response.comments.unwrap_or_default(),
            luau: response.luau,
        })
    }

    /*
    Make one round trip.

    When a worm stops in the middle of a call, larvae reports an error against
    the current file and does not stop the run. The wasm form has this
    property, and this form must match it: one bad file must not stop a watch
    session. The pool drops a failed instance, so the next file on this worker
    starts a new child.
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

/// Report if a span lies on the source. The rules match the format path:
/// ordered ends, inside the source, and on character boundaries.
fn span_ok(src: &str, start: u32, end: u32) -> bool {
    let (s, e) = (start as usize, end as usize);

    s <= e && e <= src.len() && src.is_char_boundary(s) && src.is_char_boundary(e)
}

/*
Start the process. Retry while the file is still open for writing.

Linux returns `ETXTBSY`, "text file busy", when a process executes a file that
an other thread still has open for writing. This case occurs here without an
error by anyone. Larvae unpacks the binary of a worm and then starts it. If a
`fork` occurs in the window where the write descriptor from the unpack is
open, the child inherits it, and the `exec` of the child fails.

No code in the process can see which thread owns the descriptor. Thus a wait
is the only answer. The window is one syscall wide, so a few short retries
close it. A file that is still busy after the retries is a real problem, and
larvae reports it.
*/
fn spawn(entry: &Path) -> std::io::Result<Child> {
    // 26 is ETXTBSY. std has no named constant for it.
    const BUSY: i32 = 26;

    for attempt in 0..5 {
        let result = Command::new(entry)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stderr stays untouched, so a worm can log without damage to the channel
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
Kill the child when the instance drops.

Without this, a worm lives longer than the build that started it. `kill` is
the correct choice. The protocol has no shutdown message, because a worm has
nothing to flush. A wait on a stuck child would hang the run.
*/
impl Drop for NativeWorm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The request JSON is the contract, so a shape change here is a protocol
/// change. These pins run on every platform.
#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn the_rules_request_shape_is_the_documented_one() {
        let rules = vec![proto::RuleCall {
            name: "x".into(),
            nodes: vec![proto::WireNode {
                id: 1,
                kind: "CallExpr".into(),
                span: (4, 20),
            }],
        }];

        let request = Request::Rules {
            source: "s",
            rules: &rules,
        };

        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"op":"rules","source":"s","rules":[{"name":"x","nodes":[{"id":1,"kind":"CallExpr","span":[4,20]}]}]}"#
        );
    }
}

/// These tests spawn a python fixture, so they are unix only. Windows CI runs
/// `cargo test`, and a `.py` script does not spawn as a process there.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A worm written as a script, which is enough to test the framing
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

        worm.init("a = 1", "", &Default::default()).expect("init");

        assert_eq!(worm.transform("hello").unwrap(), "HELLO");
    }

    /// Several files cross one process. This is the reason the host keeps it alive.
    #[test]
    fn one_process_answers_many_files() {
        let (_dir, mut worm) =
            worm_that(r#"    send({"ok": True, "output": req.get("source", "")[::-1]})"#);

        worm.init("", "", &Default::default()).unwrap();

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

    /// A worm that stops is an error against this file, not a panic
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

    /// Content with newlines and non-ASCII must cross the framing intact
    #[test]
    fn the_framing_is_byte_exact() {
        let (_dir, mut worm) =
            worm_that(r#"    send({"ok": True, "output": req.get("source", "")})"#);

        worm.init("", "", &Default::default()).unwrap();

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

        worm.init("", "", &Default::default()).unwrap();

        let reply = worm.format("ab").unwrap();

        assert_eq!(reply.doc, 1);
        assert_eq!(reply.document, Some(proto::WireDoc::Src(0, 2)));
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

    /// The worm learns the layout contract at init, so it can refuse early
    #[test]
    fn init_carries_the_doc_version() {
        let (_dir, mut worm) = worm_that(
            r#"    send({"ok": False, "error": "saw doc v%d" % req.get("doc_version", -1)})"#,
        );

        let err = worm
            .init("", "", &Default::default())
            .expect_err("the worm refuses everything");

        assert!(format!("{err:#}").contains("saw doc v1"), "{err:#}");
    }

    /// One crossing carries every rule and every node, and one reply carries
    /// every edit
    #[test]
    fn a_worm_answers_rules_with_edits() {
        let (_dir, mut worm) = worm_that(
            r#"    if req["op"] == "init":
        send({"ok": True})
    else:
        edits = []
        for rule in req["rules"]:
            for node in rule["nodes"]:
                s, e = node["span"]
                edits.append([s, e, req["source"][s:e].upper()])
        send({"ok": True, "edits": edits})"#,
        );

        worm.init("", "", &Default::default()).unwrap();

        let rules = vec![
            proto::RuleCall {
                name: "shout".into(),
                nodes: vec![proto::WireNode {
                    id: 1,
                    kind: "Name".into(),
                    span: (0, 5),
                }],
            },
            proto::RuleCall {
                name: "shout_more".into(),
                nodes: vec![proto::WireNode {
                    id: 2,
                    kind: "Name".into(),
                    span: (6, 11),
                }],
            },
        ];

        assert_eq!(
            worm.rules("hello world", &rules).unwrap(),
            vec![(0, 5, "HELLO".to_owned()), (6, 11, "WORLD".to_owned())]
        );
    }

    /// A reply without edits means no change
    #[test]
    fn a_rules_reply_without_edits_is_no_change() {
        let (_dir, mut worm) = worm_that(r#"    send({"ok": True})"#);

        assert!(worm.rules("x", &[]).unwrap().is_empty());
    }

    /// A span off the source is an error against this file, with the worm named
    #[test]
    fn an_edit_span_off_the_source_is_refused_by_name() {
        let (_dir, mut worm) = worm_that(r#"    send({"ok": True, "edits": [[0, 99, "y"]]})"#);

        let err = worm.rules("short", &[]).expect_err("the span is off");
        let text = format!("{err:#}");

        assert!(text.contains("test"), "{text}");
        assert!(text.contains("0..99"), "{text}");
    }

    /// A span that splits a character is refused, the same as a format span
    #[test]
    fn an_edit_span_splitting_a_character_is_refused() {
        let (_dir, mut worm) = worm_that(r#"    send({"ok": True, "edits": [[1, 2, "y"]]})"#);

        // é is two bytes, so 1..2 points inside it
        assert!(worm.rules("é", &[]).is_err());
    }
}
