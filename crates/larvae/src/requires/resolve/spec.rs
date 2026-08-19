//! Spec string and filesystem helpers that the resolver shares

use std::path::{Path, PathBuf};

use super::ModuleNode;

// --- helpers -----------------------------------------------------------------

pub(super) fn strip_alias<'s>(spec: &'s str, name: &str) -> Option<&'s str> {
    let body = spec.strip_prefix('@')?;

    if body.eq_ignore_ascii_case(name) {
        return Some("");
    }

    let rest = body.get(..name.len() + 1)?;

    if rest[..name.len()].eq_ignore_ascii_case(name) && rest.ends_with('/') {
        Some(&body[name.len() + 1..])
    } else {
        None
    }
}

pub(super) fn split_alias(body: &str) -> (&str, &str) {
    match body.find('/') {
        Some(i) => (&body[..i], &body[i + 1..]),

        None => (body, ""),
    }
}

pub(super) fn join_specs(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),

        (_, true) => a.to_string(),

        _ => format!("{}/{}", a.trim_end_matches('/'), b),
    }
}

pub(super) fn has_module_extension(spec: &str) -> bool {
    spec.ends_with(".luau") || spec.ends_with(".lua")
}

/// Join a spec onto a base directory and resolve dots; None when it escapes the root
pub(super) fn normalize_join(base: &Path, spec: &str) -> Option<PathBuf> {
    let mut out = base.to_owned();

    for part in spec.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if !out.pop() {
                    return None;
                }
            }

            seg => out.push(seg),
        }
    }

    Some(out)
}

/*
RFC module resolution: a file, or a directory with init.

More than one match is ambiguous, which the RFC makes an error.

`claimed` holds the extensions a worm front-end owns. A claimed file is a
module like any other: the pipeline turns it into Luau in the output, so a
require that names it resolves at runtime, and the resolver has to find it or
it warns about a require that is correct. A project with no worms passes an
empty list and the behaviour is what it always was.
*/
pub(super) fn resolve_module(
    base: &Path,
    claimed: &[String],
) -> Result<Option<ModuleNode>, String> {
    let mut found: Vec<ModuleNode> = Vec::new();

    let builtin = ["luau", "lua"].into_iter().map(str::to_owned);

    for ext in builtin.chain(claimed.iter().cloned()) {
        let candidate = base.with_extension(&ext);

        if candidate.is_file() {
            found.push(ModuleNode::File(candidate));
        }
    }

    if base.is_dir() && (base.join("init.luau").is_file() || base.join("init.lua").is_file()) {
        found.push(ModuleNode::Dir(base.to_owned()));
    }

    match found.len() {
        0 => Ok(None),

        1 => Ok(Some(found.remove(0))),

        _ => Err(format!(
            "ambiguous module at {} (multiple of .luau/.lua/dir-with-init exist; the RFC makes this an error)",
            base.display()
        )),
    }
}

/// A relative path in require syntax, ./x/y or ../../x
pub(super) fn fs_relative(from_dir: &Path, to: &Path) -> String {
    let from: Vec<_> = from_dir.components().collect();
    let to_comps: Vec<_> = to.components().collect();

    let common = from
        .iter()
        .zip(&to_comps)
        .take_while(|(a, b)| a == b)
        .count();
    let ups = from.len() - common;
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_string(), ups).collect();

    parts.extend(
        to_comps[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );

    if ups == 0 {
        format!("./{}", parts.join("/"))
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod claimed_modules {
    use super::*;

    fn tree(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp dir");

        for file in files {
            let path = dir.path().join(file);

            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("the parent exists");
            }

            std::fs::write(&path, "return {}\n").expect("the file writes");
        }

        dir
    }

    /*
    A file a worm claims is a module.

    The pipeline turns it into Luau in the output, so a require that names it
    resolves at runtime. Without this the resolver finds nothing and warns
    about a require that is correct, which is what it did.
    */
    #[test]
    fn a_claimed_extension_resolves_as_a_module() {
        let dir = tree(&["widget.luaux"]);
        let base = dir.path().join("widget");

        let found = resolve_module(&base, &["luaux".to_string()])
            .expect("no ambiguity")
            .expect("the module is found");

        assert!(matches!(found, ModuleNode::File(p) if p.extension().unwrap() == "luaux"));
    }

    /// A project with no worms behaves as it always did.
    #[test]
    fn without_the_claim_the_same_file_is_not_a_module() {
        let dir = tree(&["widget.luaux"]);

        assert!(
            resolve_module(&dir.path().join("widget"), &[])
                .expect("no ambiguity")
                .is_none()
        );
    }

    #[test]
    fn luau_still_resolves_alongside_a_claim() {
        let dir = tree(&["plain.luau"]);

        assert!(
            resolve_module(&dir.path().join("plain"), &["luaux".to_string()])
                .expect("no ambiguity")
                .is_some()
        );
    }

    /*
    Two files of one name is an error, and a claimed one counts.

    `widget.luau` beside `widget.luaux` gives two modules for one require, and
    the RFC makes that an error rather than a guess.
    */
    #[test]
    fn a_claimed_file_beside_a_luau_one_is_ambiguous() {
        let dir = tree(&["widget.luau", "widget.luaux"]);

        let err = resolve_module(&dir.path().join("widget"), &["luaux".to_string()])
            .expect_err("two modules answer one name");

        assert!(err.contains("ambiguous"), "{err}");
    }
}
