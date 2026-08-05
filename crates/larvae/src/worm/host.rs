/*!
The host half of the worm ABI, over `wasmi`.

wasm has no strings, so a source file crosses as an offset and a length into the
worm's own linear memory. The guest side of this lives in the `larvae-worm`
crate, which hides it behind a macro so a worm author never sees a pointer.

We run an interpreter rather than a JIT on purpose: `wasmtime` with Cranelift
measured at nearly three times the binary, and one artifact per worm plus a
sandbox around code fetched from a URL is what we were buying, not raw speed.
*/

use anyhow::{Context, Result, bail};

/// Guest exports we require, and the one alias we still accept
mod export {
    pub const MEMORY: &str = "memory";
    pub const ALLOC: &str = "larvae_alloc";
    pub const DEALLOC: &str = "larvae_dealloc";
    pub const TRANSFORM: &str = "larvae_transform";

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
pub struct WasmWorm {
    store: wasmi::Store<()>,
    memory: wasmi::Memory,
    alloc: wasmi::TypedFunc<u32, u32>,
    dealloc: wasmi::TypedFunc<(u32, u32), ()>,
    transform: wasmi::TypedFunc<(u32, u32, u32, u32), u32>,
}

impl WasmWorm {
    /// Compile and instantiate a worm module
    pub fn load(wasm: &[u8]) -> Result<Self> {
        let engine = wasmi::Engine::default();
        let module =
            wasmi::Module::new(&engine, wasm).context("worm is not a valid wasm module")?;
        let mut store = wasmi::Store::new(&engine, ());

        /*
        No imports are linked, deliberately. A worm that needs nothing from the
        host cannot reach a filesystem even by accident, which is what turns the
        sandbox from a policy into a property. Host functions get added here when
        the structured tier lands, and never WASI.
        */
        let linker = wasmi::Linker::new(&engine);

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
            .with_context(|| format!("worm exports no `{}`", export::TRANSFORM))?;

        Ok(Self {
            store,
            memory,
            alloc,
            dealloc,
            transform,
        })
    }

    /// Run the worm over one file, with its `[config.<name>]` table as TOML
    pub fn transform(&mut self, source: &str, config: &str) -> Result<Outcome> {
        let src = self.push(source.as_bytes())?;
        let cfg = self.push(config.as_bytes())?;

        let header = self
            .transform
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
    store: &wasmi::Store<()>,
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
