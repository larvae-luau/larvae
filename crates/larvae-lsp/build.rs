/*
The vendored Luau build, in two halves so the shim iterates fast.

The vendor half compiles the pinned Luau sources once into a static
library, keyed by a marker file: while the pin holds, a rebuild skips it
in under a second. The shim half compiles our one C++ file every time,
which is the file that actually changes during development.
*/

fn main() {
    #[cfg(feature = "analyzer")]
    build_analyzer();
}

#[cfg(feature = "analyzer")]
fn build_analyzer() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let luau = std::path::Path::new("luau");

    println!("cargo:rerun-if-changed=shim/shim.cpp");
    println!("cargo:rerun-if-changed=shim/shim.h");
    println!("cargo:rerun-if-changed=luau.pin");

    /*
    The pin lives in one file, so the nightly workflow moves it without
    touching this script: it writes the new tag into luau.pin, points the
    submodule at it, and the stale marker forces one vendor rebuild.
    */
    let pin = std::fs::read_to_string("luau.pin")
        .expect("luau.pin names the vendored Luau release")
        .trim()
        .to_string();

    let marker = out.join(format!("luau-{pin}.built"));
    let vendor_lib = out.join("libluauvendor.a");

    if !marker.exists() || !vendor_lib.exists() {
        let mut vendor = cc::Build::new();

        vendor
            .cpp(true)
            .std("c++17")
            .include(luau.join("Common/include"))
            .include(luau.join("Ast/include"))
            .include(luau.join("Config/include"))
            .include(luau.join("Bytecode/include"))
            .include(luau.join("Compiler/include"))
            .include(luau.join("VM/include"))
            .include(luau.join("VM/src"))
            .include(luau.join("Analysis/include"))
            .define("LUA_USE_LONGJMP", "1")
            .warnings(false);

        for dir in [
            "Ast/src",
            "Config/src",
            "Bytecode/src",
            "Compiler/src",
            "VM/src",
            "Analysis/src",
        ] {
            for entry in std::fs::read_dir(luau.join(dir)).unwrap() {
                let path = entry.unwrap().path();

                if path.extension().is_some_and(|e| e == "cpp") {
                    vendor.file(path);
                }
            }
        }

        vendor
            .cargo_metadata(false)
            .out_dir(&out)
            .compile("luauvendor");
        std::fs::write(&marker, &pin).unwrap();
    }

    let mut shim = cc::Build::new();

    shim.cpp(true)
        .std("c++17")
        .include(luau.join("Common/include"))
        .include(luau.join("Ast/include"))
        .include(luau.join("Config/include"))
        .include(luau.join("Bytecode/include"))
        .include(luau.join("Compiler/include"))
        .include(luau.join("Analysis/include"))
        .file("shim/shim.cpp")
        .warnings(false)
        .cargo_metadata(false)
        .out_dir(out.join("shim"));

    shim.compile("larvaeshim");

    seal(&out);

    println!(
        "cargo:rustc-link-search=native={}",
        out.join("sealed").display()
    );
    println!("cargo:rustc-link-lib=dylib=eclipse_analysis");
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
}

/*
The analyzer as one shared library that exports the shim API and nothing
else.

larvae links mlua, which carries its own vendored Luau VM at another
version, and two Luau copies in one static link is a wall of duplicate
and half-bound weak symbols; the archive experiments died there. A shared
object is the isolation the platform actually supports: everything inside
is hidden, the nine larvae_* functions are the export table, and the
dynamic linker keeps the two Luau worlds apart by construction.

The library lands beside the binary, and the binary finds it through
$ORIGIN. The C++ runtime links statically into the .so, so shipping stays
two files with no libstdc++ question.
*/
#[cfg(feature = "analyzer")]
fn seal(out: &std::path::Path) {
    let sealed = out.join("sealed");
    std::fs::create_dir_all(&sealed).unwrap();

    let scratch = sealed.join("objects");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();

    for archive in [
        out.join("libluauvendor.a"),
        out.join("shim/liblarvaeshim.a"),
    ] {
        let status = std::process::Command::new("ar")
            .arg("x")
            .arg(&archive)
            .current_dir(&scratch)
            .status()
            .expect("ar extracts the archives");

        assert!(status.success(), "ar x failed for {}", archive.display());
    }

    let objects: Vec<_> = std::fs::read_dir(&scratch)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "o"))
        .collect();

    let exports = sealed.join("exports.map");
    std::fs::write(&exports, "{ global: larvae_*; local: *; };\n").unwrap();

    let lib = sealed.join("libeclipse_analysis.so");

    let status = std::process::Command::new("c++")
        .arg("-shared")
        .arg("-o")
        .arg(&lib)
        .args(&objects)
        .arg(format!("-Wl,--version-script={}", exports.display()))
        .arg("-Wl,--exclude-libs,ALL")
        .arg("-static-libstdc++")
        .arg("-static-libgcc")
        .status()
        .expect("c++ links the analyzer library");

    assert!(status.success(), "linking libeclipse_analysis.so failed");

    /*
    The library must sit beside the binary for $ORIGIN to find it. OUT_DIR
    sits under target/<profile>/build/..., so three parents up is the
    profile directory that holds the binary.
    */
    let profile_dir = out
        .ancestors()
        .nth(3)
        .expect("OUT_DIR sits under the profile directory");

    std::fs::copy(&lib, profile_dir.join("libeclipse_analysis.so"))
        .expect("the library lands beside the binary");
}
