/*
The vendored Luau build, in two halves so the shim iterates fast.

The vendor half compiles the pinned Luau sources once into a static
library, keyed by a marker file: while the pin holds, a rebuild skips it
in under a second. The shim half compiles our one C++ file every time,
which is the file that actually changes during development.

Every compiler flag goes through `cc`, which spells it for the toolchain it
found. The link step below cannot: it makes a shared library, and no two of
the three platforms spell that the same way.
*/

fn main() {
    #[cfg(feature = "analyzer")]
    build_analyzer();
}

/*
Which shared library this target wants, and how its linker asks for one.

The three platforms disagree about the file name, about the flag that hides
every symbol but ours, and about how a binary finds a library beside it.
They are gathered here so the steps below read as one story rather than as
three interleaved ones.

The name matches what `std::env::consts::DLL_PREFIX` and `DLL_SUFFIX` report
at runtime, which is what `larvae self install` looks for. It is derived from
the target and not from those constants, because a build script runs on the
host and may be building for another platform.
*/
#[cfg(feature = "analyzer")]
struct Platform {
    /// True for the MSVC toolchain, which shares nothing with the other two
    msvc: bool,
    /// True for macOS, where the linker is ld64 and spells its own flags
    apple: bool,
    /// ex: `libeclipse_analysis.so`, `libeclipse_analysis.dylib`, `eclipse_analysis.dll`
    library: String,
    /// The extension an object file takes: `.o`, or `.obj` under MSVC
    object: &'static str,
}

#[cfg(feature = "analyzer")]
impl Platform {
    fn of(target: &str) -> Self {
        let msvc = target.contains("msvc");
        let apple = target.contains("apple") || target.contains("darwin");

        // Windows targets that are not MSVC, ex: `*-pc-windows-gnu`, still
        // produce a `.dll` and still take no prefix.
        let windows = target.contains("windows");

        let (prefix, suffix) = if windows {
            ("", ".dll")
        } else if apple {
            ("lib", ".dylib")
        } else {
            ("lib", ".so")
        };

        Self {
            msvc,
            apple,
            library: format!("{prefix}eclipse_analysis{suffix}"),
            object: if msvc { "obj" } else { "o" },
        }
    }

    /// The name `cc` gives the static library it compiles under `name`
    fn archive(&self, name: &str) -> String {
        match self.msvc {
            true => format!("{name}.lib"),

            false => format!("lib{name}.a"),
        }
    }
}

#[cfg(feature = "analyzer")]
fn build_analyzer() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let luau = std::path::Path::new("luau");
    let target = std::env::var("TARGET").unwrap();
    let platform = Platform::of(&target);

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

    patch_vendored(luau);

    let marker = out.join(format!("luau-{pin}.built"));
    let vendor_lib = out.join(platform.archive("luauvendor"));

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

        harden(&mut vendor);

        for dir in [
            /*
            Common/src is load bearing: TimeTrace and hashRange live
            there. A Linux shared library links with the symbols
            missing and resolves lazily, so the gap stayed silent;
            macOS and MSVC refuse the link, which is how the release
            found it.
            */
            "Common/src",
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

    harden(&mut shim);

    shim.compile("larvaeshim");

    seal(&out, &shim, &platform, &target);

    println!(
        "cargo:rustc-link-search=native={}",
        out.join("sealed").display()
    );
    println!("cargo:rustc-link-lib=dylib=eclipse_analysis");

    /*
    The name this build produced, so the crate can check it.

    `larvae self install` looks the library up through
    `std::env::consts::DLL_PREFIX` and `DLL_SUFFIX`. A build that spelled it
    any other way would ship an archive the installer walks past, and the
    server would refuse to start with the analyzer missing. The test in
    `analyzer.rs` compares the two on whatever platform it runs.
    */
    println!(
        "cargo:rustc-env=LARVAE_ANALYZER_LIBRARY={}",
        platform.library
    );

    /*
    How the binary finds the library beside it.

    Windows searches the directory the executable came from, so it needs
    nothing. The other two need a run path, and they spell the same idea
    with different words.
    */
    if platform.apple {
        println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
    } else if !platform.msvc {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }
}

/*
The two flags the C++ needs that `cc` does not supply.

MSVC compiles without unwinding unless it is asked, and this shim is built
out of `try` and `catch`: Luau's frontend throws, and a checker that could
not catch reaches the editor as a crash. `_CRT_SECURE_NO_WARNINGS` is what
Luau's own build sets, for the same portable CRT calls.

Both go through `flag_if_supported`, so a compiler that does not know the
spelling drops it instead of failing.
*/
#[cfg(feature = "analyzer")]
fn harden(build: &mut cc::Build) {
    build.flag_if_supported("/EHsc");
    build.define("_CRT_SECURE_NO_WARNINGS", None);
}

/*
The analyzer as one shared library that exports the shim API and nothing
else.

larvae links mlua, which carries its own vendored Luau VM at another
version, and two Luau copies in one static link is a wall of duplicate
and half-bound weak symbols; the archive experiments died there. A shared
library is the isolation the platform actually supports: everything inside
is hidden, the larvae_* functions are the export table, and the dynamic
linker keeps the two Luau worlds apart by construction.

The library lands beside the binary. The C++ runtime links statically where
the toolchain offers that, so shipping stays two files.
*/
#[cfg(feature = "analyzer")]
fn seal(out: &std::path::Path, shim: &cc::Build, platform: &Platform, target: &str) {
    let sealed = out.join("sealed");
    std::fs::create_dir_all(&sealed).unwrap();

    let archives = [
        out.join(platform.archive("luauvendor")),
        out.join("shim").join(platform.archive("larvaeshim")),
    ];

    let lib = sealed.join(&platform.library);

    match platform.msvc {
        true => seal_msvc(&sealed, &archives, &lib, target),

        false => seal_unix(&sealed, &archives, &lib, shim, platform),
    }

    /*
    The library must sit beside the binary for the run path to find it.
    OUT_DIR sits under target/<profile>/build/..., so three parents up is
    the profile directory that holds the binary.
    */
    let profile_dir = out
        .ancestors()
        .nth(3)
        .expect("OUT_DIR sits under the profile directory");

    std::fs::copy(&lib, profile_dir.join(&platform.library))
        .expect("the library lands beside the binary");
}

/*
The shared library, linked by the C++ driver that compiled the objects.

The objects come out of the archives first, because a linker pulls only
what something references and the export list is the only thing that
references any of this.

The driver comes from `cc` rather than from the name `c++`, so a cross
build uses the compiler it was told to use and reads the environment that
came with it.
*/
#[cfg(feature = "analyzer")]
fn seal_unix(
    sealed: &std::path::Path,
    archives: &[std::path::PathBuf],
    lib: &std::path::Path,
    shim: &cc::Build,
    platform: &Platform,
) {
    let scratch = sealed.join("objects");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();

    for archive in archives {
        let mut ar = shim.get_archiver();

        let status = ar
            .arg("x")
            .arg(archive)
            .current_dir(&scratch)
            .status()
            .expect("the archiver extracts the archives");

        assert!(status.success(), "ar x failed for {}", archive.display());
    }

    let objects: Vec<_> = std::fs::read_dir(&scratch)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == platform.object))
        .collect();

    assert!(!objects.is_empty(), "the archives held no object files");

    let mut link = shim.get_compiler().to_command();

    link.arg("-o").arg(lib).args(&objects);

    if platform.apple {
        /*
        ld64 takes a list of the symbols to keep, and every name in it
        carries the underscore the Mach-O ABI puts in front. The install
        name is the run path the binary was linked with, so the loader
        looks beside the binary and nowhere else.
        */
        let exports = sealed.join("exports.sym");
        std::fs::write(&exports, "_larvae_*\n").unwrap();

        link.arg("-dynamiclib")
            .arg("-Wl,-exported_symbols_list")
            .arg(&exports)
            .arg(format!(
                "-Wl,-install_name,@rpath/{}",
                lib.file_name().unwrap().to_string_lossy()
            ));
    } else {
        /*
        GNU ld takes a version script, which names what stays global and
        hides the rest. The C++ runtime goes in statically, so the library
        carries no libstdc++ question to the machine that runs it.
        */
        let exports = sealed.join("exports.map");
        std::fs::write(&exports, "{ global: larvae_*; local: *; };\n").unwrap();

        link.arg("-shared")
            .arg(format!("-Wl,--version-script={}", exports.display()))
            .arg("-Wl,--exclude-libs,ALL")
            .arg("-static-libstdc++")
            .arg("-static-libgcc");
    }

    let status = link.status().expect("the compiler links the library");

    assert!(status.success(), "linking {} failed", lib.display());
}

/*
The shared library, linked by MSVC.

Nothing here matches the other two. The linker is a separate program, it
takes the static libraries whole rather than the objects, and it exports
only what a module definition file names: on Windows a symbol is private
until something says otherwise, which is the isolation this whole step
exists to build, given for free.

The export list is read out of `shim.h`, so a function added to the surface
is exported without a second edit here.
*/
#[cfg(feature = "analyzer")]
fn seal_msvc(
    sealed: &std::path::Path,
    archives: &[std::path::PathBuf],
    lib: &std::path::Path,
    target: &str,
) {
    let header = std::fs::read_to_string("shim/shim.h").expect("shim.h names the surface");

    let mut exports: Vec<String> = Vec::new();

    for (at, _) in header.match_indices("larvae_") {
        let name: String = header[at..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();

        // Only a call, ex: `larvae_open(`. A typedef or a word in prose is not one.
        if header[at + name.len()..].starts_with('(') && !exports.contains(&name) {
            exports.push(name);
        }
    }

    exports.sort();

    assert!(!exports.is_empty(), "shim.h named no exported function");

    let definition = sealed.join("exports.def");
    let mut text = String::from("EXPORTS\n");

    for name in &exports {
        text.push_str("    ");
        text.push_str(name);
        text.push('\n');
    }

    std::fs::write(&definition, text).unwrap();

    let mut link = match cc::windows_registry::find_tool(target, "link.exe") {
        Some(tool) => tool.to_command(),

        None => std::process::Command::new("link.exe"),
    };

    link.arg("/NOLOGO")
        .arg("/DLL")
        .arg(format!("/DEF:{}", definition.display()))
        .arg(format!("/OUT:{}", lib.display()))
        .arg(format!(
            "/IMPLIB:{}",
            sealed.join("eclipse_analysis.lib").display()
        ));

    for archive in archives {
        link.arg(archive);
    }

    let status = link.status().expect("link.exe links the library");

    assert!(status.success(), "linking {} failed", lib.display());
}

/*
The display patches larvae carries onto the vendored Luau.

The vendor is a submodule pinned by luau.pin, so a change cannot be
committed there; it is re-applied here, before the vendor compiles, and
skipped when it is already present. When a pin bump moves the anchor
text, the warning below says the patch stopped applying, which is the
moment to re-read the upstream code and re-anchor or retire the patch.

One patch today. A zero-argument function's pack collapses to a bare
hidden variadic when a module interface is cloned, and the stringifier
dispatches a bare pack straight to the variadic printer, which ignores
the hidden flag that the wrapped-pack path honors. The printed result
was `(...any) -> T` for a function the author wrote as `()`. The patch
makes the function printer treat a bare hidden tail as the empty
argument list, the same answer the wrapped form already gets.
*/
#[cfg(feature = "analyzer")]
fn patch_vendored(luau: &std::path::Path) {
    let file = luau.join("Analysis/src/ToString.cpp");

    let Ok(text) = std::fs::read_to_string(&file) else {
        println!("cargo:warning=larvae: cannot read the vendored ToString.cpp to patch it");

        return;
    };

    let marker = "larvae: a bare hidden tail is an empty argument list";

    if text.contains(marker) {
        return;
    }

    let anchor = "        if (isEmpty(ftv.argTypes))
        {
            // if we've got an empty argument pack, we're done.
        }
        else if (state.opts.functionTypeArguments)";

    let Some(_) = text.find(anchor) else {
        println!(
            "cargo:warning=larvae: the ToString.cpp patch anchor is gone; zero-argument \
             functions render as (...any) across modules until it is re-anchored"
        );

        return;
    };

    let replacement = "        if (isEmpty(ftv.argTypes))
        {
            // if we've got an empty argument pack, we're done.
        }
        // larvae: a bare hidden tail is an empty argument list. See the
        // build script for why this line is applied rather than committed.
        else if (auto vtp = get<VariadicTypePack>(follow(ftv.argTypes));
                 vtp && vtp->hidden && FInt::DebugLuauVerboseTypeNames < 1)
        {
        }
        else if (state.opts.functionTypeArguments)";

    let patched = text.replacen(anchor, replacement, 1);

    if std::fs::write(&file, patched).is_err() {
        println!("cargo:warning=larvae: cannot write the ToString.cpp patch");
    }
}
