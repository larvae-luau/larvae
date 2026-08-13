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

`larvae_transform` returns a pointer to a three word header,
`[out_ptr, out_len, ok]`. `ok` is 1 when the bytes are output and 0 when they
are an error message. The header lives in a static, so the host does not free
it. The host calls `larvae_dealloc(out_ptr, out_len)` when it has read the
payload out.
*/

#![deny(missing_docs)]

/// The ABI revision this crate implements. It must match `api` in `worm.toml`.
pub const ABI_VERSION: u32 = 1;

pub mod abi;
#[cfg(feature = "native")]
pub mod native;
pub mod node;

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
