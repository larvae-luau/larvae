//! The crate links into a program that is not wasm.
//!
//! A test is a program, so this file is the check itself: it takes the address
//! of each method that reaches a host function, which makes the linker resolve
//! every name behind them. An `extern` block that no `cfg` guards then fails
//! the run.
//!
//! The failure that this holds off is quiet on two platforms of three. A native
//! worm on windows reports nine unresolved symbols from `link.exe`, among them
//! `node_kind` and `take_str`, while linux and macos drop the code of a rule
//! and say nothing. Nothing in this workspace links `larvae-worm` either, so
//! `cargo build --all-targets` passed for as long as this test was absent.

use larvae_worm::Node;

#[test]
fn every_host_function_resolves_outside_wasm() {
    // The address of a function, and never a call of it: a call of one of
    // these outside wasm is a mistake, and the point here is the linker.
    let reads: [usize; 4] = [
        Node::kind as *const () as usize,
        Node::text as *const () as usize,
        Node::span as *const () as usize,
        Node::parent as *const () as usize,
    ];

    let writes: [usize; 3] = [
        Node::children as *const () as usize,
        Node::replace as *const () as usize,
        Node::remove as *const () as usize,
    ];

    assert!(
        reads
            .iter()
            .chain(writes.iter())
            .all(|address| *address != 0)
    );
}
