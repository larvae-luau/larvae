/*!
Flag comments, the ones addressed to larvae rather than to a reader.

```lua
local unused = 1 -- larvae: allow(unused_variable)
```

One vocabulary, two readers. The linter reads them to know what an author
already decided is fine here, and `larvae process` reads them to know which
comments are instructions to a tool rather than notes to a person, so it can
leave them out of the output. Keeping the recognition in one place is what
stops those two from drifting into disagreeing about what a flag is.

selene's spelling is accepted alongside ours, because a project switching over
has these scattered through it already and rewriting every one by hand to say
the same thing is not a migration anyone should have to do.

Deliberately narrow: only `allow(...)` counts. `-- larvae: this is load bearing`
is a note somebody left for the next reader, and treating it as a flag would
mean quietly deleting it from the build.
*/

/// The names a flag comment can be addressed to
const PREFIXES: [&str; 2] = ["larvae:", "selene:"];

/// The lints this comment allows, or None when it is not a flag at all
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

/// Whether this comment is talking to larvae rather than to a reader
pub fn is_flag(text: &str) -> bool {
    allows(text).is_some()
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

    /// A project switching over should not have to rewrite its comments
    #[test]
    fn selenes_spelling_is_a_flag_too() {
        assert!(is_flag("-- selene: allow(unused_variable)"));
    }

    #[test]
    fn a_note_to_a_reader_is_not_a_flag() {
        assert!(!is_flag("-- just a note"));
        assert!(!is_flag("-- larvae: this one is load bearing"));
        assert!(!is_flag("-- larvae: allow(unclosed"));
        assert!(!is_flag("--!strict"));
    }
}
