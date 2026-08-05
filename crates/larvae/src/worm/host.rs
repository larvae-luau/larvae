/*!
The host half of the worm ABI, over `wasmi`.

wasm has no strings, so a source file crosses as an offset and a length into the
worm's own linear memory. The guest side of this lives in the `larvae-worm`
crate, which hides it behind a macro so a worm author never sees a pointer.

We run an interpreter rather than a JIT on purpose: `wasmtime` with Cranelift
measured at nearly three times the binary, and one artifact per worm plus a
sandbox around code fetched from a URL is what we were buying, not raw speed.
*/

use std::sync::Arc;

use anyhow::{Context, Result, bail};

use super::ctx::FileCtx;
use crate::rules::edits::Edit;

/// Guest exports we require, and the one alias we still accept
mod export {
    pub const MEMORY: &str = "memory";
    pub const ALLOC: &str = "larvae_alloc";
    pub const DEALLOC: &str = "larvae_dealloc";
    pub const TRANSFORM: &str = "larvae_transform";
    pub const INIT: &str = "larvae_init";
    pub const VISIT: &str = "larvae_visit";

    /// The name luaux's prototype shipped before the ABI settled, dropped once api 1 freezes
    pub const TRANSFORM_LEGACY: &str = "transform";
}

/// `[out_ptr, out_len, ok]`, three little endian u32
const HEADER_BYTES: usize = 12;

/// A worm output is source text, so anything near this is a bug rather than a file
const MAX_OUTPUT: u32 = 64 * 1024 * 1024;

/// What a worm returned, which is either output or the reason there is none
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Transformed source when `ok`, the diagnostic when not
    pub text: String,
    /// Whether `text` is output rather than an error
    pub ok: bool,
}

impl Outcome {
    /// The transformed source, or the worm's own message as an error
    pub fn into_source(self) -> Result<String> {
        if self.ok {
            Ok(self.text)
        } else {
            bail!("{}", self.text)
        }
    }
}

/// A loaded wasm worm, ready to be called once per file
/// What the host functions reach while a rule is running
#[derive(Default)]
pub struct HostState {
    /// The file being walked, absent outside a rule
    file: Option<Arc<FileCtx>>,
    /// Text a node accessor produced, read back through larvae_take_str
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
}

impl WasmWorm {
    /// Compile and instantiate a worm module
    pub fn load(wasm: &[u8]) -> Result<Self> {
        let engine = wasmi::Engine::default();
        let module =
            wasmi::Module::new(&engine, wasm).context("worm is not a valid wasm module")?;
        let mut store = wasmi::Store::new(&engine, HostState::default());

        /*
        The only imports are ours. There is no WASI and never will be, so a worm
        cannot reach a filesystem even by accident, which is what turns the
        sandbox from a policy into a property rather than a promise.

        Node accessors are functions rather than a shared view of the tree, for
        the same reason the Luau form uses handles: our AST must stay free to
        change without breaking every pinned worm.
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

        if transform.is_none() && visit.is_none() {
            bail!(
                "worm exports neither `{}` nor `{}`, so it would never run",
                export::TRANSFORM,
                export::VISIT
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
        })
    }

    /// Run the worm over one file, with its `[config.<name>]` table as TOML
    pub fn transform(&mut self, source: &str, config: &str) -> Result<Outcome> {
        let src = self.push(source.as_bytes())?;
        let cfg = self.push(config.as_bytes())?;

        let Some(transform) = self.transform else {
            bail!("worm has no frontend");
        };

        let header = transform
            .call(&mut self.store, (src.0, src.1, cfg.0, cfg.1))
            .context("worm trapped")?;

        // The guest is done with the inputs once it has returned, so release them
        self.free(src)?;
        self.free(cfg)?;

        self.pull(header)
    }

    /// Copy bytes into the guest and return where they landed
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

    /// Read `[ptr, len, ok]`, copy the payload out, then hand it back to be freed
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

        // The header is static on the guest side, only the payload is ours to release
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

Strings never cross as return values, since a wasm function returns one number.
An accessor stages its text in host scratch and answers with a length, and the
guest allocates that many bytes and calls larvae_take_str to copy it over. Two
calls instead of one, but no allocator on our side of the boundary and no
ownership question about who frees what.

Every accessor checks the handle's epoch first, so a worm reaching for a node
from a previous file traps with a sentence rather than reading the wrong tree.
*/
fn host_functions(linker: &mut wasmi::Linker<HostState>) -> Result<()> {
    /// Answer for a node that is not there, distinct from a real zero
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

    // copy the staged string into guest memory, returning how much was written
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
    /// Hand over settings once, before any file is seen
    ///
    /// Both blobs cross as TOML text. The Luau form gets real tables instead,
    /// because each form should get what is natural for it.
    pub fn init(&mut self, config: &str, rules: &str) -> Result<()> {
        let Some(init) = self.init else {
            return Ok(());
        };

        let cfg = self.push(config.as_bytes())?;
        let rls = self.push(rules.as_bytes())?;

        init.call(&mut self.store, (cfg.0, cfg.1, rls.0, rls.1))
            .context("worm trapped during init")?;

        self.free(cfg)?;
        self.free(rls)?;

        Ok(())
    }

    /// Whether this worm implements the rule half
    pub fn has_rules(&self) -> bool {
        self.visit.is_some()
    }

    /// Run one rule over every node its filter matched
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
