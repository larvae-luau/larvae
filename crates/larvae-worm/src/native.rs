/*!
The native transport: all the parts that a worm shipped as an executable needs.

A native worm is an ordinary program that larvae starts and keeps alive. Each
message is a 4 byte little endian length, then that many bytes of JSON, in
both directions, over stdin and stdout. This module owns that protocol, in the
same way as [`frontend!`](crate::frontend) owns the wasm one. Implement
[`Handler`] for the state of your worm and give it to [`serve`]. The function
loops until larvae closes the pipe.

```no_run
use larvae_worm::native::{serve, Doc, Format, Handler};

struct MyWorm;

impl Handler for MyWorm {
    fn transform(&mut self, source: &str) -> Result<String, String> {
        Ok(source.replace("<>", "{}"))
    }

    fn format(&mut self, source: &str) -> Result<Format, String> {
        Ok(Format::document(Doc::concat([
            Doc::lit("-- formatted"),
            Doc::Hard,
            Doc::host(0, source.len() as u32),
        ])))
    }
}

fn main() {
    serve(MyWorm)
}
```

# The requests that larvae sends

```jsonc
{"op": "init", "config": "pretty = true\n", "rules": "", "doc_version": 1}
{"op": "transform", "source": "..."}   // reply {"ok": true, "output": "..."}
{"op": "format", "source": "..."}      // reply below
{"op": "lint", "source": "..."}        // reply below
{"op": "rules", "source": "...",       // reply {"ok": true, "edits": [[4, 20, "new"]]}
 "rules": [{"name": "x", "nodes": [{"id": 1, "kind": "CallExpr", "span": [4, 20]}]}]}
```

A format reply carries a layout document. larvae renders it with the width
and indentation of the project, so no worm reimplements the printer:

```jsonc
{ "ok": true, "doc": 1,
  "document": { "concat": [ {"src": [0, 12]}, "hard", {"host": {"start": 13, "end": 40}} ] },
  "comments": [[0, 10]] }
```

A lint reply carries findings without a severity, because the host owns the
levels, the suppression, and the exit codes:

```jsonc
{ "ok": true,
  "findings": [ {"span": [2, 9], "lint": "my_lint", "message": "..."} ],
  "comments": [[0, 10]] }
```

A rules request carries every enabled rule of one file, each with its matched
nodes, in one message. One message per file is the contract, because a pipe
crossing costs about 24 µs and a rule worm visits about 120 nodes per file.
The reply carries whole span replacements against the original source.

An error replies `{"ok": false, "error": "why"}`, and the worm continues to
serve. One bad file must not stop a watch session.
*/

use std::io::{Read, Write};

pub use crate::wire::{DOC_VERSION, Doc, Finding, Format, HostParse, Lint};
use serde::{Deserialize, Serialize};

/**
The operations of a worm. Each default is a refusal.

Implement the operations that your worm.toml declares: `transform` for a
`[frontend]`, `format` when it sets `fmt = true`, `lint` when it declares
`[lints]`, and `rules` when it declares `[rules]`. larvae does not call an op
that it does not send. Thus the defaults answer only when a manifest and its
worm disagree.
*/
pub trait Handler {
    /**
    The settings and enabled rules, sent once before the first file.

    `settings` carries the resolved `[fmt]` table and the lint levels of the
    project. Read them to lay your own constructs out in the style that the
    project asked for. Then the user states a setting one time, and not a
    second time under `[worms.<name>.config]`.
    */
    fn init(&mut self, config: &str, rules: &str, settings: &Settings) -> Result<(), String> {
        let _ = (config, rules, settings);

        Ok(())
    }

    /// Turn a claimed file into Luau
    fn transform(&mut self, source: &str) -> Result<String, String> {
        let _ = source;

        Err("this worm does not transform".into())
    }

    /// Format a claimed file for larvae to render
    fn format(&mut self, source: &str) -> Result<Format, String> {
        let _ = source;

        Err("this worm does not format".into())
    }

    /// Report the problems of a claimed file
    fn lint(&mut self, source: &str) -> Result<Lint, String> {
        let _ = source;

        Err("this worm does not lint".into())
    }

    /**
    The `worm.toml` text that this worm carries, for the cargo channel.

    `cargo install` ships one binary and no data files. A worm that returns
    its manifest here installs with `[worms.x] cargo = "crate@version"`, and
    larvae writes the returned text beside the binary. Embed the file:

    ```ignore
    fn manifest(&self) -> Option<&'static str> {
        Some(include_str!("../worm.toml"))
    }
    ```

    A worm that returns `None` installs only from a release or a path.
    */
    fn manifest(&self) -> Option<&'static str> {
        None
    }

    /**
    Run the enabled rules over the matched nodes of one file.

    larvae sends one request per file and never one per node, because a pipe
    crossing costs about 24 µs and a rule worm visits about 120 nodes per
    file. A rule receives the matched nodes of its `filter`, each as an id, a
    kind name, and a byte span, and the whole source beside them. It
    navigates no tree. It returns whole span replacements against the
    original source, as `(start, end, new_text)`. A span that does not lie on
    the source fails the file on the host.
    */
    fn rules(&mut self, source: &str, rules: &[RuleCall]) -> Result<Vec<Edit>, String> {
        let _ = (source, rules);

        Err("this worm does not run rules".into())
    }
}

/// A whole span replacement: the start byte, the end byte, and the new text
pub type Edit = (u32, u32, String);

/// One rule of the worm, with the nodes that its `filter` matched
#[derive(Debug, Clone, Deserialize)]
pub struct RuleCall {
    /// The rule name, as `[rules]` in `worm.toml` declares it
    pub name: String,
    /// The matched nodes, in pre-order
    pub nodes: Vec<WireNode>,
}

/// One matched node of a [`RuleCall`]
#[derive(Debug, Clone, Deserialize)]
pub struct WireNode {
    /// The pre-order index of the node in its file
    pub id: u32,
    /// The kind name, the same text as `filter` in `worm.toml`
    pub kind: String,
    /// The byte range in the source, as a half open range
    pub span: (u32, u32),
}

/// The resolved settings of the project, as larvae sent them
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// The `[fmt]` table of the project, as JSON text. It is empty when the
    /// project states nothing.
    pub fmt: String,
    /// The lint levels of the project, as JSON text
    pub lint: String,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Init {
        #[serde(default)]
        config: String,
        #[serde(default)]
        rules: String,
        /// A host older than the format op does not send this field
        #[serde(default)]
        doc_version: u32,
        /// The `[fmt]` table of the project, as JSON text
        #[serde(default)]
        fmt: String,
        /// The lint levels of the project, as JSON text
        #[serde(default)]
        lint: String,
    },
    Transform {
        source: String,
    },
    Format {
        source: String,
    },
    Lint {
        source: String,
    },
    Rules {
        source: String,
        #[serde(default)]
        rules: Vec<RuleCall>,
    },
    Manifest,
}

/**
Serve larvae until it closes the pipe.

A handler error becomes an `{"ok": false, "error": ...}` reply and does not
stop the process. This matches the treatment on the larvae side: an error
counts against one file and does not stop a run. The function returns only
when stdin reaches end of file, which means larvae dropped the worm. Thus a
return of `()` from `main` directly after is the clean shutdown.
*/
pub fn serve(mut handler: impl Handler) {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();

    loop {
        let Some(body) = read_frame(&mut input) else {
            return;
        };

        let reply = match serde_json::from_slice::<Request>(&body) {
            Ok(request) => answer(&mut handler, request),

            Err(e) => error_reply(format!("cannot read the request, {e}")),
        };

        write_frame(&mut output, &reply);
    }
}

fn answer(handler: &mut impl Handler, request: Request) -> Vec<u8> {
    let reply = match request {
        Request::Init {
            config,
            rules,
            doc_version,
            fmt,
            lint,
        } => {
            // 0 means a host from before the format op existed. Such a host
            // does not send one, so only a real mismatch is a reason to refuse.
            if doc_version != 0 && doc_version != DOC_VERSION {
                return error_reply(format!(
                    "this worm speaks doc v{DOC_VERSION}, larvae speaks v{doc_version}"
                ));
            }

            handler
                .init(&config, &rules, &Settings { fmt, lint })
                .map(|()| serde_json::json!({ "ok": true }))
        }

        Request::Transform { source } => handler
            .transform(&source)
            .map(|output| serde_json::json!({ "ok": true, "output": output })),

        Request::Format { source } => handler.format(&source).map(|format| {
            serde_json::json!({
                "ok": true,
                "doc": DOC_VERSION,
                "document": format.document,
                "spans": format.spans,
                "comments": format.comments,
            })
        }),

        Request::Lint { source } => handler.lint(&source).map(|lint| {
            serde_json::json!({
                "ok": true,
                "findings": lint.findings,
                "comments": lint.comments,
                "luau": lint.luau,
            })
        }),

        Request::Rules { source, rules } => handler
            .rules(&source, &rules)
            .map(|edits| serde_json::json!({ "ok": true, "edits": edits })),

        Request::Manifest => match handler.manifest() {
            Some(text) => Ok(serde_json::json!({ "ok": true, "manifest": text })),

            None => Err("this worm does not carry its manifest".into()),
        },
    };

    match reply {
        Ok(value) => serde_json::to_vec(&value).expect("a reply always serialises"),

        Err(why) => error_reply(why),
    }
}

fn error_reply(why: String) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "ok": false, "error": why }))
        .expect("a reply always serialises")
}

/// Read one length prefixed frame, or `None` at end of file
fn read_frame(input: &mut impl Read) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    input.read_exact(&mut len).ok()?;

    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    input.read_exact(&mut body).ok()?;

    Some(body)
}

fn write_frame(output: &mut impl Write, body: &[u8]) {
    let len = u32::try_from(body.len()).expect("a reply under 4GB");

    // a failed write means larvae is gone, and there is no receiver left to tell
    let _ = output.write_all(&len.to_le_bytes());
    let _ = output.write_all(body);
    let _ = output.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact JSON that the shape tests of larvae pin, from the guest side
    #[test]
    fn the_wire_doc_shape_matches_the_host() {
        let doc = Doc::concat([
            Doc::Nil,
            Doc::src(0, 4),
            Doc::lit("<"),
            Doc::Line,
            Doc::if_break(Doc::Nil, Doc::Hard),
            Doc::group(Doc::indent(Doc::host_expr(8, 12))),
        ]);

        assert_eq!(
            serde_json::to_string(&doc).unwrap(),
            r#"{"concat":["nil",{"src":[0,4]},{"lit":"<"},"line",{"if_break":["nil","hard"]},{"group":{"indent":{"host":{"start":8,"end":12,"parse":"expr"}}}}]}"#
        );
    }

    #[test]
    fn a_finding_serialises_without_a_null_help() {
        let finding = Finding::new("tidy", (2, 7), "untidy");

        assert_eq!(
            serde_json::to_string(&finding).unwrap(),
            r#"{"span":[2,7],"lint":"tidy","message":"untidy"}"#
        );

        let helped = finding.with_help("do less");

        assert!(serde_json::to_string(&helped).unwrap().contains("do less"));
    }

    struct Echo;

    impl Handler for Echo {
        fn transform(&mut self, source: &str) -> Result<String, String> {
            Ok(source.to_uppercase())
        }
    }

    fn frame(json: &str) -> Vec<u8> {
        let mut out = (json.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(json.as_bytes());

        out
    }

    #[test]
    fn a_transform_round_trips_through_answer() {
        let body = frame(r#"{"op":"transform","source":"hi"}"#);
        let request: Request = serde_json::from_slice(&body[4..]).unwrap();
        let reply = answer(&mut Echo, request);

        assert_eq!(
            String::from_utf8(reply).unwrap(),
            r#"{"ok":true,"output":"HI"}"#
        );
    }

    #[test]
    fn an_undeclared_op_refuses_rather_than_panics() {
        let request: Request = serde_json::from_slice(br#"{"op":"format","source":"x"}"#).unwrap();

        let reply = String::from_utf8(answer(&mut Echo, request)).unwrap();

        assert!(reply.contains(r#""ok":false"#), "{reply}");
        assert!(reply.contains("does not format"), "{reply}");
    }

    #[test]
    fn a_doc_version_mismatch_is_refused_at_init() {
        let request: Request =
            serde_json::from_slice(br#"{"op":"init","config":"","rules":"","doc_version":9}"#)
                .unwrap();

        let reply = String::from_utf8(answer(&mut Echo, request)).unwrap();

        assert!(reply.contains("doc v1"), "{reply}");
    }

    /// A worm that uppercases every matched span, which is enough to prove
    /// that the batch crosses and the edits cross back
    struct Shout;

    impl Handler for Shout {
        fn rules(&mut self, source: &str, rules: &[RuleCall]) -> Result<Vec<Edit>, String> {
            Ok(rules
                .iter()
                .flat_map(|call| &call.nodes)
                .map(|node| {
                    let (start, end) = node.span;
                    let text = source[start as usize..end as usize].to_uppercase();

                    (start, end, text)
                })
                .collect())
        }
    }

    #[test]
    fn a_rules_request_round_trips_through_answer() {
        let request: Request = serde_json::from_slice(
            br#"{"op":"rules","source":"hi there","rules":[{"name":"up","nodes":[{"id":1,"kind":"Name","span":[0,2]},{"id":2,"kind":"Name","span":[3,8]}]}]}"#,
        )
        .unwrap();

        let reply = String::from_utf8(answer(&mut Shout, request)).unwrap();

        // json! sorts its keys, so `edits` precedes `ok` on the wire
        assert_eq!(reply, r#"{"edits":[[0,2,"HI"],[3,8,"THERE"]],"ok":true}"#);
    }

    #[test]
    fn an_undeclared_rules_op_refuses_rather_than_panics() {
        let request: Request =
            serde_json::from_slice(br#"{"op":"rules","source":"x","rules":[]}"#).unwrap();

        let reply = String::from_utf8(answer(&mut Echo, request)).unwrap();

        assert!(reply.contains(r#""ok":false"#), "{reply}");
        assert!(reply.contains("does not run rules"), "{reply}");
    }
}
