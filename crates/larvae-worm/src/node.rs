//! The guest side of the node API, so a rule reads an ordinary type and not offsets
//!
//! A rule runs in the wasm form, where larvae hands each host function below to
//! the module as it instantiates it. [`Node`] and every method of it stay on
//! each target all the same, so a worm reads one API whichever form it ships
//! as, and the `rules!` macro expands wherever it is written.

use crate::abi;

// The host functions, which larvae supplies to a wasm module. rustdoc writes
// nothing for an extern block, so this is a plain comment.
#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "larvae")]
unsafe extern "C" {
    #[link_name = "node_kind"]
    safe fn host_node_kind(epoch: u64, id: u32) -> i64;
    #[link_name = "node_text"]
    safe fn host_node_text(epoch: u64, id: u32) -> i64;
    #[link_name = "node_span_start"]
    safe fn host_span_start(epoch: u64, id: u32) -> i64;
    #[link_name = "node_span_end"]
    safe fn host_span_end(epoch: u64, id: u32) -> i64;
    #[link_name = "node_parent"]
    safe fn host_parent(epoch: u64, id: u32) -> i64;
    #[link_name = "node_child_count"]
    safe fn host_child_count(epoch: u64, id: u32) -> i64;
    #[link_name = "node_child"]
    safe fn host_child(epoch: u64, id: u32, index: u32) -> i64;
    #[link_name = "take_str"]
    safe fn host_take_str(ptr: u32, len: u32) -> i64;
    #[link_name = "replace"]
    safe fn host_replace(epoch: u64, id: u32, ptr: u32, len: u32) -> i64;
    #[link_name = "remove"]
    safe fn host_remove(epoch: u64, id: u32) -> i64;
}

/// The same names on a target that is not wasm, where no host answers them.
///
/// A worm that is not wasm holds no node: larvae gives a native worm one file
/// at a time, and a Luau worm runs in the interpreter. So nothing here runs,
/// and each one says so if it ever does.
///
/// The names have to exist all the same. An `extern` block outside wasm is a
/// symbol that something must define, and `wasm_import_module` means nothing
/// there. `link.exe` reads every object that it links and refuses a native
/// worm over the ten names, while the linkers of linux and of macos drop the
/// code of a rule first and refuse nothing, so two platforms of three hide the
/// problem. One name hides even better: `remove` is a function of the C
/// library, so a linker binds that import to the one that deletes a file.
#[cfg(not(target_arch = "wasm32"))]
mod outside_wasm {
    pub fn host_node_kind(_epoch: u64, _id: u32) -> i64 {
        absent()
    }

    pub fn host_node_text(_epoch: u64, _id: u32) -> i64 {
        absent()
    }

    pub fn host_span_start(_epoch: u64, _id: u32) -> i64 {
        absent()
    }

    pub fn host_span_end(_epoch: u64, _id: u32) -> i64 {
        absent()
    }

    pub fn host_parent(_epoch: u64, _id: u32) -> i64 {
        absent()
    }

    pub fn host_child_count(_epoch: u64, _id: u32) -> i64 {
        absent()
    }

    pub fn host_child(_epoch: u64, _id: u32, _index: u32) -> i64 {
        absent()
    }

    pub fn host_take_str(_ptr: u32, _len: u32) -> i64 {
        absent()
    }

    pub fn host_replace(_epoch: u64, _id: u32, _ptr: u32, _len: u32) -> i64 {
        absent()
    }

    pub fn host_remove(_epoch: u64, _id: u32) -> i64 {
        absent()
    }

    fn absent() -> ! {
        unreachable!("the node API belongs to the wasm form, and this worm is not wasm")
    }
}

#[cfg(not(target_arch = "wasm32"))]
use outside_wasm::*;

/// A handle to one node of the larvae AST, valid only for the file it came from
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    epoch: u64,
    id: u32,
}

impl Node {
    /// Rebuild a handle that the host named. Only the generated exports call this.
    #[doc(hidden)]
    pub fn from_raw(epoch: u64, id: u32) -> Self {
        Self { epoch, id }
    }

    /// The kind of this node, for example `"CallExpr"`
    pub fn kind(&self) -> String {
        pull(host_node_kind(self.epoch, self.id))
    }

    /// The source text that this node covers
    pub fn text(&self) -> String {
        pull(host_node_text(self.epoch, self.id))
    }

    /// Byte offsets into the original source, as a half open range
    pub fn span(&self) -> (u32, u32) {
        let start = host_span_start(self.epoch, self.id).max(0) as u32;
        let end = host_span_end(self.epoch, self.id).max(0) as u32;

        (start, end)
    }

    /// The node that contains this one. Only the root has none.
    pub fn parent(&self) -> Option<Node> {
        match host_parent(self.epoch, self.id) {
            id if id < 0 => None,

            id => Some(Node::from_raw(self.epoch, id as u32)),
        }
    }

    /// The direct children, in source order
    pub fn children(&self) -> Vec<Node> {
        let count = host_child_count(self.epoch, self.id).max(0) as u32;

        (0..count)
            .filter_map(|i| match host_child(self.epoch, self.id, i) {
                id if id < 0 => None,

                id => Some(Node::from_raw(self.epoch, id as u32)),
            })
            .collect()
    }

    /// Queue a replacement of the bytes of this node
    pub fn replace(&self, text: &str) -> bool {
        host_replace(self.epoch, self.id, text.as_ptr() as u32, text.len() as u32) >= 0
    }

    /// Queue a removal. larvae keeps the newlines, so the line counts hold.
    pub fn remove(&self) -> bool {
        host_remove(self.epoch, self.id) >= 0
    }
}

/*
An accessor stages its text on the host side and returns a length, because a
wasm function returns one number. The guest allocates that many bytes and asks
for the copy. Thus the host needs no allocator on the guest side of the
boundary.
*/
fn pull(len: i64) -> String {
    if len <= 0 {
        return String::new();
    }

    let len = len as u32;
    let buf = abi::alloc(len);
    let written = host_take_str(buf as u32, len);

    if written < 0 {
        // SAFETY: buf came from alloc with exactly len bytes, and no code uses it
        unsafe { abi::dealloc(buf, len) };

        return String::new();
    }

    // SAFETY: the host wrote `written` bytes of a &str that it held, so this is utf-8
    unsafe {
        let bytes = std::slice::from_raw_parts(buf, written as usize).to_vec();
        abi::dealloc(buf, len);

        String::from_utf8_unchecked(bytes)
    }
}
