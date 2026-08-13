/*!
Build profiles

`[profile.release]` holds the same keys as the root. A selection with
`--profile release` merges the profile over the base before type checks. The
merge works on the raw toml. So a profile can change any key, and no config
struct needs an optional twin.

Tables merge key by key, so a profile can change one rule and keep the rest.
Arrays and scalars replace in full. If the merge appended to a list, a
profile could not express a replacement.
*/

use anyhow::{Result, bail};

/// Merge a profile over the base config, in place
pub fn apply(base: &mut toml::Value, name: &str) -> Result<()> {
    let Some(table) = base.as_table_mut() else {
        bail!("the config is not a table");
    };

    // The profile block does not stay in the merged config.
    let Some(profiles) = table.remove("profile") else {
        bail!("no [profile.{name}] in this config, there are no profiles at all");
    };

    let Some(found) = profiles.as_table().and_then(|t| t.get(name)) else {
        let known = profiles
            .as_table()
            .map(|t| t.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();

        if known.is_empty() {
            bail!("no [profile.{name}] in this config");
        }

        bail!("no [profile.{name}] in this config, it has {known}");
    };

    merge(base, &found.clone());

    Ok(())
}

/// Deep merge for tables, replacement for all other values
fn merge(base: &mut toml::Value, over: &toml::Value) {
    match (base.as_table_mut(), over.as_table()) {
        (Some(b), Some(o)) => {
            for (key, value) in o {
                match b.get_mut(key) {
                    Some(existing) => merge(existing, value),

                    None => {
                        b.insert(key.clone(), value.clone());
                    }
                }
            }
        }

        _ => *base = over.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merged(text: &str, profile: &str) -> toml::Value {
        let mut v: toml::Value = toml::from_str(text).unwrap();
        apply(&mut v, profile).unwrap();

        v
    }

    #[test]
    fn a_profile_wins_key_by_key() {
        let v = merged(
            concat!(
                "[process]\ninput = \"src\"\noutput = \"dist\"\n",
                "[profile.release]\nprocess = { output = \"build\" }\n",
            ),
            "release",
        );
        let process = v.get("process").unwrap();

        // Only the key that the profile named changed.
        assert_eq!(process.get("output").unwrap().as_str(), Some("build"));
        assert_eq!(process.get("input").unwrap().as_str(), Some("src"));
    }

    #[test]
    fn it_can_add_a_table_the_base_never_had() {
        let v = merged(
            "[process]\ninput = \"src\"\n[profile.release.defines]\nDEBUG = false\n",
            "release",
        );

        assert_eq!(
            v.get("defines").unwrap().get("DEBUG").unwrap().as_bool(),
            Some(false)
        );
    }

    #[test]
    fn arrays_replace_rather_than_append() {
        let v = merged(
            concat!(
                "[process]\nexclude = [\"a\", \"b\"]\n",
                "[profile.release]\nprocess = { exclude = [\"c\"] }\n",
            ),
            "release",
        );
        let list = v.get("process").unwrap().get("exclude").unwrap();

        assert_eq!(list.as_array().unwrap().len(), 1);
    }

    #[test]
    fn the_profile_block_does_not_survive() {
        let v = merged("[process]\ninput = \"src\"\n[profile.release]\n", "release");

        assert!(v.get("profile").is_none());
    }

    #[test]
    fn an_unknown_profile_says_what_there_is() {
        let mut v: toml::Value = toml::from_str("[profile.release]\n[profile.lune]\n").unwrap();
        let err = apply(&mut v, "nope").unwrap_err().to_string();

        assert!(err.contains("lune") && err.contains("release"), "{err}");
    }

    #[test]
    fn a_config_with_no_profiles_says_so() {
        let mut v: toml::Value = toml::from_str("[process]\ninput = \"src\"\n").unwrap();
        let err = apply(&mut v, "release").unwrap_err().to_string();

        assert!(err.contains("no profiles at all"), "{err}");
    }
}
