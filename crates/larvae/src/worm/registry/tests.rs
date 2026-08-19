//! The tests for the registry, kept out of the code they exercise.

use super::*;

mod loading {
    use super::*;

    fn write_worm(root: &Path, dir: &str, manifest: &str, body: &str) {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("worm.toml"), manifest).unwrap();
        std::fs::write(d.join("init.luau"), body).unwrap();
    }

    const ECHO: &str = "return { frontend = { compile = function(s) return s end } }";

    fn config(src: &str) -> WormsConfig {
        WormsConfig::parse(&toml::from_str::<toml::Value>(src).unwrap()).unwrap()
    }

    #[test]
    fn a_local_worm_loads_through_the_registry() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"echo\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".echo\"]\n",
            ECHO,
        );

        let r = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("echo = { path = \"w\" }"),
        )
        .unwrap();

        assert_eq!(r.iter().count(), 1);
    }

    #[test]
    fn a_name_that_disagrees_with_the_key_is_refused() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"actual\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".x\"]\n",
            ECHO,
        );

        let err = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("expected = { path = \"w\" }"),
        )
        .err()
        .unwrap();

        assert!(format!("{err:#}").contains("have to match"), "{err:#}");
    }

    #[test]
    fn two_worms_claiming_one_extension_is_refused() {
        let root = tempfile::tempdir().unwrap();

        for (dir, name) in [("a", "one"), ("b", "two")] {
            write_worm(
                root.path(),
                dir,
                &format!(
                    "name = \"{name}\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".luaux\"]\n"
                ),
                ECHO,
            );
        }

        let err = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("one = { path = \"a\" }\ntwo = { path = \"b\" }"),
        )
        .err()
        .unwrap();

        assert!(format!("{err:#}").contains("both claim .luaux"), "{err:#}");
    }

    /*
    A pin resolves through the cache, so a worm that is already unpacked needs
    no network at all. The tests in fetch.rs cover the fetch itself offline.
    */
    #[test]
    fn a_pinned_worm_is_taken_from_the_cache_when_it_is_there() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join(".larvae");
        let installed = crate::worm::fetch::install_dir(&cache, "echo", "0.1.0");

        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(
            installed.join("worm.toml"),
            "name = \"echo\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".echo\"]\n",
        )
        .unwrap();
        std::fs::write(installed.join("init.luau"), ECHO).unwrap();

        let r = Registry::load(
            root.path(),
            &cache,
            &config("echo = { repo = \"someone/echo\", version = \"0.1.0\" }"),
        )
        .unwrap();

        assert_eq!(r.iter().count(), 1);
    }

    #[test]
    fn an_off_rule_never_reaches_the_worm() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"r\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\nrun_order = 1\n\n[rules.loud]\ndefault = false\n",
            "return { rules = { loud = { visit = function() end } } }",
        );

        let r = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("r = { path = \"w\" }"),
        )
        .unwrap();

        assert!(r.iter().next().unwrap().rules.is_empty());
    }

    #[test]
    fn the_user_switching_a_rule_on_beats_the_default() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"r\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\nrun_order = 1\n\n[rules.loud]\ndefault = false\n",
            "return { rules = { loud = { visit = function() end } } }",
        );

        let r = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("r = { path = \"w\", rules = { loud = true } }"),
        )
        .unwrap();

        assert_eq!(
            r.iter().next().unwrap().rules["loud"],
            toml::Value::Boolean(true)
        );
    }

    /// There are three levels, and the user is the top one
    #[test]
    fn a_users_run_order_overrides_the_worms() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"r\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\nrun_order = 5\n\n[rules.loud]\ndefault = true\n",
            "return { rules = { loud = { visit = function() end } } }",
        );

        let r = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("r = { path = \"w\", run_order = 1 }"),
        )
        .unwrap();

        assert_eq!(r.iter().next().unwrap().run_order, Some(Stage::At(1)));
    }

    #[test]
    fn without_a_user_value_the_worms_own_order_stands() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"r\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\nrun_order = 5\n\n[rules.loud]\ndefault = true\n",
            "return { rules = { loud = { visit = function() end } } }",
        );

        let r = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("r = { path = \"w\" }"),
        )
        .unwrap();

        assert_eq!(
            r.iter().next().unwrap().worm.manifest.run_order,
            Some(Stage::At(5))
        );
    }

    #[test]
    fn a_frontend_is_found_by_the_files_extension() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"echo\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".echo\"]\n",
            ECHO,
        );

        let mut r = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("echo = { path = \"w\" }"),
        )
        .unwrap();

        assert!(r.frontend_for(Path::new("a/b.echo")).is_some());
        assert!(r.frontend_for(Path::new("a/b.luau")).is_none());
    }

    fn options_worm(root: &Path) {
        write_worm(
            root,
            "w",
            "name = \"echo\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".echo\"]\n\n[options.pretty]\ntype = \"boolean\"\ndefault = true\n\n[options.factory]\ntype = \"string\"\nvalues = [\"vide\", \"react\"]\n",
            ECHO,
        );
    }

    #[test]
    fn a_declared_option_takes_its_default_when_the_user_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        options_worm(root.path());

        let r = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("echo = { path = \"w\" }"),
        )
        .unwrap();

        let settings = r.iter().next().unwrap().config.as_table().unwrap();

        assert_eq!(settings["pretty"], toml::Value::Boolean(true));

        // an option with no default stays absent rather than guessing one
        assert!(!settings.contains_key("factory"));
    }

    #[test]
    fn an_option_the_worm_does_not_declare_is_refused() {
        let root = tempfile::tempdir().unwrap();
        options_worm(root.path());

        let err = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("echo = { path = \"w\", config = { prety = true } }"),
        )
        .err()
        .unwrap();

        assert!(format!("{err:#}").contains("no option `prety`"), "{err:#}");
    }

    #[test]
    fn an_option_of_the_wrong_type_is_refused() {
        let root = tempfile::tempdir().unwrap();
        options_worm(root.path());

        let err = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("echo = { path = \"w\", config = { pretty = \"yes\" } }"),
        )
        .err()
        .unwrap();

        assert!(format!("{err:#}").contains("takes a boolean"), "{err:#}");
    }

    #[test]
    fn an_option_outside_its_listed_values_is_refused() {
        let root = tempfile::tempdir().unwrap();
        options_worm(root.path());

        let err = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("echo = { path = \"w\", config = { factory = \"solid\" } }"),
        )
        .err()
        .unwrap();

        assert!(format!("{err:#}").contains("takes one of"), "{err:#}");
    }

    fn fmt_worm(root: &Path) {
        write_worm(
            root,
            "w",
            "name = \"echo\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".echo\"]\n\n[fmt.attribute_per_line]\ntype = \"boolean\"\ndefault = false\n",
            ECHO,
        );
    }

    fn loaded(root: &Path) -> Registry {
        Registry::load(
            root,
            &root.join(".larvae"),
            &config("echo = { path = \"w\" }"),
        )
        .unwrap()
    }

    #[test]
    fn a_worm_format_option_sits_in_a_table_under_its_key() {
        let root = tempfile::tempdir().unwrap();
        fmt_worm(root.path());

        let mut cfg: crate::fmt::FmtConfig =
            toml::from_str("column_width = 80\n\n[echo]\nattribute_per_line = true").unwrap();

        loaded(root.path()).resolve_fmt(&mut cfg).unwrap();

        assert_eq!(cfg.column_width, 80);
        assert_eq!(
            cfg.rest["echo"].as_table().unwrap()["attribute_per_line"],
            toml::Value::Boolean(true)
        );
    }

    #[test]
    fn an_option_the_worm_does_not_declare_is_refused_by_name() {
        let root = tempfile::tempdir().unwrap();
        fmt_worm(root.path());

        let mut cfg: crate::fmt::FmtConfig =
            toml::from_str("[echo]\nattribute_per_lien = true").unwrap();

        let err = loaded(root.path()).resolve_fmt(&mut cfg).err().unwrap();

        assert!(
            format!("{err:#}").contains("no format option `attribute_per_lien`"),
            "{err:#}"
        );
    }

    #[test]
    fn a_format_option_takes_its_default_when_the_user_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        fmt_worm(root.path());

        let mut cfg = crate::fmt::FmtConfig::default();
        loaded(root.path()).resolve_fmt(&mut cfg).unwrap();

        assert_eq!(
            cfg.rest["echo"].as_table().unwrap()["attribute_per_line"],
            toml::Value::Boolean(false)
        );
    }

    #[test]
    fn a_format_key_that_belongs_to_nobody_is_refused() {
        let root = tempfile::tempdir().unwrap();
        fmt_worm(root.path());

        let mut cfg: crate::fmt::FmtConfig = toml::from_str("colum_width = 80").unwrap();
        let err = loaded(root.path()).resolve_fmt(&mut cfg).err().unwrap();

        assert!(format!("{err:#}").contains("colum_width"), "{err:#}");
    }

    #[test]
    fn a_worm_lint_named_like_a_builtin_is_refused() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"echo\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".echo\"]\n\n[lints.unused_variable]\n",
            ECHO,
        );

        let err = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("echo = { path = \"w\" }"),
        )
        .err()
        .unwrap();

        assert!(format!("{err:#}").contains("builtin"), "{err:#}");
    }

    #[test]
    fn two_worms_declaring_one_lint_is_refused() {
        let root = tempfile::tempdir().unwrap();

        for (dir, name, ext) in [("a", "one", ".aa"), ("b", "two", ".bb")] {
            write_worm(
                root.path(),
                dir,
                &format!(
                    "name = \"{name}\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\"{ext}\"]\n\n[lints.tidy]\n"
                ),
                ECHO,
            );
        }

        let err = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("one = { path = \"a\" }\ntwo = { path = \"b\" }"),
        )
        .err()
        .unwrap();

        assert!(
            format!("{err:#}").contains("both declare lint `tidy`"),
            "{err:#}"
        );
    }
}
