/*!
The host half of the worm ABI, over `wasmi`.

wasm has no strings. Thus a source file crosses the boundary as an offset and
a length into the linear memory of the worm. The guest side lives in the
`larvae-worm` crate. That crate hides the pointers behind a macro, so a worm
author does not see a pointer.

Larvae runs an interpreter and not a JIT, by intent. A measurement showed that
`wasmtime` with Cranelift makes the binary almost three times as large. The
goals are one artifact per worm and a sandbox around code fetched from a URL,
not raw speed.
*/

use std::sync::Arc;

use anyhow::{Context, Result, bail};

use super::ctx::FileCtx;
use super::proto;
use crate::rules::edits::Edit;

/// The guest exports the host requires, and the one alias the host still accepts
mod export {
    pub const MEMORY: &str = "memory";
    pub const ALLOC: &str = "larvae_alloc";
    pub const DEALLOC: &str = "larvae_dealloc";
    pub const TRANSFORM: &str = "larvae_transform";
    pub const INIT: &str = "larvae_init";
    pub const VISIT: &str = "larvae_visit";
    pub const FORMAT: &str = "larvae_format";
    pub const LINT: &str = "larvae_lint";
    pub const SETTINGS: &str = "larvae_settings";
    /// Code actions over a byte range. Optional: a worm that has none omits it.
    pub const ACTIONS: &str = "larvae_actions";
    /// Luau type definitions the worm supplies. Optional in the same way.
    pub const DEFINITIONS: &str = "larvae_definitions";

    /// The name the luaux prototype shipped before the ABI was stable.
    /// It is removed when api 1 freezes.
    pub const TRANSFORM_LEGACY: &str = "transform";
}

/// The header is `[out_ptr, out_len, ok]`, three little endian u32 values
const HEADER_BYTES: usize = 12;

/// A worm output is source text. An output near this size shows a bug, not a file.
const MAX_OUTPUT: u32 = 64 * 1024 * 1024;

/// The result a worm returned, which is output or the reason there is no output
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The transformed source when `ok` is true, or the diagnostic when it is false
    pub text: String,
    /// If true, `text` is output and not an error
    pub ok: bool,
}

impl Outcome {
    /// Return the transformed source, or the message of the worm as an error
    pub fn into_source(self) -> Result<String> {
        if self.ok {
            Ok(self.text)
        } else {
            bail!("{}", self.text)
        }
    }
}

/// A loaded wasm worm, ready for one call per file
/// The state the host functions reach while a rule runs
#[derive(Default)]
pub struct HostState {
    /// The file that larvae walks now. It is absent outside a rule.
    file: Option<Arc<FileCtx>>,
    /// The text a node accessor produced. The guest reads it back through larvae_take_str.
    scratch: Vec<u8>,
}

pub struct WasmWorm {
    store: wasmi::Store<HostState>,
    memory: wasmi::Memory,
    alloc: wasmi::TypedFunc<u32, u32>,
    dealloc: wasmi::TypedFunc<(u32, u32), ()>,
    transform: Option<wasmi::TypedFunc<(u32, u32, u32, u32), u32>>,
    init: Option<wasmi::TypedFunc<(u32, u32, u32, u32), ()>>,
    visit: Option<wasmi::TypedFunc<(u32, u64, u32), ()>>,
    format: Option<wasmi::TypedFunc<(u32, u32), u32>>,
    lint: Option<wasmi::TypedFunc<(u32, u32), u32>>,
    settings: Option<wasmi::TypedFunc<(u32, u32, u32, u32), ()>>,
    /// `(src_ptr, src_len, start, end) -> header`, absent where the worm has no actions
    actions: Option<wasmi::TypedFunc<(u32, u32, u32, u32), u32>>,
    /// `() -> header`, absent where the worm supplies no types
    definitions: Option<wasmi::TypedFunc<(), u32>>,
}

impl WasmWorm {
    /// Compile and instantiate a worm module
    pub fn load(wasm: &[u8]) -> Result<Self> {
        let engine = wasmi::Engine::default();
        let module =
            wasmi::Module::new(&engine, wasm).context("worm is not a valid wasm module")?;
        let mut store = wasmi::Store::new(&engine, HostState::default());

        /*
        The host supplies the only imports. There is no WASI, and there will be
        no WASI. Thus a worm cannot reach a filesystem, not even by accident.
        This makes the sandbox a property of the system, not only a policy.

        The node accessors are functions and not a shared view of the tree.
        The reason is the same as for the handles in the Luau form: the AST
        must stay free to change without breakage of every pinned worm.
        */
        let mut linker = wasmi::Linker::new(&engine);

        host_functions(&mut linker)?;

        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .context("worm failed to instantiate")?;

        let memory = instance
            .get_memory(&store, export::MEMORY)
            .with_context(|| format!("worm exports no `{}`", export::MEMORY))?;

        let alloc = typed(&instance, &store, export::ALLOC)?;
        let dealloc = typed(&instance, &store, export::DEALLOC)?;

        let transform = typed(&instance, &store, export::TRANSFORM)
            .or_else(|_| typed(&instance, &store, export::TRANSFORM_LEGACY))
            .ok();

        let init = typed(&instance, &store, export::INIT).ok();
        let visit = typed(&instance, &store, export::VISIT).ok();
        let format = typed(&instance, &store, export::FORMAT).ok();
        let lint = typed(&instance, &store, export::LINT).ok();
        let settings = typed(&instance, &store, export::SETTINGS).ok();
        let actions = typed(&instance, &store, export::ACTIONS).ok();
        let definitions = typed(&instance, &store, export::DEFINITIONS).ok();

        // a module with only a format or lint export is legal, because a
        // worm can report on its claimed files without a transform
        if transform.is_none() && visit.is_none() && format.is_none() && lint.is_none() {
            bail!(
                "worm exports none of `{}`, `{}`, `{}`, or `{}`, so it would never run",
                export::TRANSFORM,
                export::VISIT,
                export::FORMAT,
                export::LINT
            );
        }

        Ok(Self {
            store,
            memory,
            alloc,
            dealloc,
            transform,
            init,
            visit,
            format,
            lint,
            settings,
            actions,
            definitions,
        })
    }

    /// Run the worm over one file, with its `[worms.<name>.config]` table as TOML
    pub fn transform(&mut self, source: &str, config: &str) -> Result<Outcome> {
        let src = self.push(source.as_bytes())?;
        let cfg = self.push(config.as_bytes())?;

        let Some(transform) = self.transform else {
            bail!("worm has no frontend");
        };

        let header = transform
            .call(&mut self.store, (src.0, src.1, cfg.0, cfg.1))
            .context("worm trapped")?;

        // The guest does not need the inputs after it returns, so release them
        self.free(src)?;
        self.free(cfg)?;

        self.pull(header)
    }

    /*
    Ask the worm for the layout of one file.

    The reply crosses as JSON in the payload of the header, in the exact
    shape that [`proto::FormatReply`] deserializes. The ok flag of the header
    separates a reply from a diagnostic, so no JSON envelope repeats it.
    */
    pub fn format(&mut self, source: &str) -> Result<proto::FormatReply> {
        let Some(format) = self.format else {
            bail!("worm sets fmt = true but exports no `{}`", export::FORMAT);
        };

        let src = self.push(source.as_bytes())?;

        let header = format
            .call(&mut self.store, (src.0, src.1))
            .context("worm trapped")?;

        self.free(src)?;

        let text = self.pull(header)?.into_source()?;

        serde_json::from_str(&text).context("worm sent a reply we cannot read")
    }

    /// Ask the worm for the problems of one file. The host decides the severity.
    pub fn lint(&mut self, source: &str) -> Result<proto::LintReply> {
        let Some(lint) = self.lint else {
            bail!("worm declares lints but exports no `{}`", export::LINT);
        };

        let src = self.push(source.as_bytes())?;

        let header = lint
            .call(&mut self.store, (src.0, src.1))
            .context("worm trapped")?;

        self.free(src)?;

        let text = self.pull(header)?.into_source()?;

        serde_json::from_str(&text).context("worm sent a reply we cannot read")
    }

    /*
    The actions this worm offers over a byte range.

    A module without the export has none, which is not an error: the editor
    asks on a keystroke, and a worm that only formats has nothing to say here.
    */
    pub fn actions(&mut self, source: &str, span: (u32, u32)) -> Result<proto::ActionsReply> {
        let Some(actions) = self.actions else {
            return Ok(proto::ActionsReply::default());
        };

        let src = self.push(source.as_bytes())?;

        let header = actions
            .call(&mut self.store, (src.0, src.1, span.0, span.1))
            .context("worm trapped")?;

        self.free(src)?;

        let text = self.pull(header)?.into_source()?;

        serde_json::from_str(&text).context("worm sent a reply we cannot read")
    }

    /// The Luau type definitions this worm supplies, or none.
    pub fn definitions(&mut self) -> Result<proto::DefinitionsReply> {
        let Some(definitions) = self.definitions else {
            return Ok(proto::DefinitionsReply::default());
        };

        let header = definitions
            .call(&mut self.store, ())
            .context("worm trapped")?;

        let text = self.pull(header)?.into_source()?;

        serde_json::from_str(&text).context("worm sent a reply we cannot read")
    }

    /// Copy bytes into the guest and return their location
    fn push(&mut self, bytes: &[u8]) -> Result<(u32, u32)> {
        let len = u32::try_from(bytes.len()).context("input is larger than a worm can address")?;
        let ptr = self
            .alloc
            .call(&mut self.store, len)
            .context("worm trapped allocating")?;

        self.memory
            .write(&mut self.store, ptr as usize, bytes)
            .context("worm allocation does not fit its memory")?;

        Ok((ptr, len))
    }

    fn free(&mut self, (ptr, len): (u32, u32)) -> Result<()> {
        self.dealloc
            .call(&mut self.store, (ptr, len))
            .context("worm trapped freeing")?;

        Ok(())
    }

    /// Read `[ptr, len, ok]`, copy the payload out, then return the payload for release
    fn pull(&mut self, header: u32) -> Result<Outcome> {
        let mut raw = [0u8; HEADER_BYTES];

        self.memory
            .read(&self.store, header as usize, &mut raw)
            .context("worm returned a header outside its memory")?;

        let word =
            |i: usize| u32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().expect("4 bytes"));
        let (ptr, len, ok) = (word(0), word(1), word(2));

        if len > MAX_OUTPUT {
            bail!("worm returned {len} bytes, refusing");
        }

        let mut bytes = vec![0u8; len as usize];

        self.memory
            .read(&self.store, ptr as usize, &mut bytes)
            .context("worm returned a payload outside its memory")?;

        // The header is static on the guest side. The host releases only the payload.
        self.free((ptr, len))?;

        Ok(Outcome {
            text: String::from_utf8(bytes).context("worm returned bytes that are not utf-8")?,
            ok: ok == 1,
        })
    }
}

fn typed<P, R>(
    instance: &wasmi::Instance,
    store: &wasmi::Store<HostState>,
    name: &str,
) -> Result<wasmi::TypedFunc<P, R>>
where
    P: wasmi::WasmParams,
    R: wasmi::WasmResults,
{
    instance
        .get_typed_func(store, name)
        .with_context(|| format!("worm exports no `{name}` with the expected signature"))
}

/*
The node API, as imports under the `larvae` module.

A string does not cross as a return value, because a wasm function returns one
number. An accessor stages its text in the host scratch and returns a length.
The guest then allocates that many bytes and calls larvae_take_str to copy the
text over. This costs two calls instead of one. In return, the host needs no
allocator at the boundary, and the ownership of each buffer is clear.

Every accessor checks the epoch of the handle first. Thus a worm that uses a
node from a previous file gets a clear error and does not read the wrong tree.
*/
fn host_functions(linker: &mut wasmi::Linker<HostState>) -> Result<()> {
    /// The answer for a node that does not exist, distinct from a real zero
    const MISSING: i64 = -1;

    fn node<'a>(
        caller: &'a wasmi::Caller<'_, HostState>,
        epoch: u64,
        id: u32,
    ) -> Option<(&'a Arc<FileCtx>, &'a super::nodes::Node)> {
        let file = caller.data().file.as_ref()?;

        if file.epoch != epoch {
            return None;
        }

        file.table.get(id).map(|node| (file, node))
    }

    linker.func_wrap(
        "larvae",
        "node_kind_len",
        |caller: wasmi::Caller<HostState>, epoch: u64, id: u32| -> i64 {
            match node(&caller, epoch, id) {
                Some((_, n)) => n.kind.name().len() as i64,
                None => MISSING,
            }
        },
    )?;

    linker.func_wrap(
        "larvae",
        "node_kind",
        |mut caller: wasmi::Caller<HostState>, epoch: u64, id: u32| -> i64 {
            let Some((_, n)) = node(&caller, epoch, id) else {
                return MISSING;
            };

            let bytes = n.kind.name().as_bytes().to_vec();
            let len = bytes.len() as i64;
            caller.data_mut().scratch = bytes;

            len
        },
    )?;

    linker.func_wrap(
        "larvae",
        "node_text",
        |mut caller: wasmi::Caller<HostState>, epoch: u64, id: u32| -> i64 {
            let Some((file, _)) = node(&caller, epoch, id) else {
                return MISSING;
            };

            let bytes = file
                .table
                .text(id, &file.src)
                .unwrap_or_default()
                .as_bytes()
                .to_vec();
            let len = bytes.len() as i64;
            caller.data_mut().scratch = bytes;

            len
        },
    )?;

    linker.func_wrap(
        "larvae",
        "node_span_start",
        |caller: wasmi::Caller<HostState>, epoch: u64, id: u32| -> i64 {
            node(&caller, epoch, id).map_or(MISSING, |(_, n)| n.span.0 as i64)
        },
    )?;

    linker.func_wrap(
        "larvae",
        "node_span_end",
        |caller: wasmi::Caller<HostState>, epoch: u64, id: u32| -> i64 {
            node(&caller, epoch, id).map_or(MISSING, |(_, n)| n.span.1 as i64)
        },
    )?;

    linker.func_wrap(
        "larvae",
        "node_parent",
        |caller: wasmi::Caller<HostState>, epoch: u64, id: u32| -> i64 {
            match node(&caller, epoch, id) {
                Some((_, n)) => n.parent.map_or(MISSING, |p| p as i64),
                None => MISSING,
            }
        },
    )?;

    linker.func_wrap(
        "larvae",
        "node_child_count",
        |caller: wasmi::Caller<HostState>, epoch: u64, id: u32| -> i64 {
            node(&caller, epoch, id).map_or(MISSING, |(_, n)| n.children.len() as i64)
        },
    )?;

    linker.func_wrap(
        "larvae",
        "node_child",
        |caller: wasmi::Caller<HostState>, epoch: u64, id: u32, index: u32| -> i64 {
            match node(&caller, epoch, id) {
                Some((_, n)) => n
                    .children
                    .get(index as usize)
                    .map_or(MISSING, |&c| c as i64),
                None => MISSING,
            }
        },
    )?;

    // copy the staged string into guest memory and return the number of bytes written
    linker.func_wrap(
        "larvae",
        "take_str",
        |mut caller: wasmi::Caller<HostState>, ptr: u32, len: u32| -> i64 {
            let staged = std::mem::take(&mut caller.data_mut().scratch);
            let n = staged.len().min(len as usize);

            let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") else {
                return MISSING;
            };

            match memory.write(&mut caller, ptr as usize, &staged[..n]) {
                Ok(()) => n as i64,
                Err(_) => MISSING,
            }
        },
    )?;

    linker.func_wrap(
        "larvae",
        "replace",
        |caller: wasmi::Caller<HostState>, epoch: u64, id: u32, ptr: u32, len: u32| -> i64 {
            let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") else {
                return MISSING;
            };

            let mut bytes = vec![0u8; len as usize];

            if memory.read(&caller, ptr as usize, &mut bytes).is_err() {
                return MISSING;
            }

            let Ok(text) = String::from_utf8(bytes) else {
                return MISSING;
            };

            let Some(file) = caller.data().file.clone() else {
                return MISSING;
            };

            match file.check(epoch).and_then(|()| file.replace(id, text)) {
                Ok(()) => 0,
                Err(_) => MISSING,
            }
        },
    )?;

    linker.func_wrap(
        "larvae",
        "remove",
        |caller: wasmi::Caller<HostState>, epoch: u64, id: u32| -> i64 {
            let Some(file) = caller.data().file.clone() else {
                return MISSING;
            };

            match file.check(epoch).and_then(|()| file.remove(id)) {
                Ok(()) => 0,
                Err(_) => MISSING,
            }
        },
    )?;

    Ok(())
}

impl WasmWorm {
    /*
    Give the settings to the worm once, before the worm sees a file.

    The config and rules cross as TOML text. The Luau form gets real tables
    instead, because each form gets the shape that is natural for it. The
    project settings cross as JSON text through `larvae_settings`, directly
    after `larvae_init`, and only when the module exports the function. Thus
    an old module runs unchanged.
    */
    pub fn init(&mut self, config: &str, rules: &str, settings: &super::Settings) -> Result<()> {
        if let Some(init) = self.init {
            let cfg = self.push(config.as_bytes())?;
            let rls = self.push(rules.as_bytes())?;

            init.call(&mut self.store, (cfg.0, cfg.1, rls.0, rls.1))
                .context("worm trapped during init")?;

            self.free(cfg)?;
            self.free(rls)?;
        }

        if let Some(accept) = self.settings {
            let fmt = self.push(settings.fmt.as_bytes())?;
            let lint = self.push(settings.lint.as_bytes())?;

            accept
                .call(&mut self.store, (fmt.0, fmt.1, lint.0, lint.1))
                .context("worm trapped during init")?;

            self.free(fmt)?;
            self.free(lint)?;
        }

        Ok(())
    }

    /// Report if this worm implements the rule half
    pub fn has_rules(&self) -> bool {
        self.visit.is_some()
    }

    /// Run one rule over every node that its filter matched
    pub fn run_rule(
        &mut self,
        rule: u32,
        file: Arc<FileCtx>,
        matched: &[u32],
    ) -> Result<Vec<Edit>> {
        let Some(visit) = self.visit else {
            bail!("worm has no rules");
        };

        let epoch = file.epoch;

        self.store.data_mut().file = Some(Arc::clone(&file));

        let result = (|| -> Result<()> {
            for &id in matched {
                visit
                    .call(&mut self.store, (rule, epoch, id))
                    .context("worm trapped")?;
            }

            Ok(())
        })();

        self.store.data_mut().file = None;
        result?;

        Ok(file.take_edits())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/echo_worm.wasm");

    /*
    A cleared func field is the exact state of a module without the export,
    because `load` stores `None` when `get_typed_func` finds nothing. The
    override spares the repository a second wasm artifact.
    */
    #[test]
    fn a_promised_format_without_the_export_is_a_clear_error() {
        let mut worm = WasmWorm::load(FIXTURE).unwrap();
        worm.format = None;

        let err = worm.format("x").unwrap_err();
        let text = format!("{err:#}");

        assert!(text.contains("fmt = true"), "{text}");
        assert!(text.contains("larvae_format"), "{text}");
    }

    /*
    The editor paths differ from format and lint: a module without the export
    answers with nothing rather than failing.

    The manifest promises `fmt` and `[lints]`, so a missing export there is a
    broken promise worth a message. Nothing promises actions or definitions,
    and the editor asks for them on a keystroke, so absence is the normal case.
    */
    #[test]
    fn a_module_without_the_editor_exports_answers_with_nothing() {
        let mut worm = WasmWorm::load(FIXTURE).unwrap();
        worm.actions = None;
        worm.definitions = None;

        assert!(worm.actions("x", (0, 1)).unwrap().actions.is_empty());
        assert!(worm.definitions().unwrap().definitions.is_empty());
    }

    #[test]
    fn promised_lints_without_the_export_are_a_clear_error() {
        let mut worm = WasmWorm::load(FIXTURE).unwrap();
        worm.lint = None;

        let err = worm.lint("x").unwrap_err();
        let text = format!("{err:#}");

        assert!(text.contains("declares lints"), "{text}");
        assert!(text.contains("larvae_lint"), "{text}");
    }
}
