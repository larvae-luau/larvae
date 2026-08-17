/*!
Flag comments: the comments that speak to larvae and not to a reader.

```lua
local unused = 1 -- larvae: allow(unused_variable)
```

One vocabulary, two readers. The linter reads them to learn what an author
already accepts here. `larvae process` reads them to learn which comments are
instructions to a tool and not notes to a person, so it can remove them from
the output. The recognition lives in one place, and this stops the two
readers from a drift into different definitions of a flag.

larvae accepts the selene spelling beside its own. A project that switches
over already has these comments in many files, and no user must rewrite each
one by hand to say the same thing.

The recognition is narrow by design. `-- larvae: this is load bearing` is a
note for the next reader. If larvae treated it as a flag, larvae would delete
it from the build without a message.

A second family switches a tool off over a span of lines:

```lua
-- larvae: fmt off
local matrix = {
    1, 0, 0,
    0, 1, 0,
}
-- larvae: fmt on
```

`off` runs to the matching `on`, or to the end of the file when no `on`
follows. So one marker covers three needs. At the top of a file with no `on`
it holds the whole file. Between two markers it holds a region. With a count,
`off(5)`, it holds that many lines below the comment.

`lint` reads the same way as `fmt`. stylua's `ignore start` and `ignore end`
map onto `fmt off` and `fmt on`, for the same reason larvae reads selene's
`allow`.
*/

/// The names that a flag comment can speak to
const PREFIXES: [&str; 2] = ["larvae:", "selene:"];

/// The lints that this comment allows, or None when it is not a flag
pub fn allows(text: &str) -> Option<impl Iterator<Item = &str>> {
    let rest = PREFIXES
        .iter()
        .find_map(|prefix| text.split_once(prefix).map(|(_, rest)| rest))?;

    let inner = rest
        .trim_start()
        .strip_prefix("allow(")?
        .split_once(')')
        .map(|(inner, _)| inner)?;

    Some(inner.split(',').map(str::trim).filter(|n| !n.is_empty()))
}

/// The tool that an `off` or `on` flag speaks to
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Fmt,
    Lint,
}

/// What an `off` or `on` flag asks for
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    /// Hold the tool off until an `on`, or to the end of the file
    Off,
    /// Hold the tool off for this many lines below the comment
    OffLines(u32),
    /// Resume
    On,
}

/*
The switch that this comment asks for, or None when it asks for none.

stylua's spelling maps onto the same two states. A project that switches over
keeps the markers it already wrote in its files.
*/
pub fn switch(text: &str) -> Option<(Subject, Switch)> {
    let rest = PREFIXES
        .iter()
        .chain(["stylua:"].iter())
        .find_map(|prefix| text.split_once(prefix).map(|(_, rest)| rest))?
        .trim();

    // stylua names no subject, because stylua only formats
    if let Some(tail) = rest.strip_prefix("ignore") {
        return match tail.trim() {
            "start" => Some((Subject::Fmt, Switch::Off)),

            "end" => Some((Subject::Fmt, Switch::On)),

            _ => None,
        };
    }

    /*
    `format` says the same thing as `fmt`, and an author reaches for either
    one. A marker that larvae does not know stays in the file as an ordinary
    comment, and the author sees no message and no effect. So larvae reads
    both spellings. `format` comes first, because `fmt` is not a prefix of it
    and the order between them does not matter, but a future longer name
    could collide.
    */
    let (subject, tail) = match () {
        _ if let Some(tail) = rest.strip_prefix("format") => (Subject::Fmt, tail),

        _ if let Some(tail) = rest.strip_prefix("fmt") => (Subject::Fmt, tail),

        _ if let Some(tail) = rest.strip_prefix("lint") => (Subject::Lint, tail),

        _ => return None,
    };

    let tail = tail.trim();

    if tail == "on" {
        return Some((subject, Switch::On));
    }

    let after = tail.strip_prefix("off")?.trim();

    if after.is_empty() {
        return Some((subject, Switch::Off));
    }

    /*
    A count that larvae cannot read is not a flag. `off(five)` then stays in
    the file as an ordinary comment, where a reader sees it, rather than
    silently holding the formatter off to the end of the file.
    */
    let count: u32 = after
        .strip_prefix('(')?
        .strip_suffix(')')?
        .trim()
        .parse()
        .ok()?;

    Some((subject, Switch::OffLines(count)))
}

/*
The byte ranges where `subject` is switched off.

A range covers whole lines, and it holds the marker line itself. Two reasons.
The marker must survive, because a formatter that moved it would move the
boundary it defines. And a reader who asks for a region means the lines they
see, not the byte where a statement happens to start.

An `off` with no matching `on` runs to the end of the file. That is what makes
one marker at the top of a file hold the whole file.
*/
pub fn off_ranges(src: &str, comments: &[(u32, u32)], subject: Subject) -> Vec<(u32, u32)> {
    let lines = LineIndex::new(src);
    let mut out: Vec<(u32, u32)> = Vec::new();
    let mut open: Option<u32> = None;

    for &(start, end) in comments {
        let Some((found, switch)) = switch(&src[start as usize..end as usize]) else {
            continue;
        };

        if found != subject {
            continue;
        }

        match switch {
            Switch::Off if open.is_none() => open = Some(lines.line_start(start)),

            // a second `off` inside a region changes nothing
            Switch::Off => {}

            Switch::OffLines(count) if open.is_none() => {
                let first = lines.line_of(start);
                let from = lines.line_start(start);

                out.push((from, lines.line_end(first + count as usize)));
            }

            Switch::OffLines(_) => {}

            Switch::On => {
                if let Some(from) = open.take() {
                    out.push((from, lines.line_end(lines.line_of(start))));
                }
            }
        }
    }

    // an `off` that never closes holds to the end of the file
    if let Some(from) = open {
        out.push((from, src.len() as u32));
    }

    out.sort_unstable();

    out
}

/// Byte offsets of the start of each line
struct LineIndex {
    starts: Vec<u32>,
    len: u32,
}

impl LineIndex {
    fn new(src: &str) -> Self {
        let mut starts = vec![0u32];

        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i as u32 + 1);
            }
        }

        Self {
            starts,
            len: src.len() as u32,
        }
    }

    fn line_of(&self, byte: u32) -> usize {
        self.starts.partition_point(|&s| s <= byte) - 1
    }

    fn line_start(&self, byte: u32) -> u32 {
        self.starts[self.line_of(byte)]
    }

    /// The byte after the newline that ends this line, or the end of the file
    fn line_end(&self, line: usize) -> u32 {
        match self.starts.get(line + 1) {
            Some(&next) => next,

            None => self.len,
        }
    }
}

/// True if a byte sits inside one of these ranges
pub fn within(ranges: &[(u32, u32)], byte: u32) -> bool {
    ranges.iter().any(|&(a, b)| byte >= a && byte < b)
}

/// True if this comment speaks to larvae and not to a reader
pub fn is_flag(text: &str) -> bool {
    allows(text).is_some() || switch(text).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(text: &str) -> Vec<&str> {
        allows(text).map(Iterator::collect).unwrap_or_default()
    }

    #[test]
    fn a_flag_names_what_it_allows() {
        assert_eq!(
            allowed("-- larvae: allow(unused_variable)"),
            ["unused_variable"]
        );
        assert_eq!(
            allowed("-- larvae: allow(unused_variable, shadowing)"),
            ["unused_variable", "shadowing"]
        );
        assert_eq!(allowed("-- larvae: allow(*)"), ["*"]);
    }

    /// A project that switches over must not have to rewrite its comments
    #[test]
    fn selenes_spelling_is_a_flag_too() {
        assert!(is_flag("-- selene: allow(unused_variable)"));
    }

    // --- the off and on family ---------------------------------------------

    #[test]
    fn a_switch_names_its_tool_and_its_state() {
        assert_eq!(
            switch("-- larvae: fmt off"),
            Some((Subject::Fmt, Switch::Off))
        );
        assert_eq!(
            switch("-- larvae: fmt on"),
            Some((Subject::Fmt, Switch::On))
        );
        assert_eq!(
            switch("-- larvae: lint off"),
            Some((Subject::Lint, Switch::Off))
        );
        assert_eq!(
            switch("-- larvae: lint on"),
            Some((Subject::Lint, Switch::On))
        );
    }

    #[test]
    fn a_count_holds_that_many_lines() {
        assert_eq!(
            switch("-- larvae: fmt off(5)"),
            Some((Subject::Fmt, Switch::OffLines(5)))
        );
        assert_eq!(
            switch("-- larvae: lint off( 12 )"),
            Some((Subject::Lint, Switch::OffLines(12)))
        );
    }

    /// An author reaches for either name, and both mean the formatter
    #[test]
    fn format_says_the_same_as_fmt() {
        assert_eq!(
            switch("-- larvae: format off"),
            Some((Subject::Fmt, Switch::Off))
        );
        assert_eq!(
            switch("-- larvae: format on"),
            Some((Subject::Fmt, Switch::On))
        );
        assert_eq!(
            switch("-- larvae: format off(3)"),
            Some((Subject::Fmt, Switch::OffLines(3)))
        );
    }

    /// stylua only formats, so its spelling names no tool
    #[test]
    fn styluas_markers_map_onto_fmt() {
        assert_eq!(
            switch("-- stylua: ignore start"),
            Some((Subject::Fmt, Switch::Off))
        );
        assert_eq!(
            switch("-- stylua: ignore end"),
            Some((Subject::Fmt, Switch::On))
        );
    }

    /*
    A count that larvae cannot read is not a flag. Were it one, the comment
    would leave the file and hold the formatter off to the end of it, and
    nothing would say so.
    */
    #[test]
    fn a_switch_larvae_cannot_read_is_not_a_flag() {
        assert_eq!(switch("-- larvae: fmt off(five)"), None);
        assert_eq!(switch("-- larvae: fmt maybe"), None);
        assert_eq!(switch("-- larvae: fmt is nice"), None);
        assert_eq!(switch("-- stylua: ignore"), None);
        assert!(!is_flag("-- larvae: fmt off(five)"));
    }

    #[test]
    fn both_families_are_flags() {
        assert!(is_flag("-- larvae: fmt off"));
        assert!(is_flag("-- larvae: lint on"));
        assert!(is_flag("-- stylua: ignore start"));
    }

    // --- the ranges ---------------------------------------------------------

    fn ranges(src: &str, subject: Subject) -> Vec<(u32, u32)> {
        let lexed = crate::syntax::lexer::lex(src).expect("lexes");

        off_ranges(src, &lexed.comments, subject)
    }

    /// One marker at the top with no `on` holds the whole file
    #[test]
    fn an_off_with_no_on_runs_to_the_end() {
        let src = "-- larvae: fmt off
local a = 1
local b = 2
";

        assert_eq!(ranges(src, Subject::Fmt), [(0, src.len() as u32)]);
    }

    #[test]
    fn a_pair_holds_the_lines_between_them_and_the_markers() {
        let src = "local a = 1
-- larvae: fmt off
local b = 2
-- larvae: fmt on
local c = 3
";
        let got = ranges(src, Subject::Fmt);

        assert_eq!(got.len(), 1);

        let (lo, hi) = got[0];

        assert!(src[lo as usize..hi as usize].contains("fmt off"));
        assert!(src[lo as usize..hi as usize].contains("fmt on"));
        assert!(!src[lo as usize..hi as usize].contains("local a"));
        assert!(!src[lo as usize..hi as usize].contains("local c"));
    }

    #[test]
    fn a_count_holds_the_marker_and_that_many_lines_below() {
        let src = "-- larvae: fmt off(1)
local a = 1
local b = 2
";
        let (lo, hi) = ranges(src, Subject::Fmt)[0];
        let held = &src[lo as usize..hi as usize];

        assert!(held.contains("local a"));
        assert!(!held.contains("local b"), "one line only: {held:?}");
    }

    /// Each tool reads its own markers and ignores the markers of the other
    #[test]
    fn a_marker_for_one_tool_does_not_hold_the_other() {
        let src = "-- larvae: lint off
local a = 1
";

        assert!(ranges(src, Subject::Fmt).is_empty());
        assert_eq!(ranges(src, Subject::Lint).len(), 1);
    }

    #[test]
    fn a_second_off_inside_a_region_changes_nothing() {
        let src = "-- larvae: fmt off
local a = 1
-- larvae: fmt off
local b = 2
-- larvae: fmt on
local c = 3
";

        assert_eq!(ranges(src, Subject::Fmt).len(), 1);
    }

    #[test]
    fn a_file_with_no_marker_holds_nothing() {
        assert!(
            ranges(
                "local a = 1
-- a note
",
                Subject::Fmt
            )
            .is_empty()
        );
    }

    #[test]
    fn a_note_to_a_reader_is_not_a_flag() {
        assert!(!is_flag("-- just a note"));
        assert!(!is_flag("-- larvae: this one is load bearing"));
        assert!(!is_flag("-- larvae: allow(unclosed"));
        assert!(!is_flag("--!strict"));
    }
}
