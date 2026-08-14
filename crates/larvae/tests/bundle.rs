/*!
Bundling, checked by running the bundle.

Most tests here execute the output, not only read it. A text assertion
proves that the bundler produced what was expected, and not that what was
expected works. larvae already embeds a Luau VM for worms, so a bundle can
simply run, and that is the only test that can catch a runtime shape being
wrong: a registry that never populates, a loader that recurses, a module
that returns the wrong thing.
*/

use std::path::Path;
use std::process::Command;

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_larvae"))
}

fn write(root: &Path, name: &str, body: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// A project with `files` under `src`, bundled with `args` extra
fn project(files: &[(&str, &str)], config: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();

    write(root, "larvae.toml", config);

    for (name, body) in files {
        write(root, &format!("src/{name}"), body);
    }

    dir
}

fn config_with(bundle: &str) -> String {
    format!(
        "[process]\ninput = \"src\"\n\n[requires.mounts]\n\"src\" = \"@game/ReplicatedStorage/app\"\n\n{bundle}"
    )
}

fn run_bundle(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(bin())
        .arg("bundle")
        .args(args)
        .current_dir(root)
        .output()
        .expect("runs");

    if !output.status.success() {
        return Err(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(std::fs::read_to_string(root.join("out.luau")).expect("the bundle was written"))
}

/// A project with `files` under `src`, bundled from `entry`
fn bundle(files: &[(&str, &str)], entry: &str) -> Result<String, String> {
    let dir = project(
        files,
        &config_with(&format!(
            "[bundle]\nentry = \"{entry}\"\noutput = \"out.luau\"\n"
        )),
    );

    run_bundle(dir.path(), &[])
}

/// Execute a bundle and return what its entry returned, as a string
fn run_luau(source: &str) -> Result<String, String> {
    let lua = mlua::Lua::new();

    lua.load(source)
        .eval::<mlua::Value>()
        .map(|v| match v {
            mlua::Value::String(s) => s.to_string_lossy().to_string(),

            other => format!("{other:?}"),
        })
        .map_err(|e| e.to_string())
}

#[test]
fn a_bundled_project_runs_and_returns_what_the_entry_returns() {
    let text = bundle(
        &[
            (
                "main.luau",
                "local g = require(\"./greet\")\nreturn g.hello(\"world\")\n",
            ),
            (
                "greet.luau",
                "return { hello = function(n) return \"hi \" .. n end }\n",
            ),
        ],
        "src/main.luau",
    )
    .expect("bundles");

    assert_eq!(run_luau(&text).expect("runs"), "hi world");
}

#[test]
fn a_chain_of_modules_all_load() {
    let text = bundle(
        &[
            ("main.luau", "return require(\"./a\")\n"),
            ("a.luau", "return require(\"./b\") .. \"-a\"\n"),
            ("b.luau", "return \"b\"\n"),
        ],
        "src/main.luau",
    )
    .expect("bundles");

    assert_eq!(run_luau(&text).expect("runs"), "b-a");
}

/// One module required from two places must initialise once, not twice
#[test]
fn a_shared_module_is_initialised_exactly_once() {
    let text = bundle(
        &[
            (
                "main.luau",
                "require(\"./a\")\nrequire(\"./b\")\nreturn require(\"./tracker\").get()\n",
            ),
            ("a.luau", "require(\"./once\")\nreturn {}\n"),
            ("b.luau", "require(\"./once\")\nreturn {}\n"),
            // The side effect sits at module level, so it happens per initialisation.
            ("once.luau", "require(\"./tracker\").bump()\nreturn {}\n"),
            (
                "tracker.luau",
                "local n = 0\nreturn { bump = function() n = n + 1 end, get = function() return tostring(n) end }\n",
            ),
        ],
        "src/main.luau",
    )
    .expect("bundles");

    assert_eq!(
        run_luau(&text).expect("runs"),
        "1",
        "the shared module ran more than once"
    );
}

/// A module that returns nil is not the same as one that has not run
#[test]
fn a_module_returning_nil_is_still_cached() {
    let text = bundle(
        &[
            (
                "main.luau",
                "require(\"./side\")\nrequire(\"./side\")\nreturn require(\"./side_count\").n()\n",
            ),
            ("side.luau", "require(\"./side_count\").bump()\nreturn nil\n"),
            (
                "side_count.luau",
                "local c = 0\nreturn { bump = function() c = c + 1 end, n = function() return tostring(c) end }\n",
            ),
        ],
        "src/main.luau",
    )
    .expect("bundles");

    assert_eq!(run_luau(&text).expect("runs"), "1", "it ran more than once");
}

/// Matches unbundled Roblox, which raises on a recursive require
#[test]
fn a_load_time_cycle_errors_naming_the_module() {
    let text = bundle(
        &[
            ("main.luau", "return require(\"./a\")\n"),
            ("a.luau", "return require(\"./b\")\n"),
            ("b.luau", "return require(\"./a\")\n"),
        ],
        "src/main.luau",
    )
    .expect("bundles");

    let err = run_luau(&text).expect_err("a load time cycle cannot work");

    assert!(err.contains("cyclic require"), "{err}");
    assert!(err.contains("src/a"), "{err}");
}

/*
The pattern that looks cyclic and is not.

A require inside a function defers past load, so both modules finish and the
call works. This is the common shape in Roblox code, and the reason `check`
reports cycles as a warning and not an error.
*/
#[test]
fn a_cycle_deferred_into_a_function_still_works() {
    let text = bundle(
        &[
            ("main.luau", "return require(\"./a\").ask()\n"),
            (
                "a.luau",
                "local m = {}\nfunction m.ask() return require(\"./b\").answer() end\nfunction m.name() return \"a\" end\nreturn m\n",
            ),
            (
                "b.luau",
                "local m = {}\nfunction m.answer() return require(\"./a\").name() .. \"-b\" end\nreturn m\n",
            ),
        ],
        "src/main.luau",
    )
    .expect("bundles");

    assert_eq!(run_luau(&text).expect("runs"), "a-b");
}

#[test]
fn an_unreachable_module_is_left_out_of_the_bundle() {
    let text = bundle(
        &[
            ("main.luau", "return \"only me\"\n"),
            ("orphan.luau", "return error(\"this must never run\")\n"),
        ],
        "src/main.luau",
    )
    .expect("bundles");

    assert!(!text.contains("must never run"), "the orphan was bundled");
    assert_eq!(run_luau(&text).expect("runs"), "only me");
}

/*
The registry finds a module by id at run time, so a module that only a
dynamic require reaches works when the bundle keeps it. `tree_shake = false`
is how a project keeps it.
*/
#[test]
fn tree_shaking_off_keeps_the_unreachable_module() {
    let dir = project(
        &[
            ("main.luau", "return \"still me\"\n"),
            ("kept.luau", "return \"kept text marker\"\n"),
        ],
        &config_with(
            "[bundle]\nentry = \"src/main.luau\"\noutput = \"out.luau\"\ntree_shake = false\n",
        ),
    );

    let text = run_bundle(dir.path(), &[]).expect("bundles");

    assert!(text.contains("kept text marker"), "{text}");
    assert_eq!(run_luau(&text).expect("runs"), "still me");
}

/// A require of a directory resolves to its init file, on one node
#[test]
fn a_directory_module_initialises_through_its_init_file() {
    let text = bundle(
        &[
            ("main.luau", "return require(\"./pkg\")\n"),
            (
                "pkg/init.luau",
                "return require(\"./pkg/helper\") .. \" from pkg\"\n",
            ),
            ("pkg/helper.luau", "return \"hi\"\n"),
        ],
        "src/main.luau",
    )
    .expect("bundles");

    assert_eq!(run_luau(&text).expect("runs"), "hi from pkg");
}

/// The command line beats the config, like every other command
#[test]
fn the_entry_flag_beats_the_configured_entry() {
    let dir = project(
        &[
            ("main.luau", "return \"from the config entry\"\n"),
            ("other.luau", "return \"from the flag entry\"\n"),
        ],
        &config_with("[bundle]\nentry = \"src/main.luau\"\noutput = \"out.luau\"\n"),
    );

    let text = run_bundle(dir.path(), &["--entry", "src/other.luau"]).expect("bundles");

    assert_eq!(run_luau(&text).expect("runs"), "from the flag entry");
}

/// Requiring one module twice from one file must rewrite both calls
#[test]
fn the_same_module_required_twice_in_a_file_works() {
    let text = bundle(
        &[
            (
                "main.luau",
                "local a = require(\"./v\")\nlocal b = require(\"./v\")\nreturn a.s .. b.s\n",
            ),
            ("v.luau", "return { s = \"x\" }\n"),
        ],
        "src/main.luau",
    )
    .expect("bundles");

    assert!(
        !text.contains("require(\"./v\")"),
        "a call was left unrewritten"
    );
    assert_eq!(run_luau(&text).expect("runs"), "xx");
}

#[test]
fn the_bundle_is_byte_identical_across_runs() {
    let files = [
        ("main.luau", "return require(\"./a\")\n"),
        ("a.luau", "return require(\"./b\")\n"),
        ("b.luau", "return \"b\"\n"),
    ];

    let once = bundle(&files, "src/main.luau").expect("bundles");
    let twice = bundle(&files, "src/main.luau").expect("bundles");

    assert_eq!(once, twice);
}

#[test]
fn a_missing_entry_is_reported_rather_than_producing_an_empty_bundle() {
    let err = bundle(&[("main.luau", "return 1\n")], "src/nope.luau")
        .expect_err("there is no such entry");

    assert!(err.contains("nope.luau"), "{err}");
}

/*
A worm-claimed file must bundle as the Luau its worm compiles, and not as
the raw claimed source.

The front-end runs inside the pipeline, so the file on disk holds markup the
whole time. A bundler that reads sources from disk ships that markup. The
worm is a real native worm in a few lines of python, like the one in
worm_fmt_lint.rs, so the test needs unix.
*/
#[cfg(unix)]
mod worm_frontend {
    use super::*;

    fn install_worm(root: &Path) {
        write(
            root,
            "mywormdir/worm.toml",
            r#"
name  = "markup"
api   = 1
form  = "native"
entry = "worm.py"

[frontend]
claims = [".luaux"]
"#,
        );

        write(
            root,
            "mywormdir/worm.py",
            r#"#!/usr/bin/env python3
import sys, json, struct

def read():
    n = sys.stdin.buffer.read(4)
    if len(n) < 4: sys.exit(0)
    return json.loads(sys.stdin.buffer.read(struct.unpack("<I", n)[0]))

def send(obj):
    b = json.dumps(obj).encode()
    sys.stdout.buffer.write(struct.pack("<I", len(b)) + b)
    sys.stdout.buffer.flush()

while True:
    req = read()
    if req["op"] == "init":
        send({"ok": True})
    elif req["op"] == "transform":
        compiled = req["source"].replace("<greet/>", '"markup greeting"')
        send({"ok": True, "output": compiled})
    else:
        send({"ok": True})
"#,
        );

        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(
                root.join("mywormdir/worm.py"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
    }

    #[test]
    fn a_claimed_file_bundles_as_the_compiled_luau() {
        let dir = project(
            &[
                (
                    "main.luaux",
                    "local msg = <greet/>\nreturn require(\"./join\").with(msg)\n",
                ),
                (
                    "join.luau",
                    "return { with = function(m) return \"compiled \" .. m end }\n",
                ),
            ],
            &format!(
                "{}\n[worms.markup]\npath = \"mywormdir\"\n",
                config_with("[bundle]\nentry = \"src/main.luaux\"\noutput = \"out.luau\"\n")
            ),
        );

        install_worm(dir.path());

        let text = run_bundle(dir.path(), &[]).expect("bundles");

        assert!(
            !text.contains("<greet/>"),
            "the raw claimed source was bundled: {text}"
        );
        assert!(text.contains("markup greeting"), "{text}");
        assert_eq!(
            run_luau(&text).expect("runs"),
            "compiled markup greeting",
            "the compiled module must run inside the bundle"
        );
    }
}
