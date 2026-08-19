/*!
Holding the formatter out of a region, in a file a worm claims.

A worm owns the layout of the file it claims, so larvae cannot hold the
formatter out of one region the way it does in Luau: the markup around the
Luau is the worm's to lay out, and the worm knows nothing about
`-- larvae: fmt off`. Larvae renders what the worm sent and writes the source
back over each held region afterwards.

Each boundary is found by the means that suits it. A region opens at a marker
comment, and every comment of a worm reaches the output, so the text of the
marker locates the start. An `on` marker closes most regions the same way. A
count closes at a source position that no comment names, and a mark planted in
the document answers for that one.
*/

use super::*;

/*
The mark that stands for one count boundary in the rendered text.

The private use area holds no character that Luau or a markup dialect writes,
so a mark cannot collide with the content of the file. One character keeps the
mark out of the width the renderer measures, so a line that a mark sits on
breaks where it would break without one.
*/
pub(super) fn mark(index: usize) -> String {
    char::from_u32(0xE000 + index as u32)
        .expect("the private use area holds every index larvae plants")
        .to_string()
}

/*
The closing position of every region that no `on` marker closes.

A region that runs to the end of the file is left out, because it closes at
the end of the output and needs no mark.
*/
pub(super) fn counts_of(src: &str, ignored: &[(u32, u32)], comments: &[(u32, u32)]) -> Vec<u32> {
    ignored
        .iter()
        .filter(|&&(lo, hi)| hi < src.len() as u32 && !closed(src, lo, hi, comments))
        .map(|&(_, hi)| hi)
        .collect()
}

/// Reports if an `on` marker is the last comment of this region.
fn closed(src: &str, lo: u32, hi: u32, comments: &[(u32, u32)]) -> bool {
    let last = comments
        .iter()
        .filter(|&&(start, _)| start >= lo && start < hi)
        .max_by_key(|&&(start, _)| start);

    let Some(&(start, end)) = last else {
        return false;
    };

    matches!(
        crate::flags::switch(&src[start as usize..end as usize]),
        Some((crate::flags::Subject::Fmt, crate::flags::Switch::On))
    )
}

/*
Plants a mark in the document where each boundary opens.

A `src` node often reaches back over the newline that ends the line before
it, so the boundary sits inside the node and not at its start. Such a node is
split at the boundary and the mark goes between the halves, which puts it at
the head of the line in the rendered text. A boundary at or before the start
of a node needs no split, and the mark goes in front of the node.

A `host` node is parsed Luau, so it is never split. A boundary inside one
cannot be placed.

None means larvae could not place every mark, and then it plants none. A worm
is free to build a document that this walk does not follow, and a splice on
marks that are not all there would cut the wrong text.
*/
pub(super) fn plant(document: &WireDoc, bounds: &[u32]) -> Option<WireDoc> {
    if bounds.is_empty() {
        return None;
    }

    let mut next = 0;
    let marked = walk(document, bounds, &mut next);

    match next == bounds.len() {
        true => Some(marked),

        false => None,
    }
}

fn walk(node: &WireDoc, bounds: &[u32], next: &mut usize) -> WireDoc {
    match node {
        WireDoc::Concat(parts) => {
            WireDoc::Concat(parts.iter().map(|p| walk(p, bounds, next)).collect())
        }

        WireDoc::Group(inner) => WireDoc::Group(Box::new(walk(inner, bounds, next))),

        WireDoc::Indent(inner) => WireDoc::Indent(Box::new(walk(inner, bounds, next))),

        WireDoc::Src(start, end) => split(*start, *end, node, bounds, next),

        WireDoc::Host { start, .. } => ahead(*start, node, bounds, next),

        /*
        An `if_break` holds two arms, and the render keeps one of them. A
        walk into both would plant each mark two times, and a walk into one
        would plant it where the render may not keep it. So neither arm is
        followed, and a boundary inside one drops the whole splice.
        */
        other => other.clone(),
    }
}

/// Cuts a source node at each boundary inside it, and puts the mark at the cut.
fn split(start: u32, end: u32, node: &WireDoc, bounds: &[u32], next: &mut usize) -> WireDoc {
    if *next >= bounds.len() || bounds[*next] >= end {
        return ahead(start, node, bounds, next);
    }

    let mut parts = Vec::new();
    let mut at = start;

    while *next < bounds.len() && bounds[*next] < end {
        let cut = bounds[*next].max(start);

        if cut > at {
            parts.push(WireDoc::Src(at, cut));
        }

        parts.push(WireDoc::Lit(mark(*next)));

        at = cut;
        *next += 1;
    }

    if at < end {
        parts.push(WireDoc::Src(at, end));
    }

    WireDoc::Concat(parts)
}

/// Puts the marks that this node's position has reached in front of it.
fn ahead(start: u32, node: &WireDoc, bounds: &[u32], next: &mut usize) -> WireDoc {
    if *next >= bounds.len() || bounds[*next] > start {
        return node.clone();
    }

    let mut parts = Vec::new();

    while *next < bounds.len() && bounds[*next] <= start {
        parts.push(WireDoc::Lit(mark(*next)));
        *next += 1;
    }

    parts.push(node.clone());

    WireDoc::Concat(parts)
}

/*
Puts the held regions of a claimed file back as the author wrote them.

A worm owns the layout of the file it claims, so larvae cannot hold the
formatter out of one region the way it does in a Luau file. It renders what
the worm sent and writes the source over each region afterwards.

Each boundary is found by the means that suits it. A region opens at a marker
comment, and every comment of a worm reaches the output, so the text of the
marker locates the start. An `on` marker closes most regions and locates the
end the same way. A count closes at a source position, and a mark planted in
the document answers for that one. A region with no `on` and no count runs to
the end of the file.

The search for a comment runs forward and never looks back, so a marker text
that appears more than one time still pairs with the right occurrence.
*/
pub(super) fn hold_regions(
    src: &str,
    out: String,
    ignored: &[(u32, u32)],
    comments: &[(u32, u32)],
) -> String {
    let mut sorted = comments.to_vec();
    sorted.sort_unstable();

    let mut result = String::with_capacity(out.len());
    let mut taken = 0;
    let mut scanned = 0;
    let mut planted = 0;

    for &(lo, hi) in ignored {
        let inside: Vec<(u32, u32)> = sorted
            .iter()
            .copied()
            .filter(|&(start, _)| start >= lo && start < hi)
            .collect();

        // A region always opens at a marker, so an empty list is not one.
        let (Some(&open), Some(&close)) = (inside.first(), inside.last()) else {
            continue;
        };

        let Some(at) = seek(&out, src, open, scanned.max(taken)) else {
            continue;
        };

        let from = line_start(&out, at);

        let to = if hi >= src.len() as u32 {
            out.len()
        } else if closed(src, lo, hi, comments) {
            match seek(&out, src, close, at) {
                Some(end) => line_end(&out, end),

                None => continue,
            }
        } else {
            let found = find(&out, planted, at);
            planted += 1;

            match found {
                Some(end) => line_start(&out, end),

                None => continue,
            }
        };

        scanned = to;

        if from < taken || to < from {
            continue;
        }

        result.push_str(&out[taken..from]);
        result.push_str(src[lo as usize..hi as usize].trim_end_matches('\n'));
        result.push('\n');

        taken = to;
    }

    result.push_str(&out[taken..]);

    // A mark that no region used must not reach the file.
    result.retain(|c| !('\u{E000}'..'\u{F000}').contains(&c));

    result
}

/// Where the text of this comment landed in the rendered output, at or after `from`.
fn seek(out: &str, src: &str, comment: (u32, u32), from: usize) -> Option<usize> {
    let text = src[comment.0 as usize..comment.1 as usize].trim_end();

    out.get(from..)
        .and_then(|rest| rest.find(text))
        .map(|at| from + at)
}

/// Where this mark landed in the rendered output, at or after `from`.
fn find(out: &str, index: usize, from: usize) -> Option<usize> {
    out.get(from..)
        .and_then(|rest| rest.find(&mark(index)))
        .map(|at| from + at)
}

fn line_start(s: &str, at: usize) -> usize {
    s[..at].rfind('\n').map_or(0, |n| n + 1)
}

/// The byte just past the newline that ends this line, or the end of the text.
fn line_end(s: &str, at: usize) -> usize {
    s[at..].find('\n').map_or(s.len(), |n| at + n + 1)
}

#[cfg(test)]
mod flags_in_a_claimed_file {
    use super::*;
    use crate::fmt::FmtConfig;

    fn comment_at(src: &str, text: &str) -> (u32, u32) {
        let at = src.find(text).expect("the marker is in the source") as u32;

        (at, at + text.len() as u32)
    }

    fn render(src: &str, spans: Vec<(u32, u32)>, comments: Vec<(u32, u32)>) -> String {
        let reply = FormatReply {
            doc: DOC_VERSION,
            document: None,
            spans,
            comments,
        };

        render_format(src, &reply, &FmtConfig::default()).unwrap()
    }

    /*
    A marker inside a claimed file reads as it does in a Luau file.

    Larvae does not read a claimed file as Luau, so it finds no comment in one
    itself. The worm reports where the comments are, and the flags come from
    that list.
    */
    #[test]
    fn a_region_inside_a_claimed_file_is_untouched() {
        let src = "<F>\n-- larvae: fmt off\nlocal  m = {1,0}\n-- larvae: fmt on\n</F>\n";
        let off = comment_at(src, "-- larvae: fmt off");
        let on = comment_at(src, "-- larvae: fmt on");
        let end = src.find("\n</F>").unwrap() as u32;

        let out = render(src, vec![(off.0, end)], vec![off, on]);

        assert!(out.contains("local  m = {1,0}"), "{out}");
    }

    #[test]
    fn a_claimed_file_held_off_in_full_comes_back_unchanged() {
        let src = "<F>\n-- larvae: fmt off\nlocal  m = {1,0}\n</F>\n";
        let off = comment_at(src, "-- larvae: fmt off");
        let end = src.find("\n</F>").unwrap() as u32;

        assert_eq!(render(src, vec![(off.0, end)], vec![off]), src);
    }

    /// The Luau outside the region still gets laid out.
    #[test]
    fn only_the_held_lines_are_left_alone() {
        let src = "<F>\nlocal  a  = 1\n-- larvae: fmt off\nlocal  m = {1,0}\n-- larvae: fmt on\nlocal  b  = 2\n</F>\n";
        let off = comment_at(src, "-- larvae: fmt off");
        let on = comment_at(src, "-- larvae: fmt on");
        let start = src.find("local  a").unwrap() as u32;
        let end = src.find("\n</F>").unwrap() as u32;

        let out = render(src, vec![(start, end)], vec![off, on]);

        assert!(out.contains("local a = 1"), "outside the region: {out}");
        assert!(out.contains("local b = 2"), "outside the region: {out}");
        assert!(out.contains("local  m = {1,0}"), "inside it: {out}");
    }

    #[test]
    fn a_count_holds_that_many_lines_in_a_claimed_file() {
        let src = "<F>\n-- larvae: fmt off(1)\nlocal  m = {1,0}\nlocal  n  = 2\n</F>\n";
        let off = comment_at(src, "-- larvae: fmt off(1)");
        let start = off.0;
        let end = src.find("\n</F>").unwrap() as u32;

        let out = render(src, vec![(start, end)], vec![off]);

        assert!(out.contains("local  m = {1,0}"), "held: {out}");
        assert!(out.contains("local n = 2"), "not held: {out}");
    }

    /*
    A worm that owns the layout of the file must still honour a region.

    The worm sends a whole document for a file it claims, and the document
    carries text of its own as `lit`, which has no source span. So larvae
    cannot hold the formatter out of a range of that document. It renders
    what the worm sent and then puts the source back over each held region.

    The document here does what a markup worm does with the attributes of an
    element: it rewrites them onto one line of its own choosing.
    */
    #[test]
    fn a_region_holds_a_worm_that_sends_a_whole_document() {
        let src = concat!(
            "-- larvae: fmt off\n",
            "local root = <ScreenGui Name=\"ROOT\" Parent={ Gui } ResetOnSpawn={ false }>\n",
            "  <TextLabel>test</TextLabel>\n",
            "</ScreenGui>\n",
            "-- larvae: fmt on\n",
            "return root\n",
        );

        let off = comment_at(src, "-- larvae: fmt off");
        let on = comment_at(src, "-- larvae: fmt on");

        // what a markup worm sends: one attribute per line, its own text
        let document = WireDoc::Concat(vec![
            WireDoc::Src(off.0, off.1),
            WireDoc::Hard,
            WireDoc::Lit("local root = <ScreenGui".to_string()),
            WireDoc::Hard,
            WireDoc::Lit("\tName=\"ROOT\"".to_string()),
            WireDoc::Hard,
            WireDoc::Lit("\tParent={ Gui }".to_string()),
            WireDoc::Hard,
            WireDoc::Lit("\tResetOnSpawn={ false }".to_string()),
            WireDoc::Hard,
            WireDoc::Lit(">".to_string()),
            WireDoc::Hard,
            WireDoc::Lit("\t<TextLabel>test</TextLabel>".to_string()),
            WireDoc::Hard,
            WireDoc::Lit("</ScreenGui>".to_string()),
            WireDoc::Hard,
            WireDoc::Src(on.0, on.1),
            WireDoc::Hard,
            WireDoc::Lit("return root".to_string()),
        ]);

        let reply = FormatReply {
            doc: DOC_VERSION,
            document: Some(document),
            spans: Vec::new(),
            comments: vec![off, on],
        };

        let out = render_format(src, &reply, &FmtConfig::default()).unwrap();

        assert!(
            out.contains(
                "local root = <ScreenGui Name=\"ROOT\" Parent={ Gui } ResetOnSpawn={ false }>"
            ),
            "the attributes moved inside the region: {out}"
        );
        assert!(
            out.contains("  <TextLabel>test</TextLabel>"),
            "the indentation of the region moved: {out}"
        );

        // outside the region the worm still owns the layout
        assert!(out.contains("return root"), "{out}");
    }

    /// Everything the worm wrote outside a region survives the splice.
    #[test]
    fn a_region_does_not_eat_the_lines_around_it() {
        let src = concat!(
            "local  a  = 1\n",
            "-- larvae: fmt off\n",
            "local  m = {1,0}\n",
            "-- larvae: fmt on\n",
            "local  b  = 2\n",
        );

        let off = comment_at(src, "-- larvae: fmt off");
        let on = comment_at(src, "-- larvae: fmt on");

        let document = WireDoc::Concat(vec![
            WireDoc::Lit("local a = 1".to_string()),
            WireDoc::Hard,
            WireDoc::Src(off.0, off.1),
            WireDoc::Hard,
            WireDoc::Lit("local m = { 1, 0 }".to_string()),
            WireDoc::Hard,
            WireDoc::Src(on.0, on.1),
            WireDoc::Hard,
            WireDoc::Lit("local b = 2".to_string()),
        ]);

        let reply = FormatReply {
            doc: DOC_VERSION,
            document: Some(document),
            spans: Vec::new(),
            comments: vec![off, on],
        };

        let out = render_format(src, &reply, &FmtConfig::default()).unwrap();

        assert_eq!(
            out,
            "local a = 1\n-- larvae: fmt off\nlocal  m = {1,0}\n-- larvae: fmt on\nlocal b = 2\n",
            "{out}"
        );
    }

    /*
    The count form works in a claimed file too.

    No `on` marker closes such a region, so the end of it is a source
    position and not a comment. The marks answer for it in the same way as
    they answer for a pair of markers.
    */
    #[test]
    fn a_count_holds_that_many_lines_of_a_whole_document() {
        let src = concat!(
            "local  a  = 1\n",
            "-- larvae: fmt off(2)\n",
            "local  m = {1,0}\n",
            "local  n = {2,3}\n",
            "local  b  = 2\n",
        );

        let off = comment_at(src, "-- larvae: fmt off(2)");

        let document = WireDoc::Concat(vec![
            WireDoc::Lit("local a = 1".to_string()),
            WireDoc::Hard,
            WireDoc::Src(off.0, off.1),
            WireDoc::Hard,
            WireDoc::Lit("local m = { 1, 0 }".to_string()),
            WireDoc::Hard,
            WireDoc::Lit("local n = { 2, 3 }".to_string()),
            WireDoc::Hard,
            WireDoc::Src(
                src.find("local  b").unwrap() as u32,
                src.find("  = 2").unwrap() as u32 + 5,
            ),
        ]);

        let reply = FormatReply {
            doc: DOC_VERSION,
            document: Some(document),
            spans: Vec::new(),
            comments: vec![off],
        };

        let out = render_format(src, &reply, &FmtConfig::default()).unwrap();

        assert_eq!(
            out,
            "local a = 1\n-- larvae: fmt off(2)\nlocal  m = {1,0}\nlocal  n = {2,3}\nlocal  b  = 2\n",
            "{out}"
        );
    }

    /// No mark may reach the file, whatever the document did.
    #[test]
    fn no_mark_survives_into_the_output() {
        let src = "-- larvae: fmt off\nlocal  m = {1,0}\n-- larvae: fmt on\nreturn m\n";
        let off = comment_at(src, "-- larvae: fmt off");
        let on = comment_at(src, "-- larvae: fmt on");

        let document = WireDoc::Concat(vec![
            WireDoc::Src(off.0, off.1),
            WireDoc::Hard,
            WireDoc::Lit("local m = { 1, 0 }".to_string()),
            WireDoc::Hard,
            WireDoc::Src(on.0, on.1),
            WireDoc::Hard,
            WireDoc::Lit("return m".to_string()),
        ]);

        let reply = FormatReply {
            doc: DOC_VERSION,
            document: Some(document),
            spans: Vec::new(),
            comments: vec![off, on],
        };

        let out = render_format(src, &reply, &FmtConfig::default()).unwrap();

        assert!(
            !out.chars().any(|c| ('\u{E000}'..'\u{F000}').contains(&c)),
            "a mark reached the file: {out:?}"
        );
        assert!(out.contains("local  m = {1,0}"), "{out}");
    }

    /*
    A `src` node that reaches back over a newline is cut at the boundary.

    A worm that lays out markup takes the Luau run before an element as one
    span, and the span starts at the newline that ends the line above. So the
    close of a count region sits inside the node and not at its start. Larvae
    cuts the node there. Without the cut the mark lands further down the
    line, and the splice takes the head of the line away with the region.

    This is the shape the luaux worm sends, and it is the case that found
    this defect.
    */
    #[test]
    fn a_source_node_that_holds_the_boundary_is_cut_at_it() {
        let src = concat!(
            "const a = 1\n",
            "-- larvae: fmt off(1)\n",
            "const root = <F A=\"1\"/>\n",
            "const b = <F B=\"2\"/>\n",
        );

        let off = comment_at(src, "-- larvae: fmt off(1)");
        let held = src.find("const root").unwrap() as u32;
        let after = src.find("\nconst b").unwrap() as u32;

        let document = WireDoc::Concat(vec![
            WireDoc::Src(0, off.0),
            WireDoc::Src(off.0, held),
            WireDoc::Lit("const root = <F".to_string()),
            WireDoc::Indent(Box::new(WireDoc::Concat(vec![
                WireDoc::Hard,
                WireDoc::Lit("A=\"1\"".to_string()),
            ]))),
            WireDoc::Hard,
            WireDoc::Lit("/>".to_string()),
            // the Luau run before the next element, opening on the newline
            WireDoc::Src(after, after + "\nconst b = ".len() as u32),
            WireDoc::Lit("<F".to_string()),
            WireDoc::Indent(Box::new(WireDoc::Concat(vec![
                WireDoc::Hard,
                WireDoc::Lit("B=\"2\"".to_string()),
            ]))),
            WireDoc::Hard,
            WireDoc::Lit("/>".to_string()),
        ]);

        let reply = FormatReply {
            doc: DOC_VERSION,
            document: Some(document),
            spans: Vec::new(),
            comments: vec![off],
        };

        let out = render_format(src, &reply, &FmtConfig::default()).unwrap();

        assert!(
            out.contains("const root = <F A=\"1\"/>"),
            "the region is held on one line: {out}"
        );
        assert!(
            out.contains("const b = <F"),
            "the head of the line past the region survives: {out}"
        );
        assert!(
            out.contains("\tB=\"2\""),
            "the worm still lays out past the region: {out}"
        );
    }

    /// A worm that reports no comment gets the formatter, as before.
    #[test]
    fn a_reply_with_no_comments_formats_as_usual() {
        let src = "<F>\nlocal  m = {1,0}\n</F>\n";
        let start = src.find("local").unwrap() as u32;
        let end = src.find("\n</F>").unwrap() as u32;

        assert!(render(src, vec![(start, end)], Vec::new()).contains("local m = { 1, 0 }"));
    }
}
