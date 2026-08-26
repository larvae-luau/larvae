/*!
Luau's analysis frontend, behind the seam.

The C++ session lives in `shim/shim.cpp`; this module is the Rust half:
the FFI declarations, the safe wrapper that implements the server's
[`Analysis`] trait, and the resolver callbacks that answer the frontend's
require questions from Rust.

Resolution here covers what a plain Luau project writes: a relative path
against the requiring file (init aware), an `@self` prefix, and the
aliases of the nearest `.luaurc` walking up from the requiring file. A
spec that resolves to a directory answers its init file. Everything else
returns nothing, and the frontend reports an unknown require, which is
what it should say.
*/

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};

use crate::resolve::resolve_spec;

/*
The Roblox global types, vendored beside the crate and refreshed by the
nightly. The session loads them at start, so the DataModel exists for
inference, and the service list for auto-imports reads from the same
text, so the two cannot disagree.
*/
const GLOBAL_TYPES: &str = include_str!("../types/globalTypes.d.luau");

use larvae::lsp::analysis::{Analysis, AnalysisCompletion, AnalysisDiag, ModuleHooks};

#[repr(C)]
struct RawDiag {
    start: u32,
    end: u32,
    severity: u8,
    message: *const c_char,
}

#[repr(C)]
struct RawParameter {
    label: *const c_char,
}

#[repr(C)]
struct RawSignature {
    label: *const c_char,
    active: u32,
    count: usize,
}

#[repr(C)]
struct RawHint {
    line: u32,
    character: u32,
    label: *const c_char,
    kind: u8,
}

#[repr(C)]
struct RawLocation {
    path: *const c_char,
    start_line: u32,
    start_character: u32,
    end_line: u32,
    end_character: u32,
}

#[repr(C)]
struct RawCompletion {
    label: *const c_char,
    kind: u8,
}

#[allow(non_camel_case_types)]
type larvae_resolve_fn = extern "C" fn(*mut c_void, *const c_char, *const c_char) -> *const c_char;
#[allow(non_camel_case_types)]
type larvae_load_fn = extern "C" fn(*mut c_void, *const c_char) -> *const c_char;

unsafe extern "C" {
    fn larvae_enable_all_flags();
    fn larvae_set_flag(name: *const c_char, value: *const c_char) -> i32;
    fn larvae_apply_required_flags();
    fn larvae_signature_help(
        s: *mut c_void,
        path: *const c_char,
        byte: u32,
        sig: *mut RawSignature,
        out: *mut RawParameter,
        cap: usize,
    ) -> i32;
    fn larvae_inlay_hints(
        s: *mut c_void,
        path: *const c_char,
        out: *mut RawHint,
        cap: usize,
    ) -> usize;
    fn larvae_definition(
        s: *mut c_void,
        path: *const c_char,
        byte: u32,
        out: *mut RawLocation,
    ) -> i32;
    fn larvae_type_definition(
        s: *mut c_void,
        path: *const c_char,
        byte: u32,
        out: *mut RawLocation,
    ) -> i32;
    fn larvae_session_new() -> *mut c_void;
    fn larvae_set_definitions(s: *mut c_void, name: *const c_char, source: *const c_char) -> i32;
    fn larvae_session_free(s: *mut c_void);
    fn larvae_set_resolver(
        s: *mut c_void,
        userdata: *mut c_void,
        resolve: larvae_resolve_fn,
        load: larvae_load_fn,
    );
    fn larvae_open(s: *mut c_void, path: *const c_char, text: *const c_char);
    fn larvae_invalidate(s: *mut c_void, path: *const c_char);
    fn larvae_check(s: *mut c_void, path: *const c_char, out: *mut RawDiag, cap: usize) -> usize;
    fn larvae_hover(
        s: *mut c_void,
        path: *const c_char,
        byte: u32,
        show_table_kinds: i32,
    ) -> *const c_char;
    fn larvae_completions(
        s: *mut c_void,
        path: *const c_char,
        byte: u32,
        out: *mut RawCompletion,
        cap: usize,
    ) -> usize;
}

/*
The state the resolver callbacks read. It lives in a Box whose address is
the `userdata` the shim hands back, so the callbacks find it without any
global. The string buffers hold the last answers, per the shim contract:
valid until the next call on the same session.
*/
struct ResolverState {
    resolve_buffer: Option<CString>,
    load_buffer: Option<CString>,
    /// The worm hooks the server installs; consulted before default resolution
    hooks: Option<ModuleHooks>,
    /*
    The DataModel map of the project, for `@game`.

    Absent until the server loads a config that describes one. A project with
    no rojo project file and no `[requires.mounts]` has no DataModel, and
    `@game` then resolves to nothing, which is the true answer.
    */
    mounts: Option<larvae::requires::datamodel::MountTable>,
}

extern "C" fn resolve_cb(
    userdata: *mut c_void,
    from: *const c_char,
    spec: *const c_char,
) -> *const c_char {
    let state = unsafe { &mut *(userdata as *mut ResolverState) };
    let from = unsafe { CStr::from_ptr(from) }.to_string_lossy();
    let spec = unsafe { CStr::from_ptr(spec) }.to_string_lossy();

    /*
    The worms answer first. A worm that claims the spec gives the analyzer
    its module; every other spec falls through to default resolution, which
    is the hook-or-fallthrough shape the plan draws.
    */
    if let Some(hooks) = &state.hooks
        && let Some(path) = (hooks.resolve)(Path::new(from.as_ref()), &spec)
    {
        state.resolve_buffer = CString::new(path).ok();

        return state
            .resolve_buffer
            .as_ref()
            .map_or(std::ptr::null(), |c| c.as_ptr());
    }

    match resolve_spec(Path::new(from.as_ref()), &spec, state.mounts.as_ref()) {
        Some(path) => {
            let text = path.to_string_lossy().into_owned();

            state.resolve_buffer = CString::new(text).ok();

            state
                .resolve_buffer
                .as_ref()
                .map_or(std::ptr::null(), |c| c.as_ptr())
        }

        None => std::ptr::null(),
    }
}

extern "C" fn load_cb(userdata: *mut c_void, path: *const c_char) -> *const c_char {
    let state = unsafe { &mut *(userdata as *mut ResolverState) };
    let path = unsafe { CStr::from_ptr(path) }.to_string_lossy();

    // A worm-resolved module loads through the worm, lowered to Luau.
    if let Some(hooks) = &state.hooks
        && let Some(text) = (hooks.load)(path.as_ref())
    {
        state.load_buffer = CString::new(text).ok();

        return state
            .load_buffer
            .as_ref()
            .map_or(std::ptr::null(), |c| c.as_ptr());
    }

    match std::fs::read_to_string(path.as_ref()) {
        Ok(text) => {
            // A required file can hold larvae syntax; the analyzer reads stock Luau.
            let text = larvae::lsp::analysis::plain_view(&text).into_owned();

            state.load_buffer = CString::new(text).ok();

            state
                .load_buffer
                .as_ref()
                .map_or(std::ptr::null(), |c| c.as_ptr())
        }

        Err(_) => std::ptr::null(),
    }
}
pub struct LuauAnalysis {
    session: *mut c_void,
    /// Owned by the session for its lifetime; the shim only borrows it
    resolver: Box<ResolverState>,
    /// Path strings the session knows, so invalidate spells them the same way
    keys: HashMap<PathBuf, CString>,
    /// The service names, extracted from the definitions once
    services: Vec<String>,
}

// One session, used from the one server thread; the raw pointer is why
// the compiler cannot see it.
unsafe impl Send for LuauAnalysis {}

impl LuauAnalysis {
    pub fn new() -> Self {
        let mut resolver = Box::new(ResolverState {
            resolve_buffer: None,
            load_buffer: None,
            hooks: None,
            mounts: None,
        });

        let session = unsafe { larvae_session_new() };

        unsafe {
            larvae_set_resolver(
                session,
                &mut *resolver as *mut ResolverState as *mut c_void,
                resolve_cb,
                load_cb,
            );
        }

        let mut new = Self {
            session,
            resolver,
            keys: HashMap::new(),
            services: Vec::new(),
        };

        /*
        The result is read, not dropped.

        It was dropped, and the load had been failing: Luau refused the
        whole file on its inference limits, so `game` had no type and every
        Roblox completion came from nothing. A silent failure hid that for
        as long as the file has shipped. A server that cannot type the
        platform says so.
        */
        if !larvae::lsp::analysis::Analysis::definitions(&mut new, "@roblox", GLOBAL_TYPES) {
            eprintln!(
                "larvae-lsp: the Roblox type definitions did not load, so \
                 game and the services have no type"
            );
        }

        new
    }

    fn key(&mut self, path: &Path) -> *const c_char {
        self.keys
            .entry(path.to_path_buf())
            .or_insert_with(|| {
                CString::new(path.to_string_lossy().into_owned()).unwrap_or_default()
            })
            .as_ptr()
    }
}

impl Drop for LuauAnalysis {
    fn drop(&mut self) {
        unsafe { larvae_session_free(self.session) };
    }
}

impl LuauAnalysis {
    /*
    One location question, asked of the shim.

    Both questions have the same shape, so one helper spells the unsafe
    part once. A zero reply means the frontend has no answer, which is the
    honest result for a name it cannot follow.
    */
    fn locate(
        &mut self,
        ask: unsafe extern "C" fn(*mut c_void, *const c_char, u32, *mut RawLocation) -> i32,
        path: &Path,
        at: u32,
    ) -> Option<larvae::lsp::analysis::AnalysisLocation> {
        let key = self.key(path);

        let mut raw = RawLocation {
            path: std::ptr::null(),
            start_line: 0,
            start_character: 0,
            end_line: 0,
            end_character: 0,
        };

        let ok = unsafe { ask(self.session, key, at, &mut raw) };

        if ok == 0 || raw.path.is_null() {
            return None;
        }

        let target = unsafe { CStr::from_ptr(raw.path) }.to_str().ok()?;

        Some(larvae::lsp::analysis::AnalysisLocation {
            path: PathBuf::from(target),
            start: (raw.start_line, raw.start_character),
            end: (raw.end_line, raw.end_character),
        })
    }
}

impl Analysis for LuauAnalysis {
    fn definition(
        &mut self,
        path: &Path,
        at: u32,
    ) -> Option<larvae::lsp::analysis::AnalysisLocation> {
        self.locate(larvae_definition, path, at)
    }

    fn type_definition(
        &mut self,
        path: &Path,
        at: u32,
    ) -> Option<larvae::lsp::analysis::AnalysisLocation> {
        self.locate(larvae_type_definition, path, at)
    }

    fn signature(
        &mut self,
        path: &Path,
        at: u32,
    ) -> Option<larvae::lsp::analysis::AnalysisSignature> {
        let key = self.key(path);

        let mut sig = RawSignature {
            label: std::ptr::null(),
            active: 0,
            count: 0,
        };

        // A signature with more parameters than this is not readable anyway.
        const CAP: usize = 64;
        let mut raw: [RawParameter; CAP] = [const {
            RawParameter {
                label: std::ptr::null(),
            }
        }; CAP];

        let ok = unsafe {
            larvae_signature_help(self.session, key, at, &mut sig, raw.as_mut_ptr(), CAP)
        };

        if ok == 0 || sig.label.is_null() {
            return None;
        }

        let label = unsafe { CStr::from_ptr(sig.label) }
            .to_string_lossy()
            .into_owned();

        let n = sig.count.min(CAP);

        let parameters = raw[..n]
            .iter()
            .filter(|p| !p.label.is_null())
            .map(|p| {
                unsafe { CStr::from_ptr(p.label) }
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        Some(larvae::lsp::analysis::AnalysisSignature {
            label,
            parameters,
            active: sig.active,
        })
    }

    fn hints(&mut self, path: &Path) -> Vec<larvae::lsp::analysis::AnalysisHint> {
        let key = self.key(path);

        /*
        A module with more hints than this is one nobody reads with hints
        on. The count comes back whole, so a truncation is visible here and
        the editor still gets a useful screenful.
        */
        const CAP: usize = 2048;
        let mut raw: [RawHint; CAP] = [const {
            RawHint {
                line: 0,
                character: 0,
                label: std::ptr::null(),
                kind: 1,
            }
        }; CAP];

        let n = unsafe { larvae_inlay_hints(self.session, key, raw.as_mut_ptr(), CAP) };

        raw[..n.min(CAP)]
            .iter()
            .filter(|h| !h.label.is_null())
            .map(|h| larvae::lsp::analysis::AnalysisHint {
                line: h.line,
                character: h.character,
                label: unsafe { CStr::from_ptr(h.label) }
                    .to_string_lossy()
                    .into_owned(),
                kind: h.kind,
            })
            .collect()
    }

    fn set_flags(&mut self, flags: &larvae::config::lsp::FFlagsConfig) -> Vec<String> {
        if flags.enable_by_default {
            unsafe { larvae_enable_all_flags() };
        }

        if flags.enable_new_solver
            && let Ok(name) = CString::new("LuauSolverV2")
            && let Ok(on) = CString::new("true")
        {
            unsafe { larvae_set_flag(name.as_ptr(), on.as_ptr()) };
        }

        let mut unknown = Vec::new();

        for (name, value) in &flags.over {
            let (Ok(key), Ok(text)) = (CString::new(name.as_str()), CString::new(value.as_str()))
            else {
                continue;
            };

            match unsafe { larvae_set_flag(key.as_ptr(), text.as_ptr()) } {
                0 => {}

                1 => unknown.push(format!("{name} is not a Luau flag")),

                _ => unknown.push(format!("{name} does not take the value {value:?}")),
            }
        }

        // Last, so an override cannot take away what larvae needs to work.
        unsafe { larvae_apply_required_flags() };

        unknown
    }

    fn set_mounts(&mut self, mounts: larvae::requires::datamodel::MountTable) {
        self.resolver.mounts = Some(mounts);
    }

    fn services(&mut self) -> Vec<String> {
        if self.services.is_empty() {
            /*
            The first line of the definitions is machine metadata,
            `--#METADATA#{...}`, and its SERVICES array is the authority:
            luau-lsp writes it from the API dump for exactly this use.
            */
            self.services = GLOBAL_TYPES
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("--#METADATA#"))
                .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
                .and_then(|meta| {
                    meta.get("SERVICES").and_then(|s| s.as_array()).map(|list| {
                        list.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                })
                .unwrap_or_default();

            self.services.sort();
            self.services.dedup();
        }

        self.services.clone()
    }

    fn set_module_hooks(&mut self, hooks: ModuleHooks) {
        self.resolver.hooks = Some(hooks);
    }

    fn definitions(&mut self, name: &str, source: &str) -> bool {
        let (Ok(name), Ok(source)) = (CString::new(name), CString::new(source)) else {
            return false;
        };

        unsafe { larvae_set_definitions(self.session, name.as_ptr(), source.as_ptr()) == 0 }
    }

    fn open(&mut self, path: &Path, text: &str) {
        let Ok(text) = CString::new(text) else {
            return;
        };

        let key = self.key(path);

        unsafe { larvae_open(self.session, key, text.as_ptr()) };
    }

    fn check(&mut self, path: &Path) -> Vec<AnalysisDiag> {
        let key = self.key(path);
        let mut raw: Vec<RawDiag> = Vec::with_capacity(256);
        let n = unsafe { larvae_check(self.session, key, raw.as_mut_ptr(), 256) };

        unsafe { raw.set_len(n) };

        raw.iter()
            .map(|d| AnalysisDiag {
                span: (d.start, d.end),
                severity: d.severity,
                message: unsafe { CStr::from_ptr(d.message) }
                    .to_string_lossy()
                    .into_owned(),
                code: None,
            })
            .collect()
    }

    fn hover(&mut self, path: &Path, at: u32, show_table_kinds: bool) -> Option<String> {
        let key = self.key(path);
        let text = unsafe { larvae_hover(self.session, key, at, show_table_kinds as i32) };

        if text.is_null() {
            return None;
        }

        Some(
            unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    fn completions(&mut self, path: &Path, at: u32) -> Vec<AnalysisCompletion> {
        let key = self.key(path);
        let mut raw: Vec<RawCompletion> = Vec::with_capacity(256);
        let n = unsafe { larvae_completions(self.session, key, at, raw.as_mut_ptr(), 256) };

        unsafe { raw.set_len(n) };

        raw.iter()
            .map(|c| AnalysisCompletion {
                label: unsafe { CStr::from_ptr(c.label) }
                    .to_string_lossy()
                    .into_owned(),
                kind: c.kind,
                detail: None,
            })
            .collect()
    }

    fn invalidate(&mut self, path: &Path) {
        let key = self.key(path);

        unsafe { larvae_invalidate(self.session, key) };
    }
}

#[cfg(test)]
mod studio_definitions {
    use super::*;
    use larvae::lsp::analysis::Analysis;

    /*
    The Studio tree's declaration text has to load into the real frontend.

    The generator is checked against larvae's own definitions parser, which
    answers whether the syntax is legal. Whether Luau accepts the meaning is
    a different question: the text declares a subclass that shadows a
    property of the class it extends, and only the frontend decides if that
    holds. This is the measurement.
    */

    /*
    The vendored Roblox types have to load, or the platform has no types.

    They did not load. Luau refused the whole file on its inference limits
    and reported one error with no useful location, and the caller dropped
    the result, so `game` had no type and nobody saw why. This is the test
    that would have caught it.
    */
    #[test]
    fn the_vendored_roblox_types_load() {
        let mut analysis = LuauAnalysis::new();

        assert!(
            analysis.definitions("@roblox", GLOBAL_TYPES),
            "Luau refused the vendored globalTypes.d.luau"
        );
    }

    /// And what they declare has to be usable, which is the point of loading them.
    #[test]
    fn a_service_call_type_checks_against_them() {
        let mut analysis = LuauAnalysis::new();
        let path = std::path::Path::new("/place.luau");

        analysis.open(
            path,
            "--!strict\nlocal players = game:GetService(\"Players\")\nreturn players\n",
        );

        let complaints: Vec<String> = analysis
            .check(path)
            .into_iter()
            .map(|d| d.message)
            .collect();

        assert!(
            complaints.is_empty(),
            "GetService did not type check, so the platform types are not in: {complaints:?}"
        );
    }

    #[test]
    fn a_mirrored_place_loads_into_luau() {
        let place = larvae::lsp::studio::sample_place();
        let text = larvae::lsp::studio::definitions(&place);

        assert!(!text.is_empty(), "the sample place produced no text");

        let mut analysis = LuauAnalysis::new();

        assert!(
            analysis.definitions("@studio", &text),
            "Luau refused the Studio definitions:\n{text}"
        );
    }

    /*
    And the types it declares have to be usable, not only loadable.

    A file that reads `game.Workspace.Baseplate` must type check, or the
    mirror gives the editor nothing a reader can see.
    */
    #[test]
    fn a_path_through_the_mirrored_tree_type_checks() {
        let place = larvae::lsp::studio::sample_place();
        let text = larvae::lsp::studio::definitions(&place);

        let mut analysis = LuauAnalysis::new();

        assert!(analysis.definitions("@studio", &text), "the text loads");

        let path = std::path::Path::new("/place.luau");
        let src = "--!strict\nlocal part = game.Workspace.Baseplate\nreturn part\n";

        analysis.open(path, src);

        let complaints: Vec<String> = analysis
            .check(path)
            .into_iter()
            .map(|d| d.message)
            .collect();

        assert!(
            complaints.is_empty(),
            "the mirrored path did not type check: {complaints:?}\n{text}"
        );
    }
}

#[cfg(test)]
mod flags {
    use super::*;
    use larvae::config::lsp::FFlagsConfig;
    use larvae::lsp::analysis::Analysis;

    /// An override reaches Luau, and a bad name comes back rather than vanishing.
    #[test]
    fn an_override_reaches_luau_and_a_typo_is_reported() {
        let mut analysis = LuauAnalysis::new();

        let mut flags = FFlagsConfig::default();
        flags.over.insert("LuauSolverV2".into(), "true".into());
        flags
            .over
            .insert("LuauNotARealFlagAtAll".into(), "true".into());
        flags
            .over
            .insert("LuauTarjanChildLimit".into(), "not a number".into());

        let complaints = analysis.set_flags(&flags);

        assert_eq!(complaints.len(), 2, "{complaints:?}");
        assert!(
            complaints
                .iter()
                .any(|c| c.contains("LuauNotARealFlagAtAll")),
            "{complaints:?}"
        );
        assert!(
            complaints
                .iter()
                .any(|c| c.contains("LuauTarjanChildLimit")),
            "{complaints:?}"
        );
    }

    /*
    The values larvae requires survive an override that would remove them.

    They are applied last for that reason. A project that set the Tarjan
    limit back to its default would otherwise lose the Roblox types, and the
    only symptom is that `game` stops having a type.
    */
    #[test]
    fn a_required_value_wins_over_an_override() {
        let mut analysis = LuauAnalysis::new();

        let mut flags = FFlagsConfig::default();
        flags
            .over
            .insert("LuauTarjanChildLimit".into(), "10000".into());

        assert!(
            analysis.set_flags(&flags).is_empty(),
            "the override is valid"
        );

        // The types still load, so the required value went back afterwards.
        assert!(
            analysis.definitions("@roblox", GLOBAL_TYPES),
            "an override took away what larvae needs"
        );
    }

    /// Turning every flag on must not stop the types loading.
    #[test]
    fn every_flag_on_still_loads_the_types() {
        let mut analysis = LuauAnalysis::new();

        let flags = FFlagsConfig {
            enable_by_default: true,
            ..Default::default()
        };

        assert!(analysis.set_flags(&flags).is_empty());
        assert!(analysis.definitions("@roblox", GLOBAL_TYPES));
    }
}

#[cfg(test)]
mod hover_cards {
    use super::*;
    use larvae::lsp::analysis::Analysis;

    /// Hover the source at a byte offset, with table kinds hidden as they are by default.
    fn card(src: &str, at: u32) -> Option<String> {
        let mut analysis = LuauAnalysis::new();
        let path = std::path::Path::new("/t.luau");

        analysis.open(path, src);
        analysis.hover(path, at, false)
    }

    /// The offset of the first byte of `word` in `src`.
    fn at(src: &str, word: &str) -> u32 {
        src.find(word).expect("the word is in the source") as u32
    }

    /*
    A local hovers, which is the case the first cut of this missed entirely.

    It asked `findTypeAtPosition`, which answers for an expression. A local's
    declaration is not one, so the name a reader just wrote showed nothing.
    */
    #[test]
    fn a_local_shows_its_name_and_type() {
        let src = "--!strict\nlocal total = 1 + 2\nreturn total\n";

        assert_eq!(
            card(src, at(src, "total")),
            Some("local total: number".into())
        );
    }

    /// A function shows the signature the author wrote, not its type.
    #[test]
    fn a_function_shows_its_signature() {
        let src = "--!strict\nlocal function add(a: number, b: number): number\n\treturn a + b\nend\nreturn add\n";
        let text = card(src, at(src, "add")).expect("a card");

        assert_eq!(text, "function add(a: number, b: number): number");
    }

    /*
    A type alias shows what the name stands for, at the declaration and at
    every use. The use needed the walk to include types, which it does not do
    by default, so a reference hovered as nothing while its declaration was
    fine.
    */
    #[test]
    fn a_type_alias_shows_what_it_stands_for() {
        let src = "--!strict\ntype Point = { x: number }\nlocal p: Point = { x = 1 }\nreturn p\n";

        let declaration = card(src, at(src, "Point")).expect("the declaration");
        let reference = card(src, src.rfind("Point").expect("the use") as u32).expect("the use");

        assert!(declaration.starts_with("type Point ="), "{declaration}");
        assert!(reference.starts_with("type Point ="), "{reference}");
    }

    /*
    The sealed table marker is hidden unless the project asks for it.

    `{| x: number |}` answers a question somebody writing a type asks, and a
    reader hovering a value is not asking it. luau-lsp hides it too.
    */
    #[test]
    fn the_table_kind_marker_follows_the_setting() {
        let src = "--!strict\nlocal map = { x = 1 }\nreturn map\n";
        let mut analysis = LuauAnalysis::new();
        let path = std::path::Path::new("/t.luau");

        analysis.open(path, src);

        let hidden = analysis.hover(path, at(src, "map"), false).expect("a card");
        let shown = analysis.hover(path, at(src, "map"), true).expect("a card");

        assert!(!hidden.contains("{|"), "the marker leaked: {hidden}");
        assert!(
            shown.contains("{|"),
            "the marker did not come back: {shown}"
        );
    }

    /// Nothing under the cursor answers with nothing, rather than a guess.
    #[test]
    fn empty_space_hovers_nothing() {
        let src = "--!strict\nlocal x = 1\n\n\nreturn x\n";

        assert_eq!(card(src, src.len() as u32 - 1), None);
    }
}
