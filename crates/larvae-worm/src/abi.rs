//! The raw ABI, for a worm that wants the exports without [`frontend!`](crate::frontend)

use core::cell::UnsafeCell;

/// The bytes cross without a type, so the alignment does not matter
const ALIGN: usize = 1;

/*
The result header lives in a static and is not allocated. Thus the host frees
exactly one item, the payload, instead of two. wasm32 is single threaded, and
this fact makes the UnsafeCell sound here.
*/
struct Header(UnsafeCell<[u32; 3]>);

// SAFETY: wasm32-unknown-unknown is single threaded, so no other code can observe this
unsafe impl Sync for Header {}

static HEADER: Header = Header(UnsafeCell::new([0; 3]));

/// Allocate `len` bytes for the host to write into
pub fn alloc(len: u32) -> *mut u8 {
    if len == 0 {
        return ALIGN as *mut u8; // dangling but aligned, and no code dereferences it
    }

    let layout = core::alloc::Layout::from_size_align(len as usize, ALIGN)
        .expect("a byte layout is always valid");

    // SAFETY: len is not zero, so the layout has a size that is not zero
    unsafe { std::alloc::alloc(layout) }
}

/// Release a buffer that [`alloc`] returned before
///
/// # Safety
/// `ptr` must come from [`alloc`] with this exact `len`, and no code has freed
/// it before.
pub unsafe fn dealloc(ptr: *mut u8, len: u32) {
    if len == 0 {
        return;
    }

    let layout = core::alloc::Layout::from_size_align(len as usize, ALIGN)
        .expect("a byte layout is always valid");

    // SAFETY: the caller guarantees that ptr came from alloc with this len
    unsafe { std::alloc::dealloc(ptr, layout) }
}

/**
Run `handler` over two byte spans from the host and publish the result.

The function returns a pointer to `[out_ptr, out_len, ok]`. When the handler
fails, or when a span is not UTF-8, `ok` is 0 and the payload is the message.

# Safety
Both `(ptr, len)` pairs must describe spans that this module allocated and
that the host filled in.
*/
pub unsafe fn dispatch<F, E>(
    src_ptr: *const u8,
    src_len: u32,
    cfg_ptr: *const u8,
    cfg_len: u32,
    handler: F,
) -> *const u32
where
    F: FnOnce(&str, &str) -> Result<String, E>,
    E: core::fmt::Display,
{
    // SAFETY: the caller guarantees that both spans are live and have the correct size
    let src = unsafe { core::slice::from_raw_parts(src_ptr, src_len as usize) };
    let cfg = unsafe { core::slice::from_raw_parts(cfg_ptr, cfg_len as usize) };

    let (text, ok) = match (core::str::from_utf8(src), core::str::from_utf8(cfg)) {
        (Ok(src), Ok(cfg)) => match handler(src, cfg) {
            Ok(out) => (out, 1),

            Err(e) => (e.to_string(), 0),
        },

        _ => ("source and config must both be utf-8".to_owned(), 0),
    };

    publish(text, ok)
}

/// Copy `text` into a new allocation and point the header at it
fn publish(text: String, ok: u32) -> *const u32 {
    let bytes = text.as_bytes();
    let len = bytes.len() as u32;
    let out = alloc(len);

    if len > 0 {
        // SAFETY: out is a new allocation of exactly len bytes, and the source
        // is a live String that lives longer than the copy
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, len as usize) };
    }

    let header = HEADER.0.get();

    // SAFETY: the module is single threaded, and no code holds a reference across this write
    unsafe { *header = [out as u32, len, ok] };

    header.cast()
}
