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

/// RFC module resolution: a file, or a directory with init; more than one match is ambiguous
pub(super) fn resolve_module(base: &Path) -> Result<Option<ModuleNode>, String> {
    let mut found: Vec<ModuleNode> = Vec::new();

    for ext in ["luau", "lua"] {
        let candidate = base.with_extension(ext);

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
