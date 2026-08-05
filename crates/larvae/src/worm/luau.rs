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

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, bail};

use super::Outcome;

/// A runaway worm dies instead of hanging the build, checked every N VM instructions
const INTERRUPT_EVERY: u32 = 1_000_000;

/// Ceiling on one worm's VM, generous for a transform and fatal for a leak
const MEMORY_LIMIT: usize = 256 * 1024 * 1024;

/// A loaded Luau worm
pub struct LuauWorm {
    /// Owns the VM `compile` came out of, so the worm outlives nothing it needs
    #[allow(dead_code)]
    lua: mlua::Lua,
    compile: mlua::Function,
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

        let frontend: mlua::Table = exports
            .get("frontend")
            .with_context(|| format!("worm `{chunk_name}` returns no `frontend` table"))?;

        let compile: mlua::Function = frontend
            .get("compile")
            .with_context(|| format!("worm `{chunk_name}`: frontend.compile is not a function"))?;

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
            budget,
        })
    }

    /// Run the worm over one file, with its `[config.<name>]` table as TOML
    pub fn transform(&mut self, source: &str, config: &str) -> Result<Outcome> {
        self.budget.store(0, Ordering::Relaxed);

        let result: mlua::Result<mlua::Value> = self.compile.call((source, config));

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
