/*!
The native transport: everything a worm shipped as an executable needs.

A native worm is an ordinary program larvae spawns and keeps alive, speaking
a 4 byte little endian length then that many bytes of JSON, in both
directions, over stdin and stdout. This module owns that dance the way
[`frontend!`](crate::frontend) owns the wasm one: implement [`Handler`] for
your worm's state and hand it to [`serve`], which loops until larvae closes
the pipe.

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

# The requests larvae sends

```jsonc
{"op": "init", "config": "pretty = true\n", "rules": "", "doc_version": 1}
{"op": "transform", "source": "..."}   // reply {"ok": true, "output": "..."}
{"op": "format", "source": "..."}      // reply below
{"op": "lint", "source": "..."}        // reply below
```

A format reply carries a layout document larvae renders with the project's
own width and indentation, so no worm reimplements the printer:

```jsonc
{ "ok": true, "doc": 1,
  "document": { "concat": [ {"src": [0, 12]}, "hard", {"host": {"start": 13, "end": 40}} ] },
  "comments": [[0, 10]] }
```

A lint reply carries findings without a severity, because the host owns
levels, suppression and exit codes:

```jsonc
{ "ok": true,
  "findings": [ {"span": [2, 9], "lint": "my_lint", "message": "..."} ],
  "comments": [[0, 10]] }
```

Errors reply `{"ok": false, "error": "why"}` and the worm keeps serving; one
bad file must not end a watch session.
*/

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

/// The layout contract revision this module speaks, `doc` in a format reply
pub const DOC_VERSION: u32 = 1;

/**
One piece of layout, exactly the shape larvae deserializes.

Source text crosses as a `Src` span rather than a copy, and `Lit` is reserved
for text the worm generated. `Host` marks a span of ordinary Luau larvae
formats itself and splices in, which is what lets a worm own its markup and
no Luau at all.
*/
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Doc {
    /// Nothing at all
    Nil,
    /// A verbatim slice of the source, by byte range
    Src(u32, u32),
    /// Text the worm generated
    Lit(String),
    /// A space when flat, a newline when broken
    Line,
    /// Nothing when flat, a newline when broken
    Soft,
    /// A newline either way, which forces every enclosing group to break
    Hard,
    /// A blank line the author wrote, kept because it separates ideas
    Blank,
    /// One thing when the enclosing group is flat, another when it breaks
    IfBreak(Box<Doc>, Box<Doc>),
    /// Flat if it fits the line, broken if it does not
    Group(Box<Doc>),
    /// One more level of indentation for anything inside
    Indent(Box<Doc>),
    /// In order
    Concat(Vec<Doc>),
    /// A span of ordinary Luau for larvae to format and splice in
    Host {
        /// Byte offset the span starts at
        start: u32,
        /// Byte offset one past its end
        end: u32,
        /// How larvae parses it
        parse: HostParse,
    },
}

/// How a [`Doc::Host`] span parses
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostParse {
    /// Statements, the shape between markup regions
    Block,
    /// One expression, a `{expr}` hole or attribute value
    Expr,
}

impl Doc {
    /// A verbatim slice of the source
    pub fn src(start: u32, end: u32) -> Self {
        Self::Src(start, end)
    }

    /// Text the worm generated
    pub fn lit(s: impl Into<String>) -> Self {
        Self::Lit(s.into())
    }

    /// Flat if it fits, broken if it does not
    pub fn group(inner: Doc) -> Self {
        Self::Group(Box::new(inner))
    }

    /// One more level of indentation for anything inside
    pub fn indent(inner: Doc) -> Self {
        Self::Indent(Box::new(inner))
    }

    /// `broken` only when the enclosing group breaks, `flat` otherwise
    pub fn if_break(flat: Doc, broken: Doc) -> Self {
        Self::IfBreak(Box::new(flat), Box::new(broken))
    }

    /// The parts in order
    pub fn concat(parts: impl IntoIterator<Item = Doc>) -> Self {
        Self::Concat(parts.into_iter().collect())
    }

    /// `parts` separated by `sep`, which is the shape most lists take
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

    /// A span holding one Luau expression for larvae to format
    pub fn host_expr(start: u32, end: u32) -> Self {
        Self::Host {
            start,
            end,
            parse: HostParse::Expr,
        }
    }
}

/// One problem found, severity deliberately absent since the host owns it
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    /// Byte range in the source
    pub span: (u32, u32),
    /// The lint's name, which must be declared in your `worm.toml` `[lints]`
    pub lint: String,
    /// What is wrong
    pub message: String,
    /// How to fix it, when there is something short to say
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

impl Finding {
    /// A finding, help added with [`with_help`](Self::with_help)
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

/// What [`Handler::format`] returns
#[derive(Debug, Clone, PartialEq)]
pub struct Format {
    /// The layout for the whole file
    pub document: Doc,
    /// Every comment's span, so larvae can refuse a layout that lost one
    pub comments: Vec<(u32, u32)>,
}

/// What [`Handler::lint`] returns
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Lint {
    /// The problems found
    pub findings: Vec<Finding>,
    /// Comment spans, so `-- larvae: allow(...)` works in a claimed file.
    /// Leave empty to opt out of suppression.
    pub comments: Vec<(u32, u32)>,
}

/**
A worm's operations, each defaulting to a refusal.

Implement the ones your worm.toml declares: `transform` for a `[frontend]`,
`format` when it says `fmt = true`, `lint` when it declares `[lints]`. An op
larvae never sends is never called, so the defaults only speak when a
manifest and its worm disagree.
*/
pub trait Handler {
    /// Settings and enabled rules, once, before any file
    fn init(&mut self, config: &str, rules: &str) -> Result<(), String> {
        let _ = (config, rules);

        Ok(())
    }

    /// Turn a claimed file into Luau
    fn transform(&mut self, source: &str) -> Result<String, String> {
        let _ = source;

        Err("this worm does not transform".into())
    }

    /// Lay a claimed file out for larvae to render
    fn format(&mut self, source: &str) -> Result<Format, String> {
        let _ = source;

        Err("this worm does not format".into())
    }

    /// Report a claimed file's problems
    fn lint(&mut self, source: &str) -> Result<Lint, String> {
        let _ = source;

        Err("this worm does not lint".into())
    }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Init {
        #[serde(default)]
        config: String,
        #[serde(default)]
        rules: String,
        /// Absent from hosts older than the format op, which never send it
        #[serde(default)]
        doc_version: u32,
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

Handler errors become `{"ok": false, "error": ...}` replies rather than ending
the process, matching how larvae treats them: an error against one file, never
the end of a run. The function only returns when stdin reaches end of file,
which is larvae dropping the worm, so returning `()` from `main` right after
is the clean shutdown.
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
        } => {
            // 0 is a host from before the format op existed, which will never
            // send one, so only an actual mismatch is worth refusing over
            if doc_version != 0 && doc_version != DOC_VERSION {
                return error_reply(format!(
                    "this worm speaks doc v{DOC_VERSION}, larvae speaks v{doc_version}"
                ));
            }

            handler
                .init(&config, &rules)
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

/// One length prefixed frame, or `None` at end of file
fn read_frame(input: &mut impl Read) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    input.read_exact(&mut len).ok()?;

    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    input.read_exact(&mut body).ok()?;

    Some(body)
}

fn write_frame(output: &mut impl Write, body: &[u8]) {
    let len = u32::try_from(body.len()).expect("a reply under 4GB");

    // a write failing means larvae is gone, and there is nobody left to tell
    let _ = output.write_all(&len.to_le_bytes());
    let _ = output.write_all(body);
    let _ = output.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact JSON larvae's own shape tests pin, from the guest side
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
