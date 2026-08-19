/*!
The Luau guest form, over `mlua`.

No data crosses a memory boundary here. mlua marshals strings natively. Thus a
Luau worm is an ordinary function, and the ptr/len protocol in [`super::host`]
has no equivalent. This is the purpose of the form: a small transform that an
author writes quickly must not need a toolchain or a `wasm32` target.

```lua
return {
    frontend = {
        compile = function(source, config)
            return source:gsub("<>", "{}")
        end,
    },
}
```
*/

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use mlua::LuaSerdeExt;

use super::Outcome;
use super::ctx::FileCtx;
use super::proto;
use crate::rules::edits::Edit;

mod convert;
mod nodes;

use convert::{json_to_lua, to_lua, to_lua_opt, worm_message};
use nodes::{NodeRef, edit_remove, edit_replace};

/// Larvae checks every N VM instructions and stops a worm that runs too long. Thus a
/// worm that does not stop cannot hang the build.
const INTERRUPT_EVERY: u32 = 1_000_000;

/// The memory limit for one worm VM. It is large for a transform and stops a leak.
const MEMORY_LIMIT: usize = 256 * 1024 * 1024;

/// A loaded Luau worm
pub struct LuauWorm {
    /// The worm owns the VM that `compile` came from, so each item the worm needs stays alive
    #[allow(dead_code)]
    lua: mlua::Lua,
    /// The name of the worm, so an error about a missing function names it
    name: String,
    /// The front-end entry point. It is absent when the worm supplies only rules.
    compile: Option<mlua::Function>,
    /// Optional. It returns the layout of a claimed file for `larvae fmt`.
    format: Option<mlua::Function>,
    /// Optional. It returns the findings of a claimed file for `larvae lint`.
    lint: Option<mlua::Function>,
    /// `frontend.actions`, absent where the worm offers none
    actions: Option<mlua::Function>,
    /// `frontend.definitions`, absent where the worm supplies no types
    definitions: Option<mlua::Function>,
    /// The `visit` function for each rule the worm declared, keyed by rule name
    rules: BTreeMap<String, mlua::Function>,
    /// Optional. Larvae calls it once with the settings before the worm sees a file.
    init: Option<mlua::Function>,
    /// The interrupt budget. A reset before each call makes the limit apply
    /// per file, not per build.
    budget: Arc<AtomicU32>,
}

impl LuauWorm {
    /// Evaluate a worm module and take its front-end entry point
    pub fn load(source: &str, chunk_name: &str) -> Result<Self> {
        let lua = mlua::Lua::new();

        lua.set_memory_limit(MEMORY_LIMIT)
            .context("could not bound the worm's memory")?;

        /*
        This is the sandbox of Luau itself. It freezes the globals and gives
        each chunk a new environment. The sandbox enforces purity and does not
        only request it. For this reason the Luau form does not need the
        import-table argument that the wasm form uses.
        */
        lua.sandbox(true).context("could not sandbox the worm")?;

        let exports: mlua::Table = lua
            .load(source)
            .set_name(chunk_name)
            .eval()
            .with_context(|| format!("worm `{chunk_name}` failed to load"))?;

        let (compile, format, lint, actions, definitions) =
            match exports.get::<Option<mlua::Table>>("frontend")? {
                Some(frontend) => {
                    let compile = frontend.get::<mlua::Function>("compile").with_context(|| {
                        format!("worm `{chunk_name}`: frontend.compile is not a function")
                    })?;

                    // both are optional, in the same way `fmt` and `[lints]` are
                    // optional in the manifest
                    let format = frontend
                        .get::<Option<mlua::Function>>("format")
                        .with_context(|| {
                            format!("worm `{chunk_name}`: frontend.format is not a function")
                        })?;

                    let lint = frontend
                        .get::<Option<mlua::Function>>("lint")
                        .with_context(|| {
                            format!("worm `{chunk_name}`: frontend.lint is not a function")
                        })?;

                    let actions = frontend
                        .get::<Option<mlua::Function>>("actions")
                        .with_context(|| {
                            format!("worm `{chunk_name}`: frontend.actions is not a function")
                        })?;

                    let definitions = frontend
                        .get::<Option<mlua::Function>>("definitions")
                        .with_context(|| {
                            format!("worm `{chunk_name}`: frontend.definitions is not a function")
                        })?;

                    (Some(compile), format, lint, actions, definitions)
                }

                None => (None, None, None, None, None),
            };

        let mut rules = BTreeMap::new();

        if let Some(table) = exports.get::<Option<mlua::Table>>("rules")? {
            for pair in table.pairs::<String, mlua::Table>() {
                let (name, rule) = pair?;

                let visit: mlua::Function = rule.get("visit").with_context(|| {
                    format!("worm `{chunk_name}`: rules.{name}.visit is not a function")
                })?;

                rules.insert(name, visit);
            }
        }

        let init = exports.get::<Option<mlua::Function>>("init")?;

        if compile.is_none() && rules.is_empty() {
            bail!("worm `{chunk_name}` returns neither a frontend nor any rules");
        }

        /*
        A worm that loops forever would hang the build with no escape. Thus
        larvae interrupts the VM at intervals and stops the worm. Larvae arms
        the interrupt once here. A reset of the budget before each call makes
        the limit apply per file.
        */
        let budget = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&budget);

        lua.set_interrupt(move |_| {
            if counter.fetch_add(1, Ordering::Relaxed) > INTERRUPT_EVERY {
                return Err(mlua::Error::runtime("worm ran too long and was stopped"));
            }

            Ok(mlua::VmState::Continue)
        });

        Ok(Self {
            lua,
            name: chunk_name.to_owned(),
            compile,
            format,
            lint,
            actions,
            definitions,
            rules,
            init,
            budget,
        })
    }

    /// Give the settings to the worm once, before the worm sees a file
    ///
    /// A Luau worm gets real tables and not a TOML string. mlua marshals the
    /// tables natively, and an author must not have to parse TOML in Luau. The
    /// wasm form gets the string, because the string is the natural shape there.
    pub fn init(
        &mut self,
        config: &toml::Value,
        rules: &BTreeMap<String, toml::Value>,
        settings: &super::Settings,
    ) -> Result<()> {
        let Some(init) = self.init.clone() else {
            return Ok(());
        };

        let cfg = to_lua(&self.lua, config)?;
        let table = self.lua.create_table()?;

        for (name, value) in rules {
            table.set(name.as_str(), to_lua(&self.lua, value)?)?;
        }

        /*
        The third argument holds the project settings, for a worm that formats
        or reports. A worm with a two-argument init stays correct, because Lua
        drops the arguments a function does not name.
        */
        let extra = self.lua.create_table()?;
        extra.set("fmt", json_to_lua(&self.lua, &settings.fmt)?)?;
        extra.set("lint", json_to_lua(&self.lua, &settings.lint)?)?;

        init.call::<()>((cfg, table, extra))
            .context("worm rejected its configuration")?;

        Ok(())
    }

    /// The rule names this worm implements
    pub fn rule_names(&self) -> impl Iterator<Item = &str> {
        self.rules.keys().map(String::as_str)
    }

    /*
    Run one rule over every node that its filter matched. The tree, the source,
    and the edit queue live in the Lua app data while the rule runs. Thus a
    handle is only a pair of integers and never a pointer into host data.
    */
    pub fn run_rule(
        &mut self,
        rule: &str,
        file: Arc<FileCtx>,
        matched: &[u32],
    ) -> Result<Vec<Edit>> {
        let Some(visit) = self.rules.get(rule).cloned() else {
            bail!("worm has no rule `{rule}`");
        };

        self.budget.store(0, Ordering::Relaxed);
        self.lua.set_app_data(Arc::clone(&file));

        let ctx = self.lua.create_table()?;
        ctx.set("path", file.path.as_str())?;
        ctx.set("value", to_lua_opt(&self.lua, file.value.as_ref())?)?;
        ctx.set("replace", self.lua.create_function(edit_replace)?)?;
        ctx.set("remove", self.lua.create_function(edit_remove)?)?;

        let result = (|| -> mlua::Result<()> {
            for &id in matched {
                visit.call::<()>((NodeRef::new(file.epoch, id), ctx.clone()))?;
            }

            Ok(())
        })();

        self.lua.remove_app_data::<Arc<FileCtx>>();

        match result {
            Ok(()) => Ok(file.take_edits()),

            Err(e) => bail!("rule `{rule}`: {}", worm_message(&e)),
        }
    }

    /// Run the worm over one file, with its `[worms.<name>.config]` table as TOML
    pub fn transform(&mut self, source: &str, config: &str) -> Result<Outcome> {
        let Some(compile) = self.compile.clone() else {
            bail!("worm has no frontend");
        };

        self.budget.store(0, Ordering::Relaxed);

        let result: mlua::Result<mlua::Value> = compile.call((source, config));

        match result {
            Ok(mlua::Value::String(text)) => Ok(Outcome {
                text: text.to_str()?.to_owned(),
                ok: true,
            }),

            // the worm reports a problem and does not produce output
            Ok(mlua::Value::Nil) => bail!("worm returned nothing"),

            Ok(other) => bail!("worm returned {}, expected a string", other.type_name()),

            Err(e) => Ok(Outcome {
                text: worm_message(&e),
                ok: false,
            }),
        }
    }

    /*
    Get the layout of one claimed file, for the host to render.

    The reply crosses as one Lua table in the wire shape of [`proto`], and
    serde deserializes it directly. Thus this transport cannot drift from the
    contract, because no conversion is written by hand.
    */
    pub fn format(&mut self, source: &str) -> Result<proto::FormatReply> {
        let Some(format) = self.format.clone() else {
            bail!(
                "worm `{}` sets fmt = true but its table has no frontend.format",
                self.name
            );
        };

        self.budget.store(0, Ordering::Relaxed);

        let value = format
            .call::<mlua::Value>(source)
            .map_err(|e| anyhow!(worm_message(&e)))?;

        let reply: LuaFormatReply = self
            .lua
            .from_value(value)
            .context("the format reply does not have the documented shape")?;

        Ok(proto::FormatReply {
            /*
            The version field is a contract between transports that ship
            apart. A Luau worm and this host share one process, so the host
            fills the version and the worm does not state it.
            */
            doc: proto::DOC_VERSION,
            document: reply.document,
            spans: reply.spans,
            comments: reply.comments,
        })
    }

    /// Get the problems of one claimed file. The host decides the severity.
    /*
    The actions this worm offers over a byte range.

    A table without the function has none, which is not an error. The editor
    asks on a keystroke, and a worm that only formats has nothing to say.
    */
    pub fn actions(&mut self, source: &str, span: (u32, u32)) -> Result<proto::ActionsReply> {
        let Some(actions) = self.actions.clone() else {
            return Ok(proto::ActionsReply::default());
        };

        self.budget.store(0, Ordering::Relaxed);

        let value = actions
            .call::<mlua::Value>((source, span.0, span.1))
            .map_err(|e| anyhow!(worm_message(&e)))?;

        self.lua
            .from_value(value)
            .context("the actions reply does not have the documented shape")
    }

    /// The Luau type definitions this worm supplies, or none.
    pub fn definitions(&mut self) -> Result<proto::DefinitionsReply> {
        let Some(definitions) = self.definitions.clone() else {
            return Ok(proto::DefinitionsReply::default());
        };

        self.budget.store(0, Ordering::Relaxed);

        let value = definitions
            .call::<mlua::Value>(())
            .map_err(|e| anyhow!(worm_message(&e)))?;

        self.lua
            .from_value(value)
            .context("the definitions reply does not have the documented shape")
    }

    pub fn lint(&mut self, source: &str) -> Result<proto::LintReply> {
        let Some(lint) = self.lint.clone() else {
            bail!(
                "worm `{}` declares lints but its table has no frontend.lint",
                self.name
            );
        };

        self.budget.store(0, Ordering::Relaxed);

        let value = lint
            .call::<mlua::Value>(source)
            .map_err(|e| anyhow!(worm_message(&e)))?;

        self.lua
            .from_value(value)
            .context("the lint reply does not have the documented shape")
    }
}

/*
The format reply, as a Luau worm returns it.

The shape is [`proto::FormatReply`] without the `doc` field. See the comment
in [`LuauWorm::format`] for the reason the field is absent here.
*/
#[derive(serde::Deserialize)]
struct LuaFormatReply {
    #[serde(default)]
    document: Option<proto::WireDoc>,
    #[serde(default)]
    spans: Vec<(u32, u32)>,
    #[serde(default)]
    comments: Vec<(u32, u32)>,
}

#[cfg(test)]
mod tests;
