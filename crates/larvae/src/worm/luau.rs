/*!
The Luau guest form, over `mlua`.

Nothing crosses a memory boundary here. mlua marshals strings natively, so a
Luau worm is an ordinary function and the whole ptr/len dance in [`super::host`]
has no equivalent. That is the point of offering the form: a transform somebody
writes in an afternoon should not need a toolchain or a `wasm32` target.

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

use anyhow::{Context, Result, bail};

use super::Outcome;
use super::ctx::FileCtx;
use crate::rules::edits::Edit;

/// A runaway worm dies instead of hanging the build, checked every N VM instructions
const INTERRUPT_EVERY: u32 = 1_000_000;

/// Ceiling on one worm's VM, generous for a transform and fatal for a leak
const MEMORY_LIMIT: usize = 256 * 1024 * 1024;

/// A loaded Luau worm
pub struct LuauWorm {
    /// Owns the VM `compile` came out of, so the worm outlives nothing it needs
    #[allow(dead_code)]
    lua: mlua::Lua,
    /// The front-end entry point, absent when the worm only ships rules
    compile: Option<mlua::Function>,
    /// `visit` per rule the worm declared, by rule name
    rules: BTreeMap<String, mlua::Function>,
    /// Optional, called once with settings before any file is seen
    init: Option<mlua::Function>,
    /// Interrupt budget, reset per call so the cap is per file and not per build
    budget: Arc<AtomicU32>,
}

impl LuauWorm {
    /// Evaluate a worm module and take its front-end entry point
    pub fn load(source: &str, chunk_name: &str) -> Result<Self> {
        let lua = mlua::Lua::new();

        lua.set_memory_limit(MEMORY_LIMIT)
            .context("could not bound the worm's memory")?;

        /*
        Luau's own sandbox, which freezes the globals and gives each chunk a
        fresh environment. It is what makes "purity is enforced, not requested"
        true rather than aspirational, and it is the reason the Luau form does
        not need the import-table argument the wasm form leans on.
        */
        lua.sandbox(true).context("could not sandbox the worm")?;

        let exports: mlua::Table = lua
            .load(source)
            .set_name(chunk_name)
            .eval()
            .with_context(|| format!("worm `{chunk_name}` failed to load"))?;

        let compile = match exports.get::<Option<mlua::Table>>("frontend")? {
            Some(frontend) => {
                Some(frontend.get::<mlua::Function>("compile").with_context(|| {
                    format!("worm `{chunk_name}`: frontend.compile is not a function")
                })?)
            }

            None => None,
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
        A worm that loops forever would otherwise hang the build with no way
        out, so the VM is interrupted periodically and asked to stop. Armed
        once here, and the budget is reset per call so the cap is per file.
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
            compile,
            rules,
            init,
            budget,
        })
    }

    /// Hand over settings once, before any file is seen
    ///
    /// A Luau worm gets real tables rather than a TOML string, because mlua
    /// marshals them natively and making an author parse TOML in Luau would be
    /// hostile. The wasm form gets the string, since that is what is natural there.
    pub fn init(
        &mut self,
        config: &toml::Value,
        rules: &BTreeMap<String, toml::Value>,
    ) -> Result<()> {
        let Some(init) = self.init.clone() else {
            return Ok(());
        };

        let cfg = to_lua(&self.lua, config)?;
        let table = self.lua.create_table()?;

        for (name, value) in rules {
            table.set(name.as_str(), to_lua(&self.lua, value)?)?;
        }

        init.call::<()>((cfg, table))
            .context("worm rejected its configuration")?;

        Ok(())
    }

    /// Rule names this worm actually implements
    pub fn rule_names(&self) -> impl Iterator<Item = &str> {
        self.rules.keys().map(String::as_str)
    }

    /*
    Run one rule over every node its filter matched. The tree, the source and
    the edit queue live in Lua app data for the duration, so a handle is only
    ever a pair of integers and never a pointer into anything of ours.
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

    /// Run the worm over one file, with its `[config.<name>]` table as TOML
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

            // a worm reporting a problem rather than producing output
            Ok(mlua::Value::Nil) => bail!("worm returned nothing"),

            Ok(other) => bail!("worm returned {}, expected a string", other.type_name()),

            Err(e) => Ok(Outcome {
                text: worm_message(&e),
                ok: false,
            }),
        }
    }
}

/*
A handle is two integers and nothing else. It deliberately does not hold the
file it came from, so that stashing one in a global and using it on the next
file is caught by the epoch rather than silently reading the wrong tree.
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

// Needed to take a NodeRef as a function argument rather than an AnyUserData
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

/// The file currently being walked, or an error if a worm called us outside one
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

        // byte offsets into the original source, half open
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

/// TOML to a Luau value, so a Luau worm reads a table rather than a string
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
A Luau error arrives with the chunk name and line glued to the front and a
traceback glued to the back. Neither helps a user who wants to know what the
worm objected to, so take the message the worm actually wrote.
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

    /// The whole reason handles carry an epoch
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

        // second file, and the worm reaches for the handle it kept
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

        w.init(&config, &rules).unwrap();

        let f = file("local x = 1\n");
        let ids = matching(&f, Kind::Number);

        assert_eq!(
            w.run_rule("strip", Arc::clone(&f), &ids).unwrap()[0].2,
            "vide/true"
        );
    }
}
