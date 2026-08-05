//! Exercises every branch of the worm ABI from the guest side

larvae_worm::frontend!(|source: &str, config: &str| -> Result<String, String> {
    match source {
        // a worm reporting a problem rather than producing output
        "FAIL" => Err(format!("refused, config was {config:?}")),

        // a worm hitting a bug, which reaches the host as a trap
        "TRAP" => panic!("worm exploded"),

        _ => Ok(format!("{source}|{config}")),
    }
});

larvae_worm::rules! {
    // rule 0, reads the node and rewrites it in place
    "describe" => |node: larvae_worm::Node| {
        let (start, end) = node.span();
        let kids = node.children().len();

        node.replace(&format!("{}|{}|{start}:{end}|{kids}", node.kind(), node.text()));
    },

    // rule 1, deletes, and larvae keeps the newlines
    "drop" => |node: larvae_worm::Node| {
        node.remove();
    },

    // rule 2, walks up to prove parent works
    "up" => |node: larvae_worm::Node| {
        let kind = node.parent().map(|p| p.kind()).unwrap_or_else(|| "none".into());

        node.replace(&kind);
    },
}
