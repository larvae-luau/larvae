/*!
The front-end pre-pass.

A front-end worm turns something we cannot parse into Luau, so it has to finish
before anything that parses Luau starts, including our own require scanning.
That is not an ordering convention we remember to honour, it is what this stage
being separate enforces: a claimed file's bytes are replaced here, and every
later stage receives Luau without ever learning a worm was involved.

It runs serially on purpose. A front-end touches the few files that use its
syntax, not the whole tree, and keeping worms off the parallel loop means no
worm instance is ever shared between threads.
*/

use std::path::{Path, PathBuf};

use crate::diag::Diag;

/*
Hand one claimed file to the worm that claimed it.

Called from inside the per-file work rather than as a pre-pass, so a file the
cache already covers never reaches a worm at all. That matters more than it
sounds: without it every save in watch mode recompiles every markup file.
*/
pub fn compile(
    pool: &crate::worm::pool::Pool,
    index: usize,
    path: &Path,
    src: &str,
    diags: &mut Vec<Diag>,
) -> Option<String> {
    let name = &pool.spec(index).manifest.name;

    let outcome = match pool.compile(index, src) {
        Ok(outcome) => outcome,

        Err(e) => {
            diags.push(Diag::error(path, format!("{e:#}")));

            return None;
        }
    };

    // the worm reported a problem, so this file is out of the build
    if !outcome.ok {
        diags.push(Diag::error(
            path,
            format!("worm `{name}`: {}", outcome.text),
        ));

        return None;
    }

    /*
    Line preservation composes only if every stage keeps it, so a worm changing
    the count is worth saying. It is a warning rather than a refusal: the output
    is still valid Luau, only the line numbers below it stop matching, and that
    is the author's call to make.
    */
    let (before, after) = (src.lines().count(), outcome.text.lines().count());

    if before != after {
        diags.push(Diag::warning(
            path,
            format!(
                "worm `{name}` changed the line count, {before} in and {after} out, so line numbers below this file will not match"
            ),
        ));
    }

    Some(outcome.text)
}

/// `App.luaux` becomes `App.luau`, so the DataModel instance is named `App`
pub fn luau_dest(rel: &Path) -> PathBuf {
    rel.with_extension("luau")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claimed_file_is_renamed_before_anything_reads_it() {
        assert_eq!(luau_dest(Path::new("a/App.luaux")), Path::new("a/App.luau"));
        assert_eq!(luau_dest(Path::new("App.rune")), Path::new("App.luau"));
    }
}
