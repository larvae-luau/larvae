/*!
The worms larvae knows by a short name.

`larvae worm add luaux` is the whole reason this exists. A worm lives at
`owner/repo`, and the repo of a worm is usually named after the worm with
`-worm` on the end, so the full spelling is longer than the thing it names and
a user has to remember the owner as well.

The list is short and it is written down rather than fetched. A registry over
the network is a service to run and an outage to handle, for a lookup that
saves typing. A user who wants a worm that is not here writes `owner/repo`,
which always works and is what the short name expands to anyway.
*/

/// A short name and the `owner/repo` it means.
const KNOWN: &[(&str, &str)] = &[("luaux", "larvae-luau/luaux-worm")];

/// The repo a short name means, or None when larvae does not know it.
pub fn repo_of(short: &str) -> Option<&'static str> {
    KNOWN
        .iter()
        .find(|(name, _)| *name == short)
        .map(|(_, repo)| *repo)
}

/// Every short name larvae knows, for a message that lists them.
pub fn names() -> Vec<&'static str> {
    KNOWN.iter().map(|(name, _)| *name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_name_expands_to_its_repo() {
        assert_eq!(repo_of("luaux"), Some("larvae-luau/luaux-worm"));
    }

    #[test]
    fn a_name_larvae_does_not_know_expands_to_nothing() {
        assert_eq!(repo_of("nothing-like-this"), None);
    }

    /// A short name with a slash is already a repo, so it must not be one here.
    #[test]
    fn no_short_name_looks_like_a_repo() {
        for name in names() {
            assert!(!name.contains('/'), "{name} would never be looked up");
        }
    }

    /// Every entry has to be a repo, or the fetch builds a URL that cannot resolve.
    #[test]
    fn every_entry_names_an_owner_and_a_repo() {
        for (short, repo) in KNOWN {
            let parts: Vec<&str> = repo.split('/').collect();

            assert_eq!(parts.len(), 2, "{short} -> {repo}");
            assert!(
                parts.iter().all(|p| !p.is_empty()),
                "{short} -> {repo} has an empty half"
            );
        }
    }
}
