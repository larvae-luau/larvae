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

/*
The Roblox reference, trimmed to what a card shows and deflated.

Luau names a page per type and per member, ex: `@roblox/globaltype/Player`,
and the frontend hands that name back for a hover or a completion. This is
the other half: the name, to the prose and the link.

It ships compressed, because the trimmed database is 3.7MB of JSON and 438KB
deflated. It inflates on the thread that builds the session, next to the
fourteen thousand lines of type definitions, so it costs the editor nothing.
Only the entries that carry prose or a link are kept; the parameter entries
of the full file say nothing a reader needs.
*/
const API_DOCS: &[u8] = include_bytes!("../types/api-docs.deflate");

/// One page of the Roblox reference, as the trimmed database spells it
#[derive(serde::Deserialize)]
struct DocEntry {
    #[serde(default, rename = "d")]
    documentation: String,
    #[serde(default, rename = "l")]
    link: String,
    /// The example the reference prints under the page
    #[serde(default, rename = "c")]
    code_sample: String,
}

impl DocEntry {
    /// The page as markdown, in the shape luau-lsp writes one
    fn markdown(&self) -> Option<String> {
        let mut out = self.documentation.clone();

        if !self.link.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }

            out.push_str(&format!("[Learn More]({})", self.link));
        }

        if !self.code_sample.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }

            out.push_str(&format!("```luau\n{}\n```", self.code_sample));
        }

        (!out.is_empty()).then_some(out)
    }
}

/*
Inflate the reference into a map, once.

A failure here is not worth a message. The database ships with the binary,
so a failure means the binary is damaged, and every other answer still holds
without it.
*/
fn read_api_docs() -> HashMap<String, DocEntry> {
    use std::io::Read;

    let mut text = String::new();

    if flate2::read::ZlibDecoder::new(API_DOCS)
        .read_to_string(&mut text)
        .is_err()
    {
        return HashMap::new();
    }

    serde_json::from_str(&text).unwrap_or_default()
}

use larvae::lsp::analysis::{Analysis, AnalysisCompletion, AnalysisDiag, ModuleHooks};

#[repr(C)]
struct RawDiag {
    start: u32,
    end: u32,
    /// Luau's own error number; 0 where the finding carries none
    code: i32,
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
    /// The type of the entry, rendered; null for a keyword
    detail: *const c_char,
    /// The argument names of a function, ex: `(self, className)`
    label_detail: *const c_char,
    /// What the editor writes, when it differs from the label
    insert_text: *const c_char,
    /// The comment block above the declaration, as markdown; null when none
    documentation: *const c_char,
    /// The entry's page in the Roblox reference; null when it names none
    documentation_symbol: *const c_char,
    kind: u8,
    deprecated: u8,
    /// 0 no, 1 the entry fits the expected type, 2 its result does
    type_correct: u8,
    /// 1 when the entry comes through an index the type does not take
    wrong_index_type: u8,
}

#[allow(non_camel_case_types)]
type larvae_resolve_fn = extern "C" fn(*mut c_void, *const c_char, *const c_char) -> *const c_char;
#[allow(non_camel_case_types)]
type larvae_load_fn = extern "C" fn(*mut c_void, *const c_char) -> *const c_char;

unsafe extern "C" {
    fn larvae_enable_all_flags();
    fn larvae_set_flag(name: *const c_char, value: *const c_char) -> i32;
    fn larvae_apply_required_flags();
    fn larvae_reset_flags();
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
    fn larvae_clear_script_types(s: *mut c_void);
    fn larvae_set_script_type(s: *mut c_void, path: *const c_char, type_name: *const c_char);
    fn larvae_check(s: *mut c_void, path: *const c_char, out: *mut RawDiag, cap: usize) -> usize;
    fn larvae_hover(
        s: *mut c_void,
        path: *const c_char,
        byte: u32,
        show_table_kinds: i32,
        include_string_length: i32,
    ) -> *const c_char;
    fn larvae_documentation_symbol(s: *mut c_void, path: *const c_char, byte: u32)
    -> *const c_char;
    fn larvae_bytecode(
        s: *mut c_void,
        source: *const c_char,
        optimization: i32,
        remarks: i32,
        debug_level: i32,
        type_info_level: i32,
        vector_lib: *const c_char,
        vector_ctor: *const c_char,
        vector_type: *const c_char,
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
    /*
    The extensions of the worms that resolve, without the dot.

    They come with the hooks and travel with them, because the two answer
    one question together: the worm resolves the spec, and this says which
    files it is able to.
    */
    claims: Vec<String>,
}

/// One string the shim handed back, or None where it handed back null
fn text(raw: *const c_char) -> Option<String> {
    match raw.is_null() {
        true => None,

        false => Some(
            unsafe { CStr::from_ptr(raw) }
                .to_string_lossy()
                .into_owned(),
        ),
    }
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
    let hooked = state
        .hooks
        .as_ref()
        .and_then(|h| (h.resolve)(Path::new(from.as_ref()), &spec));

    if std::env::var_os("LARVAE_RESOLVE_DEBUG").is_some() {
        eprintln!(
            "hook {:?} from {:?} -> {:?} (hooks installed: {})",
            spec,
            from.as_ref(),
            hooked,
            state.hooks.is_some()
        );
    }

    if let Some(path) = hooked {
        state.resolve_buffer = CString::new(path).ok();

        return state
            .resolve_buffer
            .as_ref()
            .map_or(std::ptr::null(), |c| c.as_ptr());
    }

    let answer = resolve_spec(
        Path::new(from.as_ref()),
        &spec,
        state.mounts.as_ref(),
        &state.claims,
    );

    if std::env::var_os("LARVAE_RESOLVE_DEBUG").is_some() {
        eprintln!(
            "resolve {:?} from {:?} -> {:?}",
            spec,
            from.as_ref(),
            answer
        );
    }

    match answer {
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
        if std::env::var_os("LARVAE_RESOLVE_DEBUG").is_some() {
            eprintln!(
                "load {:?} -> {} bytes:\n{}",
                path.as_ref(),
                text.len(),
                &text[..text.len().min(400)]
            );
        }

        state.load_buffer = CString::new(text).ok();

        return state
            .load_buffer
            .as_ref()
            .map_or(std::ptr::null(), |c| c.as_ptr());
    }

    /*
    Only Luau is read from disk. A claimed file reaches the frontend through
    the worm above, and a worm that declined has nothing to say about it, so
    the raw text would be read as Luau and report its first brace as a
    syntax error inside a file the author cannot see.
    */
    let luau = Path::new(path.as_ref())
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext == "luau" || ext == "lua");

    if !luau {
        return std::ptr::null();
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
/*
Luau's own flags, applied to this process.

The flags are global to the process and not to a session, and some of them
decide what a session is: `LuauSolverV2` picks the type solver, and the
globals are registered under whichever one was on when the session was
built. So this runs before `LuauAnalysis::new`, on the thread that builds
it, and a project that asks for the new solver gets one.

The order is luau-lsp's: every flag on, then the project's overrides, then
the values larvae cannot work without. A later step wins. The names Luau did
not recognise come back, because a flag Luau renamed is a setting that
quietly stopped working and only the user can fix it.
*/
pub fn apply_flags(flags: &larvae::config::lsp::FFlagsConfig) -> Vec<String> {
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

pub struct LuauAnalysis {
    session: *mut c_void,
    /// Owned by the session for its lifetime; the shim only borrows it
    resolver: Box<ResolverState>,
    /// Path strings the session knows, so invalidate spells them the same way
    keys: HashMap<PathBuf, CString>,
    /// The service names, extracted from the definitions once
    services: Vec<String>,
    /// The Roblox reference, by documentation symbol
    docs: HashMap<String, DocEntry>,
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
            claims: Vec::new(),
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
            docs: read_api_docs(),
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
        apply_flags(flags)
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
        self.resolver.claims = hooks.claims.clone();
        self.resolver.hooks = Some(hooks);
    }

    fn definitions(&mut self, name: &str, source: &str) -> bool {
        let (Ok(name), Ok(source)) = (CString::new(name), CString::new(source)) else {
            return false;
        };

        unsafe { larvae_set_definitions(self.session, name.as_ptr(), source.as_ptr()) == 0 }
    }

    fn set_script_types(&mut self, types: &HashMap<PathBuf, String>) {
        unsafe { larvae_clear_script_types(self.session) };

        for (path, name) in types {
            let Ok(name) = CString::new(name.as_str()) else {
                continue;
            };

            let key = self.key(path);

            unsafe { larvae_set_script_type(self.session, key, name.as_ptr()) };
        }
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
                code: (d.code != 0).then(|| d.code.to_string()),
            })
            .collect()
    }

    fn hover(
        &mut self,
        path: &Path,
        at: u32,
        show_table_kinds: bool,
        include_string_length: bool,
    ) -> Option<String> {
        let key = self.key(path);

        let text = unsafe {
            larvae_hover(
                self.session,
                key,
                at,
                show_table_kinds as i32,
                include_string_length as i32,
            )
        };

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

        // The borrow of the map ends before the closure takes `self` again.
        let docs = &self.docs;

        raw.iter()
            .map(|c| AnalysisCompletion {
                label: unsafe { CStr::from_ptr(c.label) }
                    .to_string_lossy()
                    .into_owned(),
                kind: c.kind,
                detail: text(c.detail),
                label_detail: text(c.label_detail),
                insert_text: text(c.insert_text),
                /*
                The comment the author wrote wins over the reference. A
                project that documents its own wrapper of a Roblox call
                means the wrapper, and the reference means the call.
                */
                documentation: text(c.documentation).or_else(|| {
                    text(c.documentation_symbol)
                        .and_then(|symbol| docs.get(&symbol))
                        .and_then(DocEntry::markdown)
                }),
                deprecated: c.deprecated == 1,
                type_correct: c.type_correct,
                wrong_index_type: c.wrong_index_type == 1,
            })
            .collect()
    }

    /*
    The Roblox reference page for the name at a position, as markdown.

    The frontend names the page and this reads it. A name the reference does
    not cover answers nothing, which is every name a project wrote itself.
    */
    fn hover_documentation(&mut self, path: &Path, at: u32) -> Option<String> {
        let key = self.key(path);
        let symbol = text(unsafe { larvae_documentation_symbol(self.session, key, at) })?;

        self.docs.get(&symbol).and_then(DocEntry::markdown)
    }

    /*
    The compiled form of one source text, as the editor shows it.

    The compiler is self-contained, so nothing here touches the module graph
    or the open documents: the text arrives already lowered and leaves as a
    listing. Source that does not compile answers with the error text, which
    is what luau-lsp puts in the same panel.
    */
    fn bytecode(
        &mut self,
        source: &str,
        optimization: u8,
        remarks: bool,
        config: &larvae::config::lsp::BytecodeConfig,
    ) -> Option<String> {
        let text_of = |value: &str| CString::new(value).unwrap_or_default();

        let source = text_of(source);
        let lib = text_of(&config.vector_lib);
        let ctor = text_of(&config.vector_ctor);
        let vector = text_of(&config.vector_type);

        let listing = unsafe {
            larvae_bytecode(
                self.session,
                source.as_ptr(),
                optimization as i32,
                remarks as i32,
                config.debug_level as i32,
                config.type_info_level as i32,
                lib.as_ptr(),
                ctor.as_ptr(),
                vector.as_ptr(),
            )
        };

        text(listing).filter(|listing| !listing.is_empty())
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

    /*
    A flag put back when the test that changed it ends.

    Luau keeps its flags in the process and not in a session, so a test that
    turns them on decides what every later test in the same binary infers.
    Every hover test failed for that reason, and only when the whole suite
    ran. The guard puts them back however the test leaves.
    */
    struct Flags;

    impl Drop for Flags {
        fn drop(&mut self) {
            unsafe { larvae_reset_flags() };
        }
    }

    /// An override reaches Luau, and a bad name comes back rather than vanishing.
    #[test]
    fn an_override_reaches_luau_and_a_typo_is_reported() {
        let _flags = Flags;
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
        let _flags = Flags;
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
        let _flags = Flags;
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
        analysis.hover(path, at, false, false)
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

        let hidden = analysis
            .hover(path, at(src, "map"), false, false)
            .expect("a card");
        let shown = analysis
            .hover(path, at(src, "map"), true, false)
            .expect("a card");

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

#[cfg(test)]
mod luau_lsp_parity {
    use super::*;
    use larvae::lsp::analysis::Analysis;

    /*
    These pin the shapes a differential run against luau-lsp found.

    The run hovered the same positions in a real Roblox project through both
    servers and compared the rendered types. Each case below is a shape that
    differed, and the expectation is what luau-lsp answers.
    */
    fn card(src: &str, word: &str) -> String {
        let mut analysis = LuauAnalysis::new();
        let path = std::path::Path::new("/t.luau");

        analysis.open(path, src);
        analysis
            .hover(path, src.find(word).expect("the word") as u32, false, true)
            .unwrap_or_default()
    }

    /// A service comes back as itself, not as the `Instance` the signature says.
    #[test]
    fn get_service_answers_with_the_service() {
        let src = "local Players = game:GetService(\"Players\")\nreturn Players\n";

        assert_eq!(card(src, "Players"), "local Players: Players");
    }

    /// A method is named for the type it hangs off, with no `self` in the list.
    #[test]
    fn a_method_reads_as_the_type_that_has_it() {
        let src = "local p = Instance.new(\"Part\")\np:Destroy()\n";

        assert_eq!(card(src, "Destroy"), "function Instance:Destroy(): nil");
    }

    /// A field keeps the path the author wrote, so the card says where it came from.
    #[test]
    fn a_field_keeps_its_path() {
        let src = "local x = math.cos(1)\nreturn x\n";

        assert_eq!(card(src, "cos"), "function math.cos(n: number): number");
    }

    /// A type in a return pack resolves, which needed the visitor to enter the pack.
    #[test]
    fn a_return_type_resolves() {
        let src = "--!strict\nlocal function g(a: number): (number, string)\n\treturn a, \"x\"\nend\nreturn g\n";
        let at = src.rfind("string").expect("the return type") as u32;

        let mut analysis = LuauAnalysis::new();
        let path = std::path::Path::new("/t.luau");

        analysis.open(path, src);

        assert_eq!(
            analysis.hover(path, at, false, true).as_deref(),
            Some("type string = string")
        );
    }

    /// A string literal says how long it is, which is the part a reader cannot count.
    #[test]
    fn a_string_literal_says_its_length() {
        let src = "local s = \"Loaded\"\nreturn s\n";

        assert_eq!(card(src, "\"Loaded\""), "string (6 bytes)");
    }

    /// A global says it is a type, so a table of constructors is not read as a value.
    #[test]
    fn a_global_reads_as_a_type() {
        let src = "local c = Color3\nreturn c\n";

        assert!(
            card(src, "Color3").starts_with("type Color3 ="),
            "{}",
            card(src, "Color3")
        );
    }
}

#[cfg(test)]
mod bytecode_listing {
    use super::*;
    use larvae::config::lsp::BytecodeConfig;
    use larvae::lsp::analysis::Analysis;

    /*
    The listing of one source, at one optimization level.

    The session is the caller's, because building one loads the Roblox
    types and a test that builds four waits four times for nothing: the
    compiler reads the text and no session state takes part.
    */
    fn listing(analysis: &mut LuauAnalysis, src: &str, optimization: u8) -> String {
        analysis
            .bytecode(src, optimization, false, &BytecodeConfig::default())
            .expect("a listing")
    }

    /*
    A function lists its opcodes, under the name the compiler gave it.

    The header, the source line, and the instruction are the three things
    luau-lsp's panel shows, and all three come from one dump.
    */
    #[test]
    fn a_function_lists_its_opcodes() {
        let mut analysis = LuauAnalysis::new();
        let text = listing(
            &mut analysis,
            "local function add(a, b)\n\treturn a + b\nend\nreturn add\n",
            1,
        );

        assert!(text.contains("Function 0 (add):"), "{text}");
        assert!(text.contains("ADD R2 R0 R1"), "{text}");
        assert!(
            text.contains("\t return a + b") || text.contains("return a + b"),
            "{text}"
        );
    }

    /*
    The optimization level reaches the compiler, so the listing changes.

    `1 + 2 * 3` is three instructions at O0 and the number 7 at O1, which is
    the difference a reader opens the panel to see.
    */
    #[test]
    fn the_optimization_level_changes_the_listing() {
        let mut analysis = LuauAnalysis::new();
        let src = "local x = 1 + 2 * 3\nreturn x\n";

        let none = listing(&mut analysis, src, 0);
        let full = listing(&mut analysis, src, 2);

        assert!(none.contains("MUL"), "{none}");
        assert!(!full.contains("MUL"), "{full}");
        assert!(full.contains("LOADN R0 7"), "{full}");
    }

    /*
    The remarks view is the source, with what the compiler decided above the
    line it decided it on. It is a different answer from the same compile.
    */
    #[test]
    fn the_remarks_view_annotates_the_source() {
        let src = "local t = {}\nfor i = 1, 10 do\n\tt[i] = i * i\nend\nreturn t\n";

        let remarks = LuauAnalysis::new()
            .bytecode(src, 2, true, &BytecodeConfig::default())
            .expect("a view");

        assert!(
            remarks.contains("-- remark: loop unroll succeeded"),
            "{remarks}"
        );
        assert!(remarks.contains("for i = 1, 10 do"), "{remarks}");
        assert!(!remarks.contains("RETURN"), "{remarks}");
    }

    /*
    Source that does not compile says why, in the line luau-lsp writes: the
    kind, the one based position, then what the parser wanted.
    */
    #[test]
    fn a_source_that_does_not_compile_says_why() {
        let mut analysis = LuauAnalysis::new();
        let text = listing(&mut analysis, "local x =\n", 2);

        assert!(text.starts_with("SyntaxError(2,1): "), "{text}");
        assert!(text.contains("Expected identifier"), "{text}");
    }

    /*
    The vector configuration reaches the compiler.

    `Vector3.new(1, 2, 3)` folds to one constant only because the project
    named `Vector3` as its vector library. A project that names none keeps
    the call, and the two listings prove the setting travelled.
    */
    #[test]
    fn the_vector_configuration_reaches_the_compiler() {
        let mut analysis = LuauAnalysis::new();
        let src = "local v = Vector3.new(1, 2, 3)\nreturn v\n";

        let folded = listing(&mut analysis, src, 2);

        let plain = analysis
            .bytecode(
                src,
                2,
                false,
                &BytecodeConfig {
                    vector_lib: String::new(),
                    vector_ctor: String::new(),
                    vector_type: String::new(),
                    ..BytecodeConfig::default()
                },
            )
            .expect("a listing");

        assert!(folded.contains("[1, 2, 3]"), "{folded}");
        assert!(!plain.contains("[1, 2, 3]"), "{plain}");
    }
}

#[cfg(test)]
mod statement_names {
    use super::*;
    use larvae::lsp::analysis::Analysis;

    /*
    The shapes below are what luau-lsp answers for the same source, read off
    the real server. A function statement writes its name through the type
    it hangs off, which is the alias in a module written this way.
    */
    const MODULE: &str = "--!strict\nlocal M = {}\n\nfunction M.Init(self: Self, n: number)\n\treturn n\nend\n\nfunction M:Bump(by: number): number\n\treturn by\nend\n\ntype Self = typeof(M)\ntype Entry = { value: any, next: Entry? }\ntype Stat = \"Strength\" | \"Walkspeed\"\n\nreturn M\n";

    fn card(src: &str, word: &str) -> String {
        let mut analysis = LuauAnalysis::new();
        let path = std::path::Path::new("/t.luau");

        analysis.open(path, src);
        analysis
            .hover(path, src.find(word).expect("the word") as u32, false, true)
            .unwrap_or_default()
    }

    /// The dot form names the type the function hangs off, and keeps `self`.
    #[test]
    fn a_function_statement_names_the_type_it_hangs_off() {
        assert_eq!(
            card(MODULE, "Init"),
            "function Self.Init(self: Self, n: number): number"
        );
    }

    /// The colon form keeps its colon and drops the receiver from the list.
    #[test]
    fn a_method_statement_hides_the_receiver() {
        assert_eq!(
            card(MODULE, "Bump"),
            "function Self:Bump<a>(by: number): number"
        );
    }

    /// The `type` keyword hovers the alias it opens, and not nothing.
    #[test]
    fn the_type_keyword_hovers_its_alias() {
        assert_eq!(
            card(MODULE, "type Stat"),
            "type Stat = \"Strength\" | \"Walkspeed\""
        );
    }

    /// A property name inside a table type answers with what the property holds.
    #[test]
    fn a_property_name_in_a_table_type_answers() {
        assert_eq!(card(MODULE, "value: any"), "any");
    }

    /// A literal inside a type answers with itself, and not with the union.
    #[test]
    fn a_literal_inside_a_type_answers_with_itself() {
        assert_eq!(card(MODULE, "\"Strength\""), "\"Strength\"");
    }
}

#[cfg(test)]
mod startup_cost {
    use super::*;
    use larvae::lsp::analysis::Analysis;

    /// What a session costs to build, which is what the editor waits for.
    #[test]
    #[ignore = "timing, run explicitly"]
    fn what_a_session_costs() {
        let session = std::time::Instant::now();
        let mut analysis = LuauAnalysis::new();
        println!("  session with definitions: {:?}", session.elapsed());

        let again = std::time::Instant::now();
        analysis.definitions("@second", "declare _probe: number\n");
        println!("  a tiny second load:       {:?}", again.elapsed());
    }
}
