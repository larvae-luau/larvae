//! Exercises every branch of the worm ABI from the guest side

/// The spans of the leading `--` comment lines, one span per line
fn leading_comments(source: &str) -> Vec<(u32, u32)> {
    let mut comments = Vec::new();
    let mut at = 0u32;

    for line in source.split_inclusive('\n') {
        let text = line.trim_end_matches(['\n', '\r']);

        if !text.starts_with("--") {
            break;
        }

        comments.push((at, at + text.len() as u32));
        at += line.len() as u32;
    }

    comments
}

larvae_worm::frontend!(|source: &str, config: &str| -> Result<String, String> {
    match source {
        // a worm reporting a problem rather than producing output
        "FAIL" => Err(format!("refused, config was {config:?}")),

        // a worm hitting a bug, which reaches the host as a trap
        "TRAP" => panic!("worm exploded"),

        _ => Ok(format!("{source}|{config}")),
    }
});

// The whole file as one host span, so larvae lays it out as Luau. The
// comment spans feed the survival backstop of the host.
larvae_worm::formatter!(|source: &str| -> Result<larvae_worm::wire::Format, String> {
    use larvae_worm::wire::{Doc, Format};

    match source {
        "FAIL" => Err("refused to format".to_owned()),

        _ => Ok(Format::document(Doc::host(0, source.len() as u32))
            .with_comments(leading_comments(source))),
    }
});

// One finding over the first "bad", a shadow equal to the source, and an
// echo of the stored settings when the file asks for them
larvae_worm::linter!(|source: &str| -> Result<larvae_worm::wire::Lint, String> {
    use larvae_worm::wire::{Finding, Lint};

    let mut lint = Lint::default();

    if let Some(at) = source.find("bad") {
        let at = at as u32;

        lint.findings
            .push(Finding::new("bad_word", (at, at + 3), "the word bad is bad"));
    }

    // the test that inits with settings reads them back through this finding
    if source.contains("SETTINGS") {
        let (fmt, levels) = larvae_worm::wasm_ops::settings();

        lint.findings.push(Finding::new(
            "settings_echo",
            (0, 0),
            format!("fmt={fmt}|lint={levels}"),
        ));
    }

    lint.comments = leading_comments(source);
    lint.luau = Some(source.to_owned());

    Ok(lint)
});

larvae_worm::settings!();

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
