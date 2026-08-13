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
        Ok(Format {
            document: Doc::concat([
                Doc::lit("-- formatted"),
                Doc::Hard,
                Doc::host(0, source.len() as u32),
            ]),
            comments: Vec::new(),
        })
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

An error replies `{"ok": false, "error": "why"}`, and the worm continues to
serve. One bad file must not stop a watch session.
*/

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

/// The layout contract revision this module speaks. It is `doc` in a format reply.
pub const DOC_VERSION: u32 = 1;

/**
One piece of layout, in exactly the shape that larvae deserializes.

Source text crosses as a `Src` span and not as a copy. `Lit` is reserved for
text that the worm generated. `Host` marks a span of ordinary Luau that
larvae formats itself and splices in. This lets a worm own its markup and no
Luau at all.
*/
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Doc {
    /// No output at all
    Nil,
    /// An exact slice of the source, by byte range
    Src(u32, u32),
    /// Text that the worm generated
    Lit(String),
    /// A space when flat, a newline when broken
    Line,
    /// Nothing when flat, a newline when broken
    Soft,
    /// A newline in both modes. It forces every enclosing group to break.
    Hard,
    /// A blank line that the author wrote. It is kept because it separates ideas.
    Blank,
    /// One value when the enclosing group is flat, an other value when it breaks
    IfBreak(Box<Doc>, Box<Doc>),
    /// Flat when it fits the line, broken when it does not fit
    Group(Box<Doc>),
    /// One more level of indentation for the content inside
    Indent(Box<Doc>),
    /// The parts, in order
    Concat(Vec<Doc>),
    /// A span of ordinary Luau for larvae to format and splice in
    Host {
        /// The byte offset where the span starts
        start: u32,
        /// The byte offset one past its end
        end: u32,
        /// The mode in which larvae parses it
        parse: HostParse,
    },
}

/// The parse mode of a [`Doc::Host`] span
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostParse {
    /// Statements, which is the shape between markup regions
    Block,
    /// One expression, a `{expr}` hole or attribute value
    Expr,
}

impl Doc {
    /// An exact slice of the source
    pub fn src(start: u32, end: u32) -> Self {
        Self::Src(start, end)
    }

    /// Text that the worm generated
    pub fn lit(s: impl Into<String>) -> Self {
        Self::Lit(s.into())
    }

    /// Flat when it fits, broken when it does not fit
    pub fn group(inner: Doc) -> Self {
        Self::Group(Box::new(inner))
    }

    /// One more level of indentation for the content inside
    pub fn indent(inner: Doc) -> Self {
        Self::Indent(Box::new(inner))
    }

    /// `broken` only when the enclosing group breaks, and `flat` in the other case
    pub fn if_break(flat: Doc, broken: Doc) -> Self {
        Self::IfBreak(Box::new(flat), Box::new(broken))
    }

    /// The parts, in order
    pub fn concat(parts: impl IntoIterator<Item = Doc>) -> Self {
        Self::Concat(parts.into_iter().collect())
    }

    /// `parts` separated by `sep`, which is the shape that most lists take
    pub fn join(sep: Doc, parts: impl IntoIterator<Item = Doc>) -> Self {
        let mut out = Vec::new();

        for (i, part) in parts.into_iter().enumerate() {
            if i > 0 {
                out.push(sep.clone());
            }

            out.push(part);
        }

        Self::Concat(out)
    }

    /// A span of Luau statements for larvae to format
    pub fn host(start: u32, end: u32) -> Self {
        Self::Host {
            start,
            end,
            parse: HostParse::Block,
        }
    }

    /// A span that holds one Luau expression for larvae to format
    pub fn host_expr(start: u32, end: u32) -> Self {
        Self::Host {
            start,
            end,
            parse: HostParse::Expr,
        }
    }
}

/// One problem found. The severity is absent by intent, because the host owns it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    /// The byte range in the source
    pub span: (u32, u32),
    /// The name of the lint. You must declare it in `[lints]` in your `worm.toml`.
    pub lint: String,
    /// The description of the problem
    pub message: String,
    /// The fix, when there is a short fix to state
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

impl Finding {
    /// A new finding. Add help with [`with_help`](Self::with_help).
    pub fn new(lint: impl Into<String>, span: (u32, u32), message: impl Into<String>) -> Self {
        Self {
            span,
            lint: lint.into(),
            message: message.into(),
            help: None,
        }
    }

    /// The same finding with a help line
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());

        self
    }
}

/// The value that [`Handler::format`] returns
#[derive(Debug, Clone, PartialEq)]
pub struct Format {
    /// The layout for the whole file
    pub document: Doc,
    /// The span of every comment, so larvae can refuse a layout that lost one
    pub comments: Vec<(u32, u32)>,
}

/// The value that [`Handler::lint`] returns
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Lint {
    /// The problems found
    pub findings: Vec<Finding>,
    /**
    The Luau shadow of the file, for the lints of larvae to read.

    The shadow is the source with every region that is not Luau replaced by
    filler of the same byte length. Thus each offset in the shadow is the same
    offset in the source, and larvae maps no spans. The shadow must parse as
    Luau.

    Set this field when `worm.toml` says `inherit_lints = true` and you want
    exact columns. Leave it empty to let larvae read the output of your own
    `transform` instead, which maps by line.
    */
    pub luau: Option<String>,
    /// The comment spans, so `-- larvae: allow(...)` works in a claimed file.
    /// Leave the list empty to remove your findings from suppression.
    pub comments: Vec<(u32, u32)>,
}

/**
The operations of a worm. Each default is a refusal.

Implement the operations that your worm.toml declares: `transform` for a
`[frontend]`, `format` when it sets `fmt = true`, and `lint` when it declares
`[lints]`. larvae does not call an op that it does not send. Thus the defaults
answer only when a manifest and its worm disagree.
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
        let request: Request =
            serde_json::from_slice(br#"{"op":"format","source":"x"}"#).unwrap();

        let reply = String::from_utf8(answer(&mut Echo, request)).unwrap();

        assert!(reply.contains(r#""ok":false"#), "{reply}");
        assert!(reply.contains("does not format"), "{reply}");
    }

    #[test]
    fn a_doc_version_mismatch_is_refused_at_init() {
        let request: Request = serde_json::from_slice(
            br#"{"op":"init","config":"","rules":"","doc_version":9}"#,
        )
        .unwrap();

        let reply = String::from_utf8(answer(&mut Echo, request)).unwrap();

        assert!(reply.contains("doc v1"), "{reply}");
    }
}
