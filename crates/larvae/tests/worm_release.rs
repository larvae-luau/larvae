//! A native worm that arrives as a release zip has to be runnable.

#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;

/// The zip a worm author uploads: the manifest and the executable, at the root
fn zip_of(entry: &str, script: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let plain: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();

        w.start_file("worm.toml", plain).unwrap();
        write!(
            w,
            "name = \"demo\"\napi = 1\nform = \"native\"\nentry = \"{entry}\"\n\n[frontend]\nclaims = [\".demo\"]\n"
        )
        .unwrap();

        // No mode is set here on purpose: a zip from CI often carries none.
        w.start_file(entry, plain).unwrap();
        write!(w, "{script}").unwrap();
        w.finish().unwrap();
    }

    buf
}

/*
larvae sets the executable bit when it unpacks a native worm.

A zip does not reliably carry the mode of a file inside it, and a worm that
the operating system refuses to run is a worse failure than a file that is
executable and never run.
*/
#[test]
fn a_native_worm_from_a_zip_is_runnable() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = zip_of("demo-worm", "#!/bin/sh\nexit 0\n");

    let target = dir.path().join("unpacked");
    std::fs::create_dir_all(&target).unwrap();

    zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .unwrap()
        .extract(&target)
        .unwrap();

    let entry = target.join("demo-worm");
    let before = std::fs::metadata(&entry).unwrap().permissions().mode();

    // the state larvae has to repair: readable, and not runnable
    std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(before & 0o111, before & 0o111);

    larvae::worm::fetch::make_runnable(&target).expect("larvae sets the bit");

    let after = std::fs::metadata(&entry).unwrap().permissions().mode();

    assert!(
        after & 0o111 != 0,
        "the entry is executable, mode {after:o}"
    );
}

/*
A zip that wraps its contents in one directory still installs.

`zip -r name.zip name/` does this by default, so most release zips arrive this
way. larvae lifts the contents rather than refuse the worm.
*/
#[test]
fn a_single_wrapping_directory_is_lifted() {
    let dir = tempfile::tempdir().unwrap();
    let inner = dir.path().join("luaux-worm-x86_64-linux");

    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(
        inner.join("worm.toml"),
        "name = \"demo\"\napi = 1\nform = \"native\"\nentry = \"demo-worm\"\n\n[frontend]\nclaims = [\".demo\"]\n",
    )
    .unwrap();
    std::fs::write(inner.join("demo-worm"), "#!/bin/sh\nexit 0\n").unwrap();

    larvae::worm::fetch::flatten_wrapper(dir.path()).expect("lifts the wrapper");

    assert!(
        dir.path().join("worm.toml").is_file(),
        "the manifest is at the root"
    );
    assert!(
        dir.path().join("demo-worm").is_file(),
        "the entry came with it"
    );
    assert!(!inner.exists(), "the empty wrapper is gone");
}

/// Two entries at the root are left alone, because neither is clearly the root
#[test]
fn more_than_one_root_entry_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();

    for name in ["one", "two"] {
        let sub = dir.path().join(name);
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("worm.toml"), "name = \"x\"\n").unwrap();
    }

    larvae::worm::fetch::flatten_wrapper(dir.path()).expect("does nothing");

    assert!(!dir.path().join("worm.toml").exists());
}

/// A worm of a portable form has no bit to set, and larvae leaves it alone
#[test]
fn a_wasm_worm_needs_no_bit() {
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("worm.toml"),
        "name = \"demo\"\napi = 1\nform = \"wasm\"\nentry = \"demo.wasm\"\n\n[frontend]\nclaims = [\".demo\"]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("demo.wasm"), b"\0asm").unwrap();

    larvae::worm::fetch::make_runnable(dir.path()).expect("nothing to do");
}

/*
The adoption half of the cargo channel, without cargo.

`cargo install` ships one binary and no data files, so larvae asks the binary
for the `worm.toml` it carries and writes the manifest beside it. This test
stands a python script in for the binary, because the pipe protocol is the
contract and the compiler that produced the binary is not.
*/
#[test]
fn a_cargo_built_binary_is_adopted_into_a_worm_dir() {
    let build = tempfile::tempdir().unwrap();
    let bin = build.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();

    let script = bin.join("demo-worm");
    std::fs::write(
        &script,
        r#"#!/usr/bin/env python3
import sys, json, struct

MANIFEST = """name = "demo"
api = 1
form = "native"
entry = "demo-worm"

[frontend]
claims = [".demo"]
"""

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
    if req["op"] == "manifest":
        send({"ok": True, "manifest": MANIFEST})
    else:
        send({"ok": True, "output": req.get("source", "")})
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let cache = tempfile::tempdir().unwrap();
    let dir = cache.path().join("worms/demo/0.1.0");

    larvae::worm::fetch::adopt(&bin, &dir, "demo").expect("adopts the binary");

    // the manifest sits beside the binary, at the entry the manifest names
    assert!(dir.join("worm.toml").is_file());
    assert!(dir.join("demo-worm").is_file());

    // and the adopted directory is a loadable worm
    let worm = larvae::worm::Worm::load(&dir).expect("loads");
    assert_eq!(worm.name(), "demo");
}

/// A binary that does not answer the manifest op cannot install through cargo
#[test]
fn a_binary_without_a_manifest_is_refused() {
    let build = tempfile::tempdir().unwrap();
    let bin = build.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();

    let script = bin.join("mute-worm");
    std::fs::write(
        &script,
        "#!/usr/bin/env python3\nimport sys, json, struct\nwhile True:\n    n = sys.stdin.buffer.read(4)\n    if len(n) < 4: sys.exit(0)\n    sys.stdin.buffer.read(struct.unpack(\"<I\", n)[0])\n    b = json.dumps({\"ok\": False, \"error\": \"no\"}).encode()\n    sys.stdout.buffer.write(struct.pack(\"<I\", len(b)) + b)\n    sys.stdout.buffer.flush()\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let cache = tempfile::tempdir().unwrap();
    let err = larvae::worm::fetch::adopt(&bin, &cache.path().join("d"), "mute")
        .expect_err("no manifest, no worm");

    assert!(format!("{err:#}").contains("mute"), "{err:#}");
}
