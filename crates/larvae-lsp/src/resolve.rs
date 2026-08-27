/*!
How the analyzer answers a require, in pure path terms.

The logic lives apart from the FFI on purpose. Resolution is path work and
needs no C++, so a machine with no Luau checkout still compiles it and runs
its tests. The analyzer module sits behind the `analyzer` feature, and a
rule this important should not be testable only where that feature builds.
*/

/*
The analyzer is the only caller, and it sits behind a feature. The module
does not, so the tests below run on a machine with no Luau checkout. That
is the reason the logic was split out, so the unused warning is expected
where the feature is off.
*/
#![cfg_attr(not(feature = "analyzer"), allow(dead_code))]

use std::path::{Path, PathBuf};

/*
One require spec against the file that wrote it.

An init file resolves `./` from the directory above its own, the same
rule the pipeline's resolver holds. A directory answers its init file,
so the frontend always receives a file it can load.
*/
pub fn resolve_spec(
    from: &Path,
    spec: &str,
    mounts: Option<&larvae::requires::datamodel::MountTable>,
    claimed: &[String],
) -> Option<PathBuf> {
    let is_init = from
        .file_stem()
        .is_some_and(|s| s == "init" || s == "init.server" || s == "init.client");

    let own_dir = from.parent()?;

    let dot_base = match is_init {
        true => own_dir.parent().unwrap_or(own_dir),

        false => own_dir,
    };

    /*
    `@game` answers before the alias lookup, and for every file.

    The spec is absolute: it names a place in the DataModel and reads
    nothing from the file that writes it. Larvae used to send it through the
    alias branch below, where it resolved only if a `.luaurc` happened to
    define a name called `game`. So a file the sourcemap did not cover got
    no type information from a require that the build resolves correctly,
    and the instance form `game.ReplicatedStorage.Thing` worked in the same
    file. Two spellings of one thing behaved differently.

    A `.luaurc` that defines `game` still wins. That file is the project
    speaking about its own names, and larvae does not overrule it.
    */
    if spec.starts_with("@game/") && lookup_alias(own_dir, "game").is_none() {
        let segments = larvae::requires::datamodel::parse_game_path(spec)?;
        let base = mounts?.fs_of(&segments)?;

        return as_module_file(&base, claimed);
    }

    let joined = if let Some(rest) = spec.strip_prefix("@self/") {
        own_dir.join(rest)
    } else if spec.starts_with("./") || spec.starts_with("../") {
        dot_base.join(spec)
    } else {
        // Anything left has to be an alias, and a bare word is not a spec.
        let rest = spec.strip_prefix('@')?;

        let (alias, tail) = match rest.split_once('/') {
            Some((a, t)) => (a, Some(t)),

            None => (rest, None),
        };

        let base = lookup_alias(own_dir, alias)?;

        match tail {
            Some(tail) => base.join(tail),

            None => base,
        }
    };

    as_module_file(&joined, claimed)
}

/// The aliases of the nearest .luaurc walking up from the requiring file
fn lookup_alias(from_dir: &Path, alias: &str) -> Option<PathBuf> {
    let want = alias.to_lowercase();

    for dir in from_dir.ancestors() {
        let path = dir.join(".luaurc");

        if !path.exists() {
            continue;
        }

        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };

        let Ok(parsed) = json5::from_str::<serde_json::Value>(&text) else {
            continue;
        };

        if let Some(aliases) = parsed.get("aliases").and_then(|a| a.as_object()) {
            for (name, value) in aliases {
                if name.to_lowercase() == want
                    && let Some(target) = value.as_str()
                {
                    return Some(dir.join(target));
                }
            }
        }
    }

    None
}

/*
A path as the module file the frontend loads: itself, or its init file.

A file the frontend cannot read is not a module. Luau is a module, and so is
a file whose extension a resolving worm claims, because that worm hands the
frontend a lowering of it. Everything else answers nothing, and Luau reports
an unsupported path, which is what a require of a text file deserves.

`claimed` carries the extensions of those worms, without the dot. It is
empty for a project with no such worm, and then Luau alone is a module.
*/
fn as_module_file(path: &Path, claimed: &[String]) -> Option<PathBuf> {
    let loadable = |path: &Path| {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| ext == "luau" || ext == "lua" || claimed.iter().any(|c| c == ext))
    };

    // The spec spelled the extension itself, ex: `./config.json`.
    if path.is_file() && loadable(path) {
        return Some(path.to_path_buf());
    }

    /*
    Luau comes first, then the claimed extensions in the order the project
    installed their worms. A `data.luau` beside a `data.json` is the module
    that `./data` names, which is the order the build resolves them in too.
    */
    for ext in ["luau", "lua"]
        .iter()
        .map(|e| (*e).to_owned())
        .chain(claimed.iter().cloned())
    {
        let with = path.with_extension(&ext);

        if with.is_file() {
            return Some(with);
        }
    }

    if path.is_dir() {
        for name in ["init.luau", "init.lua"] {
            let init = path.join(name);

            if init.is_file() {
                return Some(init);
            }
        }
    }

    None
}

#[cfg(test)]
mod game_requires {
    use super::*;
    use larvae::requires::datamodel::{Mount, MountTable};

    fn tree(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp dir");

        for file in files {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes it");
            std::fs::write(&path, "return {}\n").expect("writes");
        }

        dir
    }

    fn mounts(root: &Path) -> MountTable {
        MountTable::new(vec![Mount {
            fs: root.join("src/Shared"),
            dm: vec!["ReplicatedStorage".into(), "App".into()],
        }])
    }

    /*
    `@game` resolves from a file the DataModel map does not cover.

    The spec is absolute, so it reads nothing from the requirer. Larvae used
    to send it through the `.luaurc` alias branch, where it resolved only if
    the project happened to define a name called `game`. A file outside the
    mounted tree then lost the type information that the build gives it.
    */
    #[test]
    fn a_file_outside_the_map_resolves_game() {
        let dir = tree(&["src/Shared/Widget.luau", "tools/build.luau"]);
        let table = mounts(dir.path());

        let found = resolve_spec(
            &dir.path().join("tools/build.luau"),
            "@game/ReplicatedStorage/App/Widget",
            Some(&table),
            &[],
        );

        assert_eq!(found, Some(dir.path().join("src/Shared/Widget.luau")));
    }

    /// A file the map does cover resolves the same way, which it always did.
    #[test]
    fn a_file_inside_the_map_still_resolves_game() {
        let dir = tree(&["src/Shared/Widget.luau", "src/Shared/Other.luau"]);
        let table = mounts(dir.path());

        let found = resolve_spec(
            &dir.path().join("src/Shared/Other.luau"),
            "@game/ReplicatedStorage/App/Widget",
            Some(&table),
            &[],
        );

        assert_eq!(found, Some(dir.path().join("src/Shared/Widget.luau")));
    }

    /// A directory answers its init file, so the frontend receives a file.
    #[test]
    fn a_game_path_that_names_a_directory_answers_its_init() {
        let dir = tree(&["src/Shared/Pkg/init.luau", "tools/build.luau"]);
        let table = mounts(dir.path());

        let found = resolve_spec(
            &dir.path().join("tools/build.luau"),
            "@game/ReplicatedStorage/App/Pkg",
            Some(&table),
            &[],
        );

        assert_eq!(found, Some(dir.path().join("src/Shared/Pkg/init.luau")));
    }

    /*
    A `.luaurc` that defines `game` wins.

    That file is the project speaking about its own names, and larvae does
    not overrule it. This is the one case the built-in meaning steps aside.
    */
    #[test]
    fn a_luaurc_alias_named_game_still_wins() {
        let dir = tree(&["src/Shared/Widget.luau", "custom/Widget.luau"]);

        std::fs::write(
            dir.path().join(".luaurc"),
            "{ \"aliases\": { \"game\": \"custom\" } }",
        )
        .expect("writes");

        let table = mounts(dir.path());

        let found = resolve_spec(
            &dir.path().join("tools/build.luau"),
            "@game/Widget",
            Some(&table),
            &[],
        );

        assert_eq!(found, Some(dir.path().join("custom/Widget.luau")));
    }

    /// With no map, `@game` resolves to nothing, which is the true answer.
    #[test]
    fn without_a_map_game_resolves_to_nothing() {
        let dir = tree(&["src/Shared/Widget.luau", "tools/build.luau"]);

        assert_eq!(
            resolve_spec(
                &dir.path().join("tools/build.luau"),
                "@game/ReplicatedStorage/App/Widget",
                None,
                &[]
            ),
            None
        );
    }

    /// The other spellings are untouched, and they need the filesystem base.
    #[test]
    fn the_relative_forms_still_resolve() {
        let dir = tree(&["tools/build.luau", "tools/helper.luau"]);
        let table = mounts(dir.path());
        let from = dir.path().join("tools/build.luau");

        assert_eq!(
            resolve_spec(&from, "./helper", Some(&table), &[]),
            Some(dir.path().join("tools/helper.luau"))
        );
        assert_eq!(
            resolve_spec(&from, "@self/helper", Some(&table), &[]),
            Some(dir.path().join("tools/helper.luau"))
        );
    }
}

#[cfg(test)]
mod claimed_files {
    use super::*;

    fn tree(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp dir");

        for file in files {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes it");
            std::fs::write(&path, "{}\n").expect("writes");
        }

        dir
    }

    /*
    A data file a worm claims is a module.

    The worm hands the analyzer a lowering of it, so the require has a type.
    Without the claim there is nothing to read the file with, and Luau says
    so rather than reading JSON as Luau and reporting the first brace.
    */
    #[test]
    fn a_claimed_file_resolves_and_an_unclaimed_one_does_not() {
        let dir = tree(&["src/a.luau", "src/data.json", "src/notes.txt"]);
        let from = dir.path().join("src/a.luau");
        let claims = ["json".to_owned()];

        assert_eq!(
            resolve_spec(&from, "./data", None, &claims),
            Some(dir.path().join("src/data.json"))
        );
        assert_eq!(
            resolve_spec(&from, "./data.json", None, &claims),
            Some(dir.path().join("src/data.json"))
        );

        // No worm claims it, so nothing does, whichever way it is spelled.
        assert_eq!(resolve_spec(&from, "./data", None, &[]), None);
        assert_eq!(resolve_spec(&from, "./notes.txt", None, &claims), None);
    }

    /// Luau wins over a claimed file of the same stem, as it does in a build.
    #[test]
    fn luau_wins_the_stem() {
        let dir = tree(&["src/a.luau", "src/data.luau", "src/data.json"]);
        let from = dir.path().join("src/a.luau");

        assert_eq!(
            resolve_spec(&from, "./data", None, &["json".to_owned()]),
            Some(dir.path().join("src/data.luau"))
        );
    }
}

#[cfg(test)]
mod huskfall {
    use super::*;
    use std::path::Path;

    /// A plain relative require in a real project has to resolve.
    #[test]
    fn a_relative_require_in_a_package_resolves() {
        let from = Path::new(
            "/mnt/new_volume/Programming/Luau/Roblox/Huskfall/packages/roblox/.ember/ffrostfall_fluid/src/anim/lerp.luau",
        );

        if !from.is_file() {
            return; // the project is not on this machine
        }

        assert!(
            resolve_spec(from, "../reactive/graph", None, &[]).is_some(),
            "the resolver did not find ../reactive/graph"
        );
    }
}
