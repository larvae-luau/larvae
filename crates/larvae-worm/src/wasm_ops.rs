/*!
The wasm side of the format and lint ops, behind the `wire` feature.

The [`formatter!`](crate::formatter), [`linter!`](crate::linter), and
[`settings!`](crate::settings) macros expand to calls into this module. The
functions here run a handler, turn its [`wire`](crate::wire) reply into the
JSON that larvae parses, and publish the JSON through the same header as
[`abi::dispatch`](crate::abi::dispatch). Thus the ok flag and the memory
discipline of the transform op also apply to these ops.
*/

use core::cell::UnsafeCell;

use crate::wire::{DOC_VERSION, Format, Lint};

/*
The settings live in a static, because larvae sends them once at init and the
ops read them later. wasm32-unknown-unknown is single threaded, and this fact
makes the UnsafeCell sound here, in the same way as the header in `abi`.
*/
struct Store(UnsafeCell<(String, String)>);

// SAFETY: wasm32-unknown-unknown is single threaded, so no other code can observe this
unsafe impl Sync for Store {}

static SETTINGS: Store = Store(UnsafeCell::new((String::new(), String::new())));

/**
Store the settings that larvae sent, for [`settings`] to return.

The host calls the `larvae_settings` export once, directly after
`larvae_init`. The [`settings!`](crate::settings) macro forwards the call
here.

# Safety
Both `(ptr, len)` pairs must describe spans that `larvae_alloc` returned and
that the host filled in.
*/
pub unsafe fn store_settings(fmt_ptr: *const u8, fmt_len: u32, lint_ptr: *const u8, lint_len: u32) {
    // SAFETY: the caller guarantees that both spans are live and have the correct size
    let fmt = unsafe { core::slice::from_raw_parts(fmt_ptr, fmt_len as usize) };
    let lint = unsafe { core::slice::from_raw_parts(lint_ptr, lint_len as usize) };

    let pair = (
        String::from_utf8_lossy(fmt).into_owned(),
        String::from_utf8_lossy(lint).into_owned(),
    );

    // SAFETY: the module is single threaded, and no code holds a reference across this write
    unsafe { *SETTINGS.0.get() = pair };
}

/**
The settings of the project, as larvae sent them at init.

The first string is the resolved `[fmt]` table as JSON, and the second string
holds the resolved lint levels as JSON. Both strings are empty before init,
and for a project that states nothing. A worm reads them to lay its own
constructs out in the style of the project.
*/
pub fn settings() -> (String, String) {
    // SAFETY: the module is single threaded, so no write overlaps this read
    unsafe { (*SETTINGS.0.get()).clone() }
}

/**
Run `handler` over the source span and publish the format reply as JSON.

The [`formatter!`](crate::formatter) macro forwards the `larvae_format`
export here. The return value is a pointer to the same `[out_ptr, out_len,
ok]` header that [`abi::dispatch`](crate::abi::dispatch) fills.

# Safety
`(src_ptr, src_len)` must describe a span that `larvae_alloc` returned and
that the host filled in.
*/
pub unsafe fn dispatch_format<F, E>(src_ptr: *const u8, src_len: u32, handler: F) -> *const u32
where
    F: FnOnce(&str) -> Result<Format, E>,
    E: core::fmt::Display,
{
    /*
    The source span doubles as an empty config span, because this op has no
    config. `abi::dispatch` reads zero bytes from the second span, so the
    pointer only has to be valid for the source.
    */
    // SAFETY: the caller guarantees that the source span is live and has the correct size
    unsafe {
        crate::abi::dispatch(src_ptr, src_len, src_ptr, 0, |src, _| {
            handler(src)
                .map(|format| encode_format(&format))
                .map_err(|e| e.to_string())
        })
    }
}

/**
Run `handler` over the source span and publish the lint reply as JSON.

The [`linter!`](crate::linter) macro forwards the `larvae_lint` export here.
The return value is a pointer to the same `[out_ptr, out_len, ok]` header
that [`abi::dispatch`](crate::abi::dispatch) fills.

# Safety
`(src_ptr, src_len)` must describe a span that `larvae_alloc` returned and
that the host filled in.
*/
pub unsafe fn dispatch_lint<F, E>(src_ptr: *const u8, src_len: u32, handler: F) -> *const u32
where
    F: FnOnce(&str) -> Result<Lint, E>,
    E: core::fmt::Display,
{
    // SAFETY: the caller guarantees that the source span is live and has the correct size
    unsafe {
        crate::abi::dispatch(src_ptr, src_len, src_ptr, 0, |src, _| {
            handler(src)
                .map(|lint| encode_lint(&lint))
                .map_err(|e| e.to_string())
        })
    }
}

/*
The field set of a native format reply, without the `ok` flag, because on
this transport the ok flag crosses in the header. The keys are inserted in
alphabetical order, so the output text does not depend on the map type that
serde_json compiles with.
*/
fn encode_format(format: &Format) -> String {
    let value = serde_json::json!({
        "comments": format.comments,
        "doc": DOC_VERSION,
        "document": format.document,
        "spans": format.spans,
    });

    serde_json::to_string(&value).expect("a reply always serialises")
}

/// The field set of a native lint reply, without the `ok` flag
fn encode_lint(lint: &Lint) -> String {
    let value = serde_json::json!({
        "comments": lint.comments,
        "findings": lint.findings,
        "luau": lint.luau,
    });

    serde_json::to_string(&value).expect("a reply always serialises")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Doc, Finding};

    /*
    The values that `worm/proto.rs` deserializes as a FormatReply.

    The comparison parses both sides, because serde_json orders the keys of
    a `json!` map alphabetically and the order of keys is not part of the
    contract. The values are.
    */
    #[test]
    fn a_format_reply_encodes_the_documented_shape() {
        let format = Format::document(Doc::host(0, 4)).with_comments(vec![(0, 2)]);

        let sent: serde_json::Value = serde_json::from_str(&encode_format(&format)).unwrap();

        assert_eq!(
            sent,
            serde_json::json!({
                "doc": 1,
                "document": { "host": { "start": 0, "end": 4, "parse": "block" } },
                "spans": [],
                "comments": [[0, 2]],
            })
        );
    }

    /// The values that `worm/proto.rs` deserializes as a LintReply
    #[test]
    fn a_lint_reply_encodes_the_documented_shape() {
        let lint = Lint {
            findings: vec![Finding::new("tidy", (2, 7), "untidy")],
            luau: Some("shadow".into()),
            comments: vec![(0, 1)],
        };

        let sent: serde_json::Value = serde_json::from_str(&encode_lint(&lint)).unwrap();

        assert_eq!(
            sent,
            serde_json::json!({
                "findings": [{ "span": [2, 7], "lint": "tidy", "message": "untidy" }],
                "luau": "shadow",
                "comments": [[0, 1]],
            })
        );
    }

    /// One test owns the static, because the settings store is process wide
    #[test]
    fn settings_store_and_read_back() {
        assert_eq!(settings(), (String::new(), String::new()));

        let fmt = "{\"column_width\":100}";
        let lint = "{\"tidy\":\"warn\"}";

        // SAFETY: both spans borrow live string data with the exact length
        unsafe {
            store_settings(
                fmt.as_ptr(),
                fmt.len() as u32,
                lint.as_ptr(),
                lint.len() as u32,
            );
        }

        assert_eq!(settings(), (fmt.to_string(), lint.to_string()));
    }
}
