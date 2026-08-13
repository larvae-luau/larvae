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

The recognition is narrow by design: only `allow(...)` counts. `-- larvae:
this is load bearing` is a note for the next reader. If larvae treated it as
a flag, larvae would delete it from the build without a message.
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

/// True if this comment speaks to larvae and not to a reader
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

    /// A project that switches over must not have to rewrite its comments
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
