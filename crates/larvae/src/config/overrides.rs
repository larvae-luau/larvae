//! Require targets set for each path
//!
//! Most projects want one output form for all files. Mixed projects do not.
//! Client code that runs from a Starter container cannot use absolute `@game`
//! strings the way shared code can. So `[requires.overrides]` lets a glob
//! select a different target for the files that need it.
//!
//! ```toml
//! [requires.overrides]
//! "client/**" = "roblox-instance"
//! "legacy/**" = { target = "roblox-instance", indexing_style = "wait_for_child" }
//! ```
//!
//! Patterns match the path relative to `[process].input`. The first pattern
//! that matches wins, so the order in the file is the priority order.

use anyhow::{Result, bail};
use globset::{Glob, GlobMatcher};

use super::requires::{IndexingStyle, Target};

/// One glob and the target that it forces
#[derive(Debug)]
pub struct Override {
    pattern: GlobMatcher,
    pub target: Target,
    pub style: Option<IndexingStyle>,
}

/// Read the `[requires.overrides]` table in its written order
pub fn parse(table: &toml::Value) -> Result<Vec<Override>> {
    let Some(map) = table.as_table() else {
        bail!("[requires.overrides] must be a table of path patterns");
    };

    let mut out = Vec::new();

    for (pattern, value) in map {
        let glob = Glob::new(pattern)
            .map_err(|e| anyhow::anyhow!("[requires.overrides] \"{pattern}\" is not a glob, {e}"))?
            .compile_matcher();

        let (target, style) = match value {
            // The short form: only a target name.
            toml::Value::String(name) => (target_named(name, pattern)?, None),

            toml::Value::Table(t) => {
                let Some(name) = t.get("target").and_then(|v| v.as_str()) else {
                    bail!("[requires.overrides] \"{pattern}\" needs a target");
                };

                let style = match t.get("indexing_style") {
                    Some(v) => Some(style_named(v.as_str().unwrap_or_default(), pattern)?),

                    None => None,
                };

                (target_named(name, pattern)?, style)
            }

            _ => bail!(
                "[requires.overrides] \"{pattern}\" must be a target name or a table with one"
            ),
        };

        out.push(Override {
            pattern: glob,
            target,
            style,
        });
    }

    Ok(out)
}

impl Override {
    pub fn matches(&self, rel: &std::path::Path) -> bool {
        self.pattern.is_match(rel)
    }
}

/// The first pattern that matches wins, so the order in the file is the priority order
pub fn lookup<'a>(overrides: &'a [Override], rel: &std::path::Path) -> Option<&'a Override> {
    overrides.iter().find(|o| o.matches(rel))
}

fn target_named(name: &str, pattern: &str) -> Result<Target> {
    match name {
        "roblox-string" => Ok(Target::RobloxString),

        "roblox-instance" => Ok(Target::RobloxInstance),

        "path" => Ok(Target::Path),

        other => bail!(
            "[requires.overrides] \"{pattern}\" has target \"{other}\", use roblox-string, roblox-instance or path"
        ),
    }
}

fn style_named(name: &str, pattern: &str) -> Result<IndexingStyle> {
    match name {
        "property" => Ok(IndexingStyle::Property),

        "find_first_child" => Ok(IndexingStyle::FindFirstChild),

        "wait_for_child" => Ok(IndexingStyle::WaitForChild),

        other => bail!(
            "[requires.overrides] \"{pattern}\" has indexing_style \"{other}\", use property, find_first_child or wait_for_child"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parsed(text: &str) -> Vec<Override> {
        parse(&toml::from_str(text).unwrap()).unwrap()
    }

    #[test]
    fn the_short_form_names_a_target() {
        let o = parsed("\"client/**\" = \"roblox-instance\"");
        let hit = lookup(&o, Path::new("client/ui/button.luau")).unwrap();

        assert_eq!(hit.target, Target::RobloxInstance);
        assert!(hit.style.is_none());
    }

    #[test]
    fn the_long_form_carries_a_style() {
        let o = parsed(
            "\"client/**\" = { target = \"roblox-instance\", indexing_style = \"wait_for_child\" }",
        );
        let hit = lookup(&o, Path::new("client/init.client.luau")).unwrap();

        assert_eq!(hit.style, Some(IndexingStyle::WaitForChild));
    }

    #[test]
    fn a_path_nothing_matches_gets_nothing() {
        let o = parsed("\"client/**\" = \"path\"");

        assert!(lookup(&o, Path::new("shared/util.luau")).is_none());
    }

    #[test]
    fn bad_names_say_what_is_allowed() {
        let err = parse(&toml::from_str("\"a/**\" = \"nope\"").unwrap())
            .unwrap_err()
            .to_string();

        assert!(err.contains("roblox-instance"), "{err}");

        let err = parse(
            &toml::from_str("\"a/**\" = { target = \"path\", indexing_style = \"nope\" }").unwrap(),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("wait_for_child"), "{err}");
    }
}
