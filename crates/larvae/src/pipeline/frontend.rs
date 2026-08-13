/*!
The front-end pre-pass.

A front-end worm turns text that larvae cannot parse into Luau. So the worm
must finish before any stage that parses Luau starts, and that includes the
require scan of larvae. This order is not a convention that developers must
remember. The separate stage enforces it: this stage replaces the bytes of a
claimed file, and every later stage receives Luau and never learns that a
worm ran.

The stage runs serially by design. A front-end touches the few files that use
its syntax and not the whole tree. Worms stay off the parallel loop, so no
worm instance is shared between threads.
*/

use std::path::{Path, PathBuf};

use crate::diag::Diag;

/*
Give one claimed file to the worm that claimed it.

The pipeline calls this inside the work for each file and not as a pre-pass.
So a file that the cache covers never reaches a worm. This is important:
without it, every save in watch mode compiles every markup file again.
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

    // The worm reported a problem, so this file is out of the build.
    if !outcome.ok {
        diags.push(Diag::error(
            path,
            format!("worm `{name}`: {}", outcome.text),
        ));

        return None;
    }

    /*
    Line preservation holds only if every stage keeps it. So larvae reports a
    worm that changes the count. This is a warning and not a refusal: the
    output is still valid Luau. Only the line numbers below it stop matching,
    and the author makes that decision.
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
