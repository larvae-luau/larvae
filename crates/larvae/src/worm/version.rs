/*!
What a version in `[worms]` asks for.

Three forms, and the difference is how much the project pins down.

- `"0.1.0"` is that release and no other. A build is the same today and in a
  year, which is what a lockfile gives a language with one.
- `"^0.1.0"` is that release or a later compatible one, by the semver rule.
- `"^"` is the newest release there is.

`larvae worm install` resolves the form against the releases that exist, so
the resolution happens when a user asks for it and not in the middle of a
build. This is why `larvae worm update` no longer exists: a project that wants
the newest says so in the version, and one that does not wants the pin to hold.
*/

use anyhow::{Result, bail};

/// What a written version asks the installer to find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    /// `0.1.0`, and nothing else.
    Exact(String),
    /// `^`, the newest release.
    Newest,
    /// `^0.1.0`, the newest release that semver calls compatible.
    Compatible(semver::VersionReq),
}

impl Wanted {
    /// Reads the version as written in `[worms]`.
    pub fn parse(written: &str) -> Result<Self> {
        let written = written.trim();

        if written == "^" {
            return Ok(Self::Newest);
        }

        if let Some(rest) = written.strip_prefix('^') {
            let req = semver::VersionReq::parse(&format!("^{}", clean(rest)))
                .map_err(|e| anyhow::anyhow!("{written:?} is not a version range, {e}"))?;

            return Ok(Self::Compatible(req));
        }

        if clean(written).is_empty() {
            bail!(
                "a version is empty; write a release, a range such as ^0.1.0, or ^ for the newest"
            );
        }

        Ok(Self::Exact(written.to_string()))
    }

    /// Reports if resolving this needs the list of releases.
    pub fn needs_the_list(&self) -> bool {
        !matches!(self, Self::Exact(_))
    }

    /*
    The release this asks for, out of the ones that exist.

    `available` arrives newest first, as GitHub lists them. A tag that is not
    semver is kept for an exact match and passed over by a range, because a
    range has no way to order it.
    */
    pub fn pick<'a>(&self, available: &[&'a str]) -> Option<&'a str> {
        match self {
            Self::Exact(want) => available
                .iter()
                .copied()
                .find(|tag| clean(tag) == clean(want)),

            Self::Newest => available
                .iter()
                .copied()
                .filter(|tag| semver::Version::parse(clean(tag)).is_ok())
                .max_by_key(|tag| semver::Version::parse(clean(tag)).expect("filtered")),

            Self::Compatible(req) => available
                .iter()
                .copied()
                .filter_map(|tag| Some((semver::Version::parse(clean(tag)).ok()?, tag)))
                .filter(|(version, _)| req.matches(version))
                .max_by(|a, b| a.0.cmp(&b.0))
                .map(|(_, tag)| tag),
        }
    }
}

/// A tag as semver reads it. Releases are tagged `v0.1.0` as often as `0.1.0`.
pub fn clean(tag: &str) -> &str {
    tag.trim().trim_start_matches('v')
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAGS: &[&str] = &["v0.3.0", "v0.2.1", "v0.2.0", "v0.1.0", "nightly"];

    #[test]
    fn an_exact_version_takes_that_one() {
        let wanted = Wanted::parse("0.2.0").unwrap();

        assert_eq!(wanted, Wanted::Exact("0.2.0".into()));
        assert_eq!(wanted.pick(TAGS), Some("v0.2.0"));
        assert!(!wanted.needs_the_list());
    }

    /// The tag carries a `v` as often as it does not, and both mean the release.
    #[test]
    fn the_v_on_a_tag_does_not_decide_a_match() {
        assert_eq!(Wanted::parse("v0.2.0").unwrap().pick(TAGS), Some("v0.2.0"));
        assert_eq!(Wanted::parse("0.2.0").unwrap().pick(TAGS), Some("v0.2.0"));
    }

    #[test]
    fn a_bare_caret_takes_the_newest() {
        let wanted = Wanted::parse("^").unwrap();

        assert_eq!(wanted, Wanted::Newest);
        assert_eq!(wanted.pick(TAGS), Some("v0.3.0"));
        assert!(wanted.needs_the_list());
    }

    /*
    Below 1.0 semver treats a minor bump as breaking, so `^0.2.0` stays on
    0.2.x. That is the rule cargo follows and the one a user coming from it
    expects.
    */
    #[test]
    fn a_caret_range_stays_inside_what_semver_allows() {
        let wanted = Wanted::parse("^0.2.0").unwrap();

        assert_eq!(wanted.pick(TAGS), Some("v0.2.1"));
    }

    #[test]
    fn a_range_with_nothing_to_match_picks_nothing() {
        assert_eq!(Wanted::parse("^9.0.0").unwrap().pick(TAGS), None);
    }

    /// A tag that is not semver cannot be ordered, so only an exact ask finds it.
    #[test]
    fn a_tag_that_is_not_semver_is_reachable_only_by_name() {
        assert_eq!(
            Wanted::parse("nightly").unwrap().pick(TAGS),
            Some("nightly")
        );
        assert_eq!(Wanted::parse("^").unwrap().pick(TAGS), Some("v0.3.0"));
    }

    #[test]
    fn an_empty_version_is_refused() {
        assert!(Wanted::parse("").is_err());
        assert!(Wanted::parse("^not-a-version").is_err());
    }
}
