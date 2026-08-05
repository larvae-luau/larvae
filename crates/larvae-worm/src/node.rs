//! Guest side of the node API, so a rule reads an ordinary type rather than offsets

use crate::abi;

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

/// A handle to one node of larvae's AST, valid only during the file it came from
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    epoch: u64,
    id: u32,
}

impl Node {
    /// Rebuild a handle the host named, only the generated exports should call this
    #[doc(hidden)]
    pub fn from_raw(epoch: u64, id: u32) -> Self {
        Self { epoch, id }
    }

    /// What this node is, ex: `"CallExpr"`
    pub fn kind(&self) -> String {
        pull(host_node_kind(self.epoch, self.id))
    }

    /// The source text this node covers
    pub fn text(&self) -> String {
        pull(host_node_text(self.epoch, self.id))
    }

    /// Byte offsets into the original source, half open
    pub fn span(&self) -> (u32, u32) {
        let start = host_span_start(self.epoch, self.id).max(0) as u32;
        let end = host_span_end(self.epoch, self.id).max(0) as u32;

        (start, end)
    }

    /// The node this one sits inside, absent only for the root
    pub fn parent(&self) -> Option<Node> {
        match host_parent(self.epoch, self.id) {
            id if id < 0 => None,

            id => Some(Node::from_raw(self.epoch, id as u32)),
        }
    }

    /// Direct children, in source order
    pub fn children(&self) -> Vec<Node> {
        let count = host_child_count(self.epoch, self.id).max(0) as u32;

        (0..count)
            .filter_map(|i| match host_child(self.epoch, self.id, i) {
                id if id < 0 => None,

                id => Some(Node::from_raw(self.epoch, id as u32)),
            })
            .collect()
    }

    /// Queue a replacement of this node's bytes
    pub fn replace(&self, text: &str) -> bool {
        host_replace(self.epoch, self.id, text.as_ptr() as u32, text.len() as u32) >= 0
    }

    /// Queue a deletion. larvae keeps the newlines, so line counts hold.
    pub fn remove(&self) -> bool {
        host_remove(self.epoch, self.id) >= 0
    }
}

/*
An accessor stages its text host side and answers with a length, because a wasm
function returns one number. We allocate that much and ask for the copy, which
keeps the host free of any allocator on our side of the boundary.
*/
fn pull(len: i64) -> String {
    if len <= 0 {
        return String::new();
    }

    let len = len as u32;
    let buf = abi::alloc(len);
    let written = host_take_str(buf as u32, len);

    if written < 0 {
        // SAFETY: buf came from alloc with exactly len bytes and is unused
        unsafe { abi::dealloc(buf, len) };

        return String::new();
    }

    // SAFETY: the host wrote `written` bytes of a &str it held, so this is utf-8
    unsafe {
        let bytes = std::slice::from_raw_parts(buf, written as usize).to_vec();
        abi::dealloc(buf, len);

        String::from_utf8_unchecked(bytes)
    }
}
