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

        let (compile, format, lint) = match exports.get::<Option<mlua::Table>>("frontend")? {
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

                (Some(compile), format, lint)
            }

            None => (None, None, None),
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

/*
A handle is two integers and nothing else. By intent, it does not hold the
file it came from. If a worm stores a handle in a global and uses it on the
next file, the epoch check catches this. Thus the handle does not read the
wrong tree silently.
*/
#[derive(Clone, Copy)]
struct NodeRef {
    epoch: u64,
    id: u32,
}

impl NodeRef {
    fn new(epoch: u64, id: u32) -> Self {
        Self { epoch, id }
    }
}

// This impl lets a function take a NodeRef argument and not an AnyUserData
impl mlua::FromLua for NodeRef {
    fn from_lua(value: mlua::Value, _: &mlua::Lua) -> mlua::Result<Self> {
        match value {
            mlua::Value::UserData(ud) => Ok(*ud.borrow::<NodeRef>()?),

            other => Err(mlua::Error::runtime(format!(
                "expected a node, got {}",
                other.type_name()
            ))),
        }
    }
}

/// The file that larvae walks now, or an error if a worm calls this outside a walk
fn current(lua: &mlua::Lua) -> mlua::Result<Arc<FileCtx>> {
    lua.app_data_ref::<Arc<FileCtx>>()
        .map(|file| Arc::clone(&file))
        .ok_or_else(|| mlua::Error::runtime("no file is being walked right now"))
}

fn checked(lua: &mlua::Lua, node: &NodeRef) -> mlua::Result<Arc<FileCtx>> {
    let file = current(lua)?;

    file.check(node.epoch)
        .map_err(|e| mlua::Error::runtime(e.to_string()))?;

    Ok(file)
}

impl mlua::UserData for NodeRef {
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("kind", |lua, this, ()| {
            let file = checked(lua, this)?;
            let node = file
                .table
                .get(this.id)
                .ok_or_else(|| mlua::Error::runtime("no such node"))?;

            Ok(node.kind.name())
        });

        methods.add_method("text", |lua, this, ()| {
            let file = checked(lua, this)?;

            Ok(file.table.text(this.id, &file.src).unwrap_or("").to_owned())
        });

        // byte offsets into the original source, as a half open range
        methods.add_method("span", |lua, this, ()| {
            let file = checked(lua, this)?;
            let node = file
                .table
                .get(this.id)
                .ok_or_else(|| mlua::Error::runtime("no such node"))?;

            Ok((node.span.0, node.span.1))
        });

        methods.add_method("children", |lua, this, ()| {
            let file = checked(lua, this)?;
            let node = file
                .table
                .get(this.id)
                .ok_or_else(|| mlua::Error::runtime("no such node"))?;

            let out = lua.create_table()?;

            for (i, &child) in node.children.iter().enumerate() {
                out.set(i + 1, NodeRef::new(this.epoch, child))?;
            }

            Ok(out)
        });

        methods.add_method("parent", |lua, this, ()| {
            let file = checked(lua, this)?;
            let node = file
                .table
                .get(this.id)
                .ok_or_else(|| mlua::Error::runtime("no such node"))?;

            Ok(node.parent.map(|p| NodeRef::new(this.epoch, p)))
        });
    }
}

fn edit_replace(
    lua: &mlua::Lua,
    (_ctx, node, text): (mlua::Value, NodeRef, String),
) -> mlua::Result<()> {
    let file = checked(lua, &node)?;

    file.replace(node.id, text)
        .map_err(|e| mlua::Error::runtime(e.to_string()))
}

fn edit_remove(lua: &mlua::Lua, (_ctx, node): (mlua::Value, NodeRef)) -> mlua::Result<()> {
    let file = checked(lua, &node)?;

    file.remove(node.id)
        .map_err(|e| mlua::Error::runtime(e.to_string()))
}

/// Convert TOML to a Luau value, so a Luau worm reads a table and not a string
fn to_lua(lua: &mlua::Lua, value: &toml::Value) -> mlua::Result<mlua::Value> {
    Ok(match value {
        toml::Value::String(s) => mlua::Value::String(lua.create_string(s)?),

        toml::Value::Integer(n) => mlua::Value::Integer(*n),

        toml::Value::Float(f) => mlua::Value::Number(*f),

        toml::Value::Boolean(b) => mlua::Value::Boolean(*b),

        toml::Value::Datetime(d) => mlua::Value::String(lua.create_string(d.to_string())?),

        toml::Value::Array(items) => {
            let out = lua.create_table()?;

            for (i, item) in items.iter().enumerate() {
                out.set(i + 1, to_lua(lua, item)?)?;
            }

            mlua::Value::Table(out)
        }

        toml::Value::Table(map) => {
            let out = lua.create_table()?;

            for (k, v) in map {
                out.set(k.as_str(), to_lua(lua, v)?)?;
            }

            mlua::Value::Table(out)
        }
    })
}

fn to_lua_opt(lua: &mlua::Lua, value: Option<&toml::Value>) -> mlua::Result<mlua::Value> {
    match value {
        Some(v) => to_lua(lua, v),

        None => Ok(mlua::Value::Nil),
    }
}

/*
Convert one settings field to a Luau value.

The registry stores each field as JSON text, because the native transport
speaks JSON. A Luau worm gets a real table instead, for the same reason it
gets its config as a table. An empty field gives an empty table, so a worm
indexes the settings without a nil check.

The serializer turns a JSON null into `nil` and not into a userdata marker,
so an absent option compares equal to nil in the worm.
*/
fn json_to_lua(lua: &mlua::Lua, json: &str) -> Result<mlua::Value> {
    let value: serde_json::Value = match json {
        "" => serde_json::Value::Object(Default::default()),

        text => serde_json::from_str(text).context("the settings are not valid JSON")?,
    };

    let options = mlua::serde::SerializeOptions::new()
        .serialize_none_to_null(false)
        .serialize_unit_to_null(false);

    lua.to_value_with(&value, options)
        .context("the settings do not convert to Luau")
}

/*
A Luau error arrives with the chunk name and line at the front and a traceback
at the back. These parts do not help a user who wants the reason from the
worm. Thus this function extracts the message that the worm wrote.
*/
fn worm_message(e: &mlua::Error) -> String {
    let full = match e {
        mlua::Error::CallbackError { cause, .. } => cause.to_string(),

        other => other.to_string(),
    };

    let head = full.split("\nstack traceback:").next().unwrap_or(&full);

    head.rsplit_once(':')
        .map(|(_, msg)| msg.trim())
        .filter(|msg| !msg.is_empty())
        .unwrap_or(head.trim())
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worm(body: &str) -> LuauWorm {
        LuauWorm::load(body, "test").expect("worm loads")
    }

    const ECHO: &str = r#"
return {
    frontend = {
        compile = function(source, config)
            return source .. "|" .. config
        end,
    },
}
"#;

    #[test]
    fn a_string_survives_the_round_trip() {
        let out = worm(ECHO).transform("hello", "cfg").unwrap();

        assert!(out.ok);
        assert_eq!(out.text, "hello|cfg");
    }

    #[test]
    fn utf8_and_newlines_cross_intact() {
        let src = "local s = \"héllo ✓\"\nreturn s\n";
        let out = worm(ECHO).transform(src, "").unwrap();

        assert_eq!(out.text, format!("{src}|"));
    }

    #[test]
    fn one_instance_serves_many_files() {
        let mut w = worm(ECHO);

        for i in 0..500 {
            let src = format!("file {i}");

            assert_eq!(w.transform(&src, "c").unwrap().text, format!("{src}|c"));
        }
    }

    #[test]
    fn a_worm_reporting_a_problem_is_not_an_error() {
        let out = worm(
            r#"
return { frontend = { compile = function() error("refused, bad tag") end } }
"#,
        )
        .transform("x", "")
        .unwrap();

        assert!(!out.ok);
        assert!(out.text.contains("refused, bad tag"), "{}", out.text);
    }

    #[test]
    fn a_module_without_a_frontend_is_refused_at_load() {
        let err = LuauWorm::load("return {}", "test").err().unwrap();

        assert!(err.to_string().contains("frontend"), "{err}");
    }

    #[test]
    fn syntax_errors_are_refused_at_load() {
        assert!(LuauWorm::load("this is not luau ===", "test").is_err());
    }

    #[test]
    fn returning_the_wrong_type_is_an_error() {
        let err = worm("return { frontend = { compile = function() return 42 end } }")
            .transform("x", "")
            .unwrap_err();

        assert!(err.to_string().contains("expected a string"), "{err}");
    }

    /// One bad file must not end a watch session
    #[test]
    fn a_failure_does_not_poison_the_next_call() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source)
            if source == "BAD" then error("no") end
            return source
        end,
    },
}
"#,
        );

        assert!(!w.transform("BAD", "").unwrap().ok);
        assert_eq!(w.transform("good", "").unwrap().text, "good");
    }

    /// The sandbox is the reason a worm cannot reach the filesystem
    #[test]
    fn the_sandbox_denies_ambient_authority() {
        let out = worm(
            r#"
return {
    frontend = {
        compile = function()
            if io ~= nil or os ~= nil and os.execute ~= nil then
                return "REACHABLE"
            end
            return "sandboxed"
        end,
    },
}
"#,
        )
        .transform("x", "")
        .unwrap();

        assert_eq!(out.text, "sandboxed");
    }

    #[test]
    fn an_endless_worm_is_stopped_rather_than_hanging() {
        let out = worm(
            r#"
return { frontend = { compile = function() while true do end end } }
"#,
        )
        .transform("x", "")
        .unwrap();

        assert!(!out.ok);
        assert!(out.text.contains("too long"), "{}", out.text);
    }
}

#[cfg(test)]
mod rule_tests {
    use super::*;
    use crate::syntax::{lexer, parser};
    use crate::worm::nodes::{Kind, NodeTable};

    fn file(src: &str) -> Arc<FileCtx> {
        let lexed = lexer::lex(src).unwrap();
        let chunk = parser::parse(src, &lexed.toks).unwrap();
        let toks = lexed.toks.clone();

        let bytes = move |span: crate::syntax::ast::TokSpan| -> (u32, u32) {
            if span.is_empty() {
                let at = toks
                    .get(span.start as usize)
                    .map(|t| t.start)
                    .unwrap_or_default();
                return (at, at);
            }
            (
                toks[span.start as usize].start,
                toks[span.end as usize - 1].end,
            )
        };

        Arc::new(FileCtx::new(
            NodeTable::build(&chunk, &bytes),
            src.to_owned(),
            "test.luau".into(),
        ))
    }

    fn matching(f: &FileCtx, kind: Kind) -> Vec<u32> {
        f.table
            .iter()
            .filter(|(_, n)| n.kind == kind)
            .map(|(id, _)| id)
            .collect()
    }

    const STRIP: &str = r#"
return {
    rules = {
        strip = {
            visit = function(node, ctx)
                if node:kind() == "Number" then
                    ctx:replace(node, "0")
                end
            end,
        },
    },
}
"#;

    #[test]
    fn a_rule_reads_nodes_and_queues_edits() {
        let mut w = LuauWorm::load(STRIP, "t").unwrap();
        let f = file("local x = 41\n");
        let ids = matching(&f, Kind::Number);

        let edits = w.run_rule("strip", Arc::clone(&f), &ids).unwrap();

        assert_eq!(edits, [(10, 12, "0".to_owned())]);
    }

    #[test]
    fn a_worm_can_ship_rules_without_a_frontend() {
        let w = LuauWorm::load(STRIP, "t").unwrap();

        assert_eq!(w.rule_names().collect::<Vec<_>>(), ["strip"]);
    }

    #[test]
    fn a_worm_with_neither_role_is_refused() {
        let err = LuauWorm::load("return { rules = {} }", "t").err().unwrap();

        assert!(err.to_string().contains("neither"), "{err}");
    }

    #[test]
    fn text_and_span_and_children_all_reach_the_real_tree() {
        let src = "print(\"a\", 2)\n";
        let mut w = LuauWorm::load(
            r#"
return {
    rules = {
        probe = {
            visit = function(node, ctx)
                local kids = node:children()
                local start, stop = node:span()
                ctx:replace(node, node:text() .. "|" .. #kids .. "|" .. start .. ":" .. stop)
            end,
        },
    },
}
"#,
            "t",
        )
        .unwrap();

        let f = file(src);
        let ids = matching(&f, Kind::CallExpr);
        let edits = w.run_rule("probe", Arc::clone(&f), &ids).unwrap();

        assert_eq!(edits[0].2, "print(\"a\", 2)|3|0:13");
    }

    #[test]
    fn parent_walks_back_up() {
        let mut w = LuauWorm::load(
            r#"
return {
    rules = {
        up = {
            visit = function(node, ctx)
                ctx:replace(node, node:parent():kind())
            end,
        },
    },
}
"#,
            "t",
        )
        .unwrap();

        let f = file("local x = 1\n");
        let ids = matching(&f, Kind::Number);

        assert_eq!(
            w.run_rule("up", Arc::clone(&f), &ids).unwrap()[0].2,
            "Local"
        );
    }

    /// This test shows the reason a handle carries an epoch
    #[test]
    fn a_handle_stashed_across_files_is_caught() {
        let mut w = LuauWorm::load(
            r#"
local stashed = nil
return {
    rules = {
        stash = {
            visit = function(node, ctx)
                if stashed == nil then
                    stashed = node
                else
                    ctx:replace(stashed, "boom")
                end
            end,
        },
    },
}
"#,
            "t",
        )
        .unwrap();

        let first = file("local a = 1\n");
        let ids = matching(&first, Kind::Number);
        w.run_rule("stash", Arc::clone(&first), &ids).unwrap();

        // on the second file, the worm uses the handle it kept
        let second = file("local b = 2\n");
        let ids = matching(&second, Kind::Number);
        let err = w.run_rule("stash", Arc::clone(&second), &ids).unwrap_err();

        assert!(err.to_string().contains("outlived its file"), "{err}");
    }

    #[test]
    fn remove_preserves_the_line_count() {
        let src = "local f = function()\n  return 1\nend\n";
        let mut w = LuauWorm::load(
            r#"
return { rules = { drop = { visit = function(node, ctx) ctx:remove(node) end } } }
"#,
            "t",
        )
        .unwrap();

        let f = file(src);
        let ids = matching(&f, Kind::FunctionExpr);
        let edits = w.run_rule("drop", Arc::clone(&f), &ids).unwrap();

        let (start, end, text) = &edits[0];
        assert_eq!(
            src[*start as usize..*end as usize].matches('\n').count(),
            text.matches('\n').count()
        );
    }

    #[test]
    fn init_hands_over_config_and_only_the_enabled_rules() {
        let mut w = LuauWorm::load(
            r#"
local seen = nil
return {
    init = function(config, rules)
        seen = config.factory .. "/" .. tostring(rules.strip)
    end,
    rules = { strip = { visit = function(node, ctx) ctx:replace(node, seen) end } },
}
"#,
            "t",
        )
        .unwrap();

        let config = toml::from_str::<toml::Value>("factory = \"vide\"").unwrap();
        let rules = BTreeMap::from([("strip".to_owned(), toml::Value::Boolean(true))]);

        // the default settings also show that a two-argument init keeps working
        w.init(&config, &rules, &Default::default()).unwrap();

        let f = file("local x = 1\n");
        let ids = matching(&f, Kind::Number);

        assert_eq!(
            w.run_rule("strip", Arc::clone(&f), &ids).unwrap()[0].2,
            "vide/true"
        );
    }
}

#[cfg(test)]
mod frontend_tests {
    use super::*;
    use crate::fmt::FmtConfig;

    fn worm(body: &str) -> LuauWorm {
        LuauWorm::load(body, "test").expect("worm loads")
    }

    /// The full path: a worm document with a host span, rendered by the host
    #[test]
    fn a_document_with_a_host_span_renders_in_the_project_style() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source) return source end,
        format = function(source)
            local at = string.find(source, "local", 1, true) - 1
            return {
                document = {
                    concat = {
                        { src = { 0, at } },
                        { host = { start = at, ["end"] = #source, parse = "block" } },
                    },
                },
                comments = {},
            }
        end,
    },
}
"#,
        );

        let src = "<Frame>\nlocal  x  =  1";
        let reply = w.format(src).unwrap();
        let out = proto::render_format(src, &reply, &FmtConfig::default()).unwrap();

        assert_eq!(out, "<Frame>\nlocal x = 1\n");
    }

    /// The least a worm can do: name the Luau, and larvae lays it out
    #[test]
    fn named_luau_spans_format_through_the_host() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source) return source end,
        format = function(source)
            local first = string.find(source, "local", 1, true) - 1
            local stop = string.find(source, "\n</Frame>", 1, true) - 1
            return { spans = { { first, stop } } }
        end,
    },
}
"#,
        );

        let src = "<Frame>\nlocal  x   =  1\n</Frame>\n";
        let reply = w.format(src).unwrap();
        let out = proto::render_format(src, &reply, &FmtConfig::default()).unwrap();

        assert_eq!(out, "<Frame>\nlocal x = 1\n</Frame>\n");
    }

    #[test]
    fn a_lint_reply_crosses_with_findings_and_a_shadow() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source) return source end,
        lint = function(source)
            return {
                findings = {
                    { span = { 0, 4 }, lint = "tidy", message = "untidy", help = "tidy it" },
                },
                comments = { { 5, 9 } },
                luau = string.rep(" ", #source),
            }
        end,
    },
}
"#,
        );

        let reply = w.lint("ab cd efgh").unwrap();

        assert_eq!(reply.findings.len(), 1);
        assert_eq!(reply.findings[0].span, (0, 4));
        assert_eq!(reply.findings[0].lint, "tidy");
        assert_eq!(reply.findings[0].message, "untidy");
        assert_eq!(reply.findings[0].help.as_deref(), Some("tidy it"));
        assert_eq!(reply.comments, vec![(5, 9)]);
        assert_eq!(reply.luau.as_deref(), Some("          "));
    }

    /// An empty list and an absent list both mean "none"
    #[test]
    fn empty_and_absent_lists_both_cross() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source) return source end,
        lint = function() return { findings = {} } end,
    },
}
"#,
        );

        let reply = w.lint("x").unwrap();

        assert!(reply.findings.is_empty());
        assert!(reply.comments.is_empty());
        assert_eq!(reply.luau, None);
    }

    /// The manifest promised a capability that the table does not supply
    #[test]
    fn a_missing_format_function_names_the_manifest_flag() {
        let mut w = worm("return { frontend = { compile = function(source) return source end } }");

        let err = w.format("x").unwrap_err();

        assert!(
            err.to_string()
                .contains("sets fmt = true but its table has no frontend.format"),
            "{err}"
        );
    }

    #[test]
    fn a_missing_lint_function_names_the_manifest_declaration() {
        let mut w = worm("return { frontend = { compile = function(source) return source end } }");

        let err = w.lint("x").unwrap_err();

        assert!(
            err.to_string()
                .contains("declares lints but its table has no frontend.lint"),
            "{err}"
        );
    }

    /// The settings arrive as tables, so no worm parses JSON
    #[test]
    fn init_hands_over_the_settings_as_tables() {
        let mut w = worm(
            r#"
local seen = nil
return {
    init = function(config, rules, settings)
        seen = tostring(settings.fmt.column_width) .. "/" .. tostring(settings.lint.tidy)
    end,
    frontend = {
        compile = function(source) return source end,
        lint = function()
            return {
                findings = {
                    { span = { 0, 1 }, lint = "probe", message = "saw " .. seen },
                },
            }
        end,
    },
}
"#,
        );

        let settings = crate::worm::Settings {
            fmt: r#"{"column_width":88}"#.to_owned(),
            lint: r#"{"tidy":"warn"}"#.to_owned(),
        };
        let config = toml::from_str::<toml::Value>("").unwrap();

        w.init(&config, &BTreeMap::new(), &settings).unwrap();

        let reply = w.lint("x").unwrap();

        assert_eq!(reply.findings[0].message, "saw 88/warn");
    }

    /// The version field is an in-tree contract, so the host fills it
    #[test]
    fn a_reply_without_a_doc_field_gets_the_host_version() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source) return source end,
        format = function() return { document = "nil" } end,
    },
}
"#,
        );

        let reply = w.format("x").unwrap();

        assert_eq!(reply.doc, proto::DOC_VERSION);
        assert_eq!(reply.document, Some(proto::WireDoc::Nil));
    }

    /// A worm error carries the reason the worm wrote, not a traceback
    #[test]
    fn a_format_error_carries_the_worm_message() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source) return source end,
        format = function() error("line 2 is not markup") end,
    },
}
"#,
        );

        let err = w.format("x").unwrap_err();

        assert!(err.to_string().contains("line 2 is not markup"), "{err}");
    }

    /// A reply of the wrong shape is refused with the reason, not a panic
    #[test]
    fn a_malformed_reply_is_refused() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source) return source end,
        format = function() return "not a table" end,
    },
}
"#,
        );

        let err = w.format("x").unwrap_err();

        assert!(
            format!("{err:#}").contains("does not have the documented shape"),
            "{err:#}"
        );
    }
}
