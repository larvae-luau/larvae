/*!
The guest side of the larvae worm ABI.

A worm is a `wasm32` module that larvae loads and calls. wasm has no strings.
Thus all data crosses as an offset and a length into the linear memory of the
module. This crate owns that protocol, so a worm author does not write it:

```ignore
larvae_worm::frontend!(|source: &str, config: &str| -> anyhow::Result<String> {
    luaux::compile_configured(source, Backend::Vide, &Config::parse(config)?)
});
```

The macro is not the only entry point. [`abi`] is public and documented. Thus
a worm with an unusual design can export the raw functions itself, without a
copy of the macro.

# The ABI

A worm exports `memory`, plus:

| export | signature |
|---|---|
| `larvae_alloc` | `(len: u32) -> ptr` |
| `larvae_dealloc` | `(ptr, len: u32)` |
| `larvae_transform` | `(src_ptr, src_len, cfg_ptr, cfg_len) -> *header` |
| `larvae_init` | `(cfg_ptr, cfg_len, rules_ptr, rules_len)` |
| `larvae_visit` | `(rule, epoch, node_id)` |
| `larvae_format` | `(src_ptr, src_len) -> *header` |
| `larvae_lint` | `(src_ptr, src_len) -> *header` |
| `larvae_settings` | `(fmt_ptr, fmt_len, lint_ptr, lint_len)` |

`larvae_transform` returns a pointer to a three word header,
`[out_ptr, out_len, ok]`. `ok` is 1 when the bytes are output and 0 when they
are an error message. The header lives in a static, so the host does not free
it. The host calls `larvae_dealloc(out_ptr, out_len)` when it has read the
payload out.

`larvae_format` and `larvae_lint` return the same header. Their ok payload is
the JSON of a [`wire::Format`] or [`wire::Lint`] reply. The
[`formatter!`], [`linter!`], and [`settings!`] macros write these three
exports, behind the `wire` feature.
*/

#![deny(missing_docs)]

/// The ABI revision this crate implements. It must match `api` in `worm.toml`.
pub const ABI_VERSION: u32 = 1;

pub mod abi;
#[cfg(feature = "native")]
pub mod native;
pub mod node;
#[cfg(feature = "wire")]
pub mod wasm_ops;
#[cfg(feature = "wire")]
pub mod wire;

pub use node::Node;

/**
Define a front-end worm. It takes source text and returns transformed source.

The closure takes the contents of the file and the `[config.<name>]` table of
the worm, serialized again as TOML. It returns the transformed source. Each
error type that implements [`Display`](core::fmt::Display) works, so
`anyhow::Result<String>` is valid.

```ignore
larvae_worm::frontend!(|source: &str, _config: &str| -> Result<String, String> {
    Ok(source.replace("<>", "{}"))
});
```

The macro expands to the three exports in the module docs. Use it once per worm.
*/
#[macro_export]
macro_rules! frontend {
    ($handler:expr) => {
        /// Allocate `len` bytes for the host to write into
        #[unsafe(no_mangle)]
        pub extern "C" fn larvae_alloc(len: u32) -> *mut u8 {
            $crate::abi::alloc(len)
        }

        /// Release a buffer that the host does not need anymore
        #[unsafe(no_mangle)]
        pub extern "C" fn larvae_dealloc(ptr: *mut u8, len: u32) {
            // SAFETY: the host passes back only pointers that larvae_alloc
            // returned, with the length of the allocation
            unsafe { $crate::abi::dealloc(ptr, len) }
        }

        /// Transform `src` under `cfg` and return a pointer to the result header
        #[unsafe(no_mangle)]
        pub extern "C" fn larvae_transform(
            src_ptr: *const u8,
            src_len: u32,
            cfg_ptr: *const u8,
            cfg_len: u32,
        ) -> *const u32 {
            // SAFETY: larvae_alloc allocated both spans, and the host wrote
            // them and knows their lengths
            unsafe { $crate::abi::dispatch(src_ptr, src_len, cfg_ptr, cfg_len, $handler) }
        }
    };
}

/**
Define the rule half of a worm.

Each rule is a name and a handler. larvae calls a rule only on the nodes that
match the `filter` you declared in `worm.toml`. Thus undeclared kinds do not
cross the boundary.

```ignore
larvae_worm::rules! {
    "strip_debug" => |node: larvae_worm::Node| {
        if node.kind() == "CallExpr" && node.text().starts_with("dprint") {
            node.remove();
        }
    },
}
```

Combine this macro with [`frontend!`](crate::frontend) when a worm holds both roles.
*/
#[macro_export]
macro_rules! rules {
    ($($name:literal => $handler:expr),+ $(,)?) => {
        /// Rule ids are indexes into the order that is declared here
        #[unsafe(no_mangle)]
        pub extern "C" fn larvae_visit(rule: u32, epoch: u64, id: u32) {
            let node = $crate::Node::from_raw(epoch, id);
            let mut which = 0u32;

            $(
                if rule == which {
                    let _ = $name;
                    let handler = $handler;
                    handler(node);
                    return;
                }

                which += 1;
            )+

            let _ = which;
        }
    };
}

/**
Define the format half of a worm. It needs the `wire` feature.

The closure takes the contents of a claimed file and returns the layout as a
[`wire::Format`]. larvae renders the layout with the width and indentation of
the project, so no worm reimplements the printer. Set `fmt = true` under
`[frontend]` in your `worm.toml`, because larvae only calls the export that
the manifest promises.

The macro writes only the `larvae_format` export. Combine it with
[`frontend!`], which writes the allocator exports that every worm needs.

```ignore
larvae_worm::formatter!(|source: &str| -> Result<larvae_worm::wire::Format, String> {
    Ok(larvae_worm::wire::Format::spans(find_luau_regions(source)))
});
```
*/
#[cfg(feature = "wire")]
#[macro_export]
macro_rules! formatter {
    ($handler:expr) => {
        /// Lay out `src` and return a pointer to the result header
        #[unsafe(no_mangle)]
        pub extern "C" fn larvae_format(src_ptr: *const u8, src_len: u32) -> *const u32 {
            // SAFETY: larvae_alloc allocated the span, and the host wrote it
            // and knows its length
            unsafe { $crate::wasm_ops::dispatch_format(src_ptr, src_len, $handler) }
        }
    };
}

/**
Define the lint half of a worm. It needs the `wire` feature.

The closure takes the contents of a claimed file and returns the problems as
a [`wire::Lint`]. The findings carry no severity, because the host stamps the
levels from `[lint.rules]` and owns the exit codes. Declare each lint name
under `[lints]` in your `worm.toml`.

The macro writes only the `larvae_lint` export. Combine it with
[`frontend!`], which writes the allocator exports that every worm needs.

```ignore
larvae_worm::linter!(|source: &str| -> Result<larvae_worm::wire::Lint, String> {
    Ok(larvae_worm::wire::Lint::default())
});
```
*/
#[cfg(feature = "wire")]
#[macro_export]
macro_rules! linter {
    ($handler:expr) => {
        /// Report the problems of `src` and return a pointer to the result header
        #[unsafe(no_mangle)]
        pub extern "C" fn larvae_lint(src_ptr: *const u8, src_len: u32) -> *const u32 {
            // SAFETY: larvae_alloc allocated the span, and the host wrote it
            // and knows its length
            unsafe { $crate::wasm_ops::dispatch_lint(src_ptr, src_len, $handler) }
        }
    };
}

/**
Receive the settings of the project. It needs the `wire` feature.

The macro writes the `larvae_settings` export. The host calls the export once,
directly after init, with the resolved `[fmt]` table and the lint levels of
the project, both as JSON text. Read them back at any later point with
[`wasm_ops::settings`]. Thus the user states a width one time, and not a
second time under `[worms.<name>.config]`.

```ignore
larvae_worm::settings!();

fn width() -> Option<u64> {
    let (fmt, _lint) = larvae_worm::wasm_ops::settings();

    serde_json::from_str::<serde_json::Value>(&fmt).ok()?["column_width"].as_u64()
}
```
*/
#[cfg(feature = "wire")]
#[macro_export]
macro_rules! settings {
    () => {
        /// Store the settings of the project for `wasm_ops::settings` to return
        #[unsafe(no_mangle)]
        pub extern "C" fn larvae_settings(
            fmt_ptr: *const u8,
            fmt_len: u32,
            lint_ptr: *const u8,
            lint_len: u32,
        ) {
            // SAFETY: larvae_alloc allocated both spans, and the host wrote
            // them and knows their lengths
            unsafe { $crate::wasm_ops::store_settings(fmt_ptr, fmt_len, lint_ptr, lint_len) }
        }
    };
}
