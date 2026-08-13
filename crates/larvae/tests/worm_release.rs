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
