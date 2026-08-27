/*!
What the server holds between requests: the config of the project and its
worms, and the reloads that keep both current.
*/

use std::io::Write;
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};

use crate::commands::fmt::pool_with;
use crate::fmt::FmtConfig;
use crate::lint::LintConfig;
use crate::worm::pool::Pool;

use super::uri::path_of_uri;
use super::{Server, rpc};

impl Server {
    pub(super) fn initialize(&mut self, params: &Value, out: &mut impl Write) -> Result<()> {
        self.root = params["rootUri"]
            .as_str()
            .and_then(path_of_uri)
            .or_else(|| {
                params["workspaceFolders"][0]["uri"]
                    .as_str()
                    .and_then(path_of_uri)
            });

        /*
        The editor settings arrive before the config load, so the project
        wins where both speak. The extension sends them at initialize and
        again on every change, which is the contract the config doc states.
        */
        self.editor = params["initializationOptions"]["settings"].clone();

        self.load_config(out)
    }

    /*
    Read the `larvae-lsp` settings the editor sent.

    The project file wins wherever it names the same thing, so these apply
    first and `load_config` writes over them. That order is the whole rule:
    a setting in the repo is shared by everyone who opens it, and a setting
    in the editor belongs to one person on one machine.

    An id the server does not know is ignored. luau-lsp ships about ninety
    settings and the extension mirrors the names, so a server that refused
    an unknown one would fail on every editor that is ahead of it.

    The editor writes an id in camelCase and the project file writes it in
    snake_case, and the two name the same setting. [`spelled`] converts
    between them, so one list of names serves both.
    */
    pub(super) fn apply_editor_settings(&mut self, project: Option<&toml::Value>) {
        let settings = self.editor["larvae-lsp"].clone();
        let lsp = project.and_then(|value| value.get("lsp"));

        /*
        One setting, or nothing where the project file spelled the same
        name. That check is what makes the rule true: `[lsp]` used to be
        copied over the whole table, so a project with any `larvae.toml`
        silently threw away every setting the user had made in the editor,
        including the ones it says nothing about.
        */
        let editor = |path: &[&str]| -> Value {
            if spelled(lsp, path) {
                return Value::Null;
            }

            let mut node = &settings;

            for segment in path {
                node = &node[*segment];
            }

            node.clone()
        };

        if let Some(on) = editor(&["enabled"]).as_bool() {
            self.lsp.enabled = on;
        }

        if let Some(on) = editor(&["claimOnly"]).as_bool() {
            self.lsp.claim_only = on;
        }

        if let Some(on) = editor(&["analyzer"]).as_bool() {
            self.lsp.analyzer = on;
        }

        if let Some(on) = editor(&["completion", "enabled"]).as_bool() {
            self.lsp.completion.enabled = on;
        }

        if let Some(on) = editor(&["completion", "showKeywords"]).as_bool() {
            self.lsp.completion.show_keywords = on;
        }

        if let Some(on) = editor(&["completion", "imports", "enabled"]).as_bool() {
            self.lsp.completion.imports.enabled = on;
        }

        if let Some(on) = editor(&["completion", "imports", "useConst"]).as_bool() {
            self.lsp.completion.imports.use_const = on;
        }

        if let Some(on) = editor(&["index", "enabled"]).as_bool() {
            self.lsp.index.enabled = on;
        }

        if let Some(on) = editor(&["fflags", "enableByDefault"]).as_bool() {
            self.lsp.fflags.enable_by_default = on;
        }

        if let Some(on) = editor(&["fflags", "enableNewSolver"]).as_bool() {
            self.lsp.fflags.enable_new_solver = on;
        }

        /*
        The editor sends `override`, which is a Rust keyword, so the table is
        `over` here. Every value arrives as text, because Luau keeps a
        boolean list and an integer list and the name decides which is asked.
        */
        if let Some(table) = editor(&["fflags", "override"]).as_object() {
            for (name, value) in table {
                let text = match value {
                    Value::String(s) => s.clone(),

                    other => other.to_string(),
                };

                self.lsp.fflags.over.insert(name.clone(), text);
            }
        }

        if let Some(n) = editor(&["bytecode", "debugLevel"]).as_u64() {
            self.lsp.bytecode.debug_level = n as u8;
        }

        if let Some(n) = editor(&["bytecode", "typeInfoLevel"]).as_u64() {
            self.lsp.bytecode.type_info_level = n as u8;
        }

        for (id, field) in [("vectorLib", 0usize), ("vectorCtor", 1), ("vectorType", 2)] {
            let Some(text) = editor(&["bytecode", id]).as_str().map(str::to_owned) else {
                continue;
            };

            match field {
                0 => self.lsp.bytecode.vector_lib = text,
                1 => self.lsp.bytecode.vector_ctor = text,
                _ => self.lsp.bytecode.vector_type = text,
            }
        }

        if let Some(on) = editor(&["studio", "enabled"]).as_bool() {
            self.lsp.studio.enabled = on;
        }

        if let Some(port) = editor(&["studio", "port"]).as_u64() {
            self.lsp.studio.port = port as u16;
        }

        if let Some(on) = editor(&["signatureHelp", "enabled"]).as_bool() {
            self.lsp.signature_help.enabled = on;
        }

        if let Some(on) = editor(&["hover", "enabled"]).as_bool() {
            self.lsp.hover.enabled = on;
        }

        if let Some(on) = editor(&["hover", "showTableKinds"]).as_bool() {
            self.lsp.hover.show_table_kinds = on;
        }

        if let Some(on) = editor(&["hover", "includeStringLength"]).as_bool() {
            self.lsp.hover.include_string_length = on;
        }

        if let Some(on) = editor(&["inlayHints", "variableTypes"]).as_bool() {
            self.lsp.inlay_hints.variable_types = on;
        }

        if let Some(on) = editor(&["inlayHints", "parameterTypes"]).as_bool() {
            self.lsp.inlay_hints.parameter_types = on;
        }

        if let Some(n) = editor(&["inlayHints", "typeHintMaxLength"]).as_u64() {
            self.lsp.inlay_hints.type_hint_max_length = n as usize;
        }

        if let Some(text) = editor(&["sourcemap"]).as_str() {
            self.lsp.sourcemap = text.to_owned();
        }

        /*
        Both spellings of each value are read, because the editor writes
        camelCase and the project file writes snake_case, and a value is
        not a key that [`snake`] converts.
        */
        if let Some(text) = editor(&["characterType"]).as_str() {
            use crate::config::lsp::CharacterType;

            match text.to_lowercase().replace('_', "").as_str() {
                "r15" => self.lsp.character_type = CharacterType::R15,

                "r6" => self.lsp.character_type = CharacterType::R6,

                "notset" => self.lsp.character_type = CharacterType::NotSet,

                _ => {}
            }
        }
    }

    /*
    Read the config of the project, with the defaults as the fallback.

    A broken config does not stop the server, because the user edits that
    config in the editor, and a server that refuses to start over an
    incomplete file is not usable. But silence is wrong too: a project that
    formats with defaults for a whole session looks broken in a quieter
    way. So a config that fails to resolve raises one editor notification,
    and the message carries the reason. A project without a larvae.toml is
    the zero config case and raises nothing.
    */
    pub(super) fn load_config(&mut self, out: &mut impl Write) -> Result<()> {
        let clock = std::time::Instant::now();
        let mark = |what: &str| {
            if std::env::var_os("LARVAE_TIME").is_some() {
                eprintln!("  {:>7.0}ms {what}", clock.elapsed().as_secs_f64() * 1000.0);
            }
        };

        // The load of the worms takes `&mut self`, so the root arrives as a copy.
        let Some(root) = self.root.clone() else {
            self.apply_editor_settings(None);
            self.start_analysis();

            return Ok(());
        };

        let path = root.join("larvae.toml");

        let project = match path.exists() {
            true => match crate::config::Config::load(&path) {
                Ok(cfg) => Some(cfg),

                Err(e) => {
                    warn(
                        out,
                        &format!("{e:#}; larvae serves defaults until it loads"),
                    )?;

                    None
                }
            },

            false => None,
        };

        if let Some(project) = &project {
            self.lsp = project.lsp.clone();
            self.aliases = project.alias_map();
        }

        /*
        The raw table, to ask which settings the project actually spelled.
        A parsed `[lsp]` cannot answer that: a field the project left out
        and a field it set to the default read the same.
        */
        let raw = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str::<toml::Value>(&text).ok());

        self.apply_editor_settings(raw.as_ref());
        mark("editor settings");

        /*
        The flags are known now, so the session can start being built. This
        is the earliest moment it can: a flag decides which type solver the
        globals are registered under, and the project that sets it was only
        read a line ago.
        */
        self.start_analysis();
        mark("analysis started");

        /*
        The analyzer receives the DataModel map, so `@game` resolves.

        The map is built the way the pipeline builds it, from the same two
        sources: `[requires.mounts]` and the rojo project. So the editor and
        `larvae process` answer one require the same way, which is the point
        of reading it from the config rather than guessing.

        A project with no config has no map, and a diagnostic about a broken
        mount belongs to `larvae check` and not to a keystroke, so the
        diagnostics of the build go nowhere here.
        */
        /*
        The flags go in before anything reads a type. They are process wide
        in Luau, so a change of them is a change of what every later answer
        means.
        */
        let unknown = self
            .analysis
            .borrow_mut()
            .as_mut()
            .map(|a| a.set_flags(&self.lsp.fflags))
            .unwrap_or_default();

        for complaint in unknown {
            warn(out, &format!("[lsp.fflags] {complaint}"))?;
        }

        mark("flags");
        self.link_studio(out)?;
        mark("studio link");

        if let Some(analysis) = self.analysis.borrow_mut().as_mut() {
            /*
            A project with no larvae.toml still gets its mounts. The rojo
            project file alone describes a DataModel, and a zero config
            project is the common Roblox case.
            */
            let fallback = crate::config::Config::default();
            let cfg = project.as_ref().unwrap_or(&fallback);

            let rojo = crate::project::rojo::find_project(&root, cfg.rojo.project.as_deref())
                .and_then(|path| crate::project::rojo::load(&path).ok());

            // A broken mount is a `larvae check` diagnostic, not a keystroke one.
            let mut ignored = Vec::new();

            analysis.set_mounts(crate::pipeline::setup::mount_table(
                &root,
                cfg,
                rojo.as_ref(),
                &mut ignored,
            ));
        }

        /*
        The server keeps a copy of its own, because a require completion is
        a filesystem question and is answered without the analyzer. The two
        are built the same way, so they cannot disagree.
        */
        {
            let fallback = crate::config::Config::default();
            let cfg = project.as_ref().unwrap_or(&fallback);

            let rojo = crate::project::rojo::find_project(&root, cfg.rojo.project.as_deref())
                .and_then(|path| crate::project::rojo::load(&path).ok());

            let mut ignored = Vec::new();

            self.mounts =
                crate::pipeline::setup::mount_table(&root, cfg, rojo.as_ref(), &mut ignored);
        }

        mark("mounts");

        /*
        The rig follows the setting. The call re-applies on every load,
        because the setting can change while the session lives.
        */
        if let Some(analysis) = self.analysis.borrow_mut().as_mut() {
            analysis.set_character_type(self.lsp.character_type);
        }

        /*
        A new sourcemap path is a new tree; the same path is checked by its
        stamp. Re-reading on every config change redeclared the whole tree
        into the global scope each time the editor sent its settings.
        */
        if self.sourcemap_config != self.lsp.sourcemap {
            self.sourcemap_config = self.lsp.sourcemap.clone();
            self.sourcemap_read = false;
        }

        self.load_instances();

        mark("sourcemap");

        match FmtConfig::discover(&root, project.as_ref().and_then(|c| c.fmt.as_ref())) {
            Ok(cfg) => self.fmt = cfg,

            Err(e) => warn(out, &format!("{e:#}"))?,
        }

        match LintConfig::discover(&root, project.as_ref().and_then(|c| c.lint.as_ref())) {
            Ok(cfg) => {
                // The root lists apply here too, so the editor and the command agree.
                let (root_in, root_ex) = project
                    .as_ref()
                    .map(|c| (c.include.as_slice(), c.exclude.as_slice()))
                    .unwrap_or((&[], &[]));

                self.excluded = cfg
                    .excludes_under(&root, root_in, root_ex)
                    .unwrap_or_default();
                self.lint = cfg;
            }

            Err(e) => warn(out, &format!("{e:#}"))?,
        }

        self.load_worms(&root, out)?;

        mark("config and worms");

        Ok(())
    }

    /*
    Start the thread that builds the session, once and no more.

    The build runs off the loop, so the editor gets its answer to
    `initialize` in milliseconds and the type questions say `Loading...`
    until the session lands as an event.
    */
    fn start_analysis(&mut self) {
        /*
        Off means never built. The builder stays, so a project that turns
        the analyzer back on gets its session then, without a restart.
        */
        if !self.lsp.analyzer {
            return;
        }

        let (Some(build), Some(events)) = (self.builder.take(), self.events.clone()) else {
            return;
        };

        let flags = self.lsp.fflags.clone();

        std::thread::spawn(move || {
            // The server is gone if this fails, and there is nothing to do about it.
            let _ = events.send(crate::lsp::Event::Analysis(build(&flags)));
        });
    }

    /*
    Read the rojo sourcemap, and give the analyzer the tree it describes.

    The tree becomes one declared type per node, loaded under a name of its
    own generation, and a file-to-type map that says what `script` is inside
    each module. Both go in together, because a binding to a type that the
    scope does not hold binds nothing.

    A project with no sourcemap loads nothing and says nothing. That is the
    common case for a project without rojo, and an editor that warned about
    it on every config change would be wrong on most of them.
    */
    fn load_instances(&mut self) {
        /*
        The tree is types, so it needs the analyzer. A session that is still
        being built leaves this undone, and `refresh_instances` runs it at
        the first request after the session lands.
        */
        if self.analysis.borrow().is_none() {
            return;
        }

        let Some(root) = self.root.clone() else {
            return;
        };

        let path = root.join(&self.lsp.sourcemap);

        let stamp = std::fs::metadata(&path)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())));

        // A file that did not change describes the tree the analyzer holds.
        if self.sourcemap_read && stamp == self.sourcemap_stamp {
            return;
        }

        self.sourcemap_stamp = stamp;
        self.sourcemap_read = true;
        self.sourcemap_generation += 1;

        let read = crate::lsp::instances::read(
            &path,
            &root,
            self.sourcemap_generation,
            &self.worms.claimed(),
        );

        if let Some(analysis) = self.analysis.borrow_mut().as_mut() {
            if !read.is_empty() {
                analysis.definitions("@sourcemap", &read.definitions);
            }

            analysis.set_script_types(&read.script_types);
        }

        self.instances = read;
    }

    /*
    Re-read the sourcemap when rojo rewrote it.

    `rojo sourcemap --watch` rewrites the file whenever a script moves or a
    folder appears, and a server that held the first read all session would
    type a tree the project no longer has. The check costs one stat, so it
    runs before each request that reads a type.

    The analyzer has to be there for the read to land, so a session that is
    still loading leaves the stamp alone and the next request tries again.
    */
    pub(super) fn refresh_instances(&mut self) {
        self.load_instances();
    }

    /*
    Read the worms of the project.

    The server keeps no worm when the build fails, and then serves the Luau
    files as before. A user who edits `[worms]` breaks that table for some
    keystrokes, and an editor that stops at each of them is not usable.

    The build also checks the `[fmt]` table against the options that the
    worms declare, and fills each missing option. So the server takes the new
    fmt config only when the build succeeds.
    */
    fn load_worms(&mut self, root: &Path, out: &mut impl Write) -> Result<()> {
        let mut fmt = self.fmt.clone();

        // the editor never downloads a worm, because a keystroke cannot wait
        match pool_with(root, None, &mut fmt, crate::worm::registry::Fetch::Quiet) {
            Ok(pool) => {
                self.fmt = fmt;
                self.worm_stamp = stamp_of(&pool);
                self.worms = pool;
                self.worm_error = None;
            }

            /*
            One broken worm takes the whole pool with it, so a file that a
            working worm claims goes back to being read as Luau and every
            require that worm answered reports as unknown. That is a big
            change to make in silence, and the reason is one line of
            `[worms]` that only the user can fix.

            The message repeats only when the reason changes. A user who is
            editing that table breaks it on the way to fixing it, and a
            toast per keystroke would be its own problem.
            */
            Err(e) => {
                let reason = format!("{e:#}");

                if self.worm_error.as_deref() != Some(reason.as_str()) {
                    warn(
                        out,
                        &format!("the worms of this project did not load: {reason}"),
                    )?;

                    self.worm_error = Some(reason);
                }

                self.worms = no_worms();
            }
        }

        self.install_lsp_hooks();

        Ok(())
    }

    /*
    The worm hooks, installed into the analyzer behind the seam.

    Tier 1 goes in as module hooks: the analyzer asks the pool before its
    default resolution, so a worm's lowered module is what the type system
    reads. Tier 2 goes in as declaration text. The install runs after every
    worm load, so a rebuilt worm re-declares, which is the invalidation the
    plan demands: a worm change behaves like a file change.

    Without an analyzer this is a no-op, and the pool's own lint and format
    hooks serve as before.
    */
    pub(super) fn install_lsp_hooks(&mut self) {
        let mut analysis = self.analysis.borrow_mut();

        let Some(analysis) = analysis.as_mut() else {
            return;
        };

        if !self.worms.has_lsp_hooks() {
            return;
        }

        let resolve_pool = self.worms.clone();
        let load_pool = self.worms.clone();

        analysis.set_module_hooks(crate::lsp::analysis::ModuleHooks {
            resolve: Box::new(move |from, spec| {
                resolve_pool.lsp_resolve(&from.to_string_lossy(), spec)
            }),
            load: Box::new(move |path| {
                load_pool
                    .lsp_load_any(path)
                    .map(|r| crate::lsp::analysis::plain_view(&r.source).into_owned())
            }),
            claims: self.worms.lsp_resolved_claims(),
        });

        for decl in self.worms.lsp_declarations() {
            analysis.definitions(&decl.name, &decl.source);
        }
    }

    /*
    Say once when a worm widens claim-only serving, because the user wrote
    that setting and the server is overriding it with a reason.
    */
    pub(super) fn notice_widened_serving(&self, out: &mut impl Write) -> Result<()> {
        if !self.lsp.claim_only || !self.worms.lsp_serves_luau() {
            return Ok(());
        }

        let names: Vec<&str> = self
            .worms
            .specs()
            .iter()
            .filter(|s| s.manifest.lsp.serves_luau)
            .map(|s| s.manifest.name.as_str())
            .collect();

        rpc::notify(
            out,
            "window/showMessage",
            json!({
                "type": 3,
                "message": format!(
                    "larvae serves every Luau file here, although [lsp] claim_only is on: worm `{}` answers inside plain Luau",
                    names.join("`, `")
                ),
            }),
        )
    }

    /*
    Rebuild the pool when a worm changed on disk.

    A worm author rebuilds a path worm and expects the next keystroke to use
    it. The command line reads the directory on every run, and a server that
    holds the first build all session would answer with a stale worm. The
    check costs one stat per worm artifact, so the server runs it before each
    request that a worm can answer.
    */
    pub(super) fn refresh_worms(&mut self, out: &mut impl Write) -> Result<()> {
        let Some(root) = self.root.clone() else {
            return Ok(());
        };

        if self.worms.is_empty() {
            return Ok(());
        }

        if stamp_of(&self.worms) != self.worm_stamp {
            self.load_worms(&root, out)?;
        }

        Ok(())
    }
}

/*
One warning toast in the editor; `window/showMessage` type 2 is Warning.

The protocol allows this notification before the reply to `initialize`, so
a config that is broken at startup reports right away.
*/
/*
Whether the `[lsp]` table of the project spells one setting.

A `serde` default and a value the project wrote read the same once the table
is parsed, so the question is asked of the raw TOML. The path arrives in the
editor's spelling and converts on the way down.
*/
fn spelled(lsp: Option<&toml::Value>, path: &[&str]) -> bool {
    let Some(mut node) = lsp else {
        return false;
    };

    for segment in path {
        match node.get(snake(segment)) {
            Some(next) => node = next,

            None => return false,
        }
    }

    true
}

/// One id, from the editor's camelCase to the project file's snake_case
fn snake(name: &str) -> String {
    // `override` is a Rust keyword, so the table is `over` in the config.
    if name == "override" {
        return "over".to_owned();
    }

    let mut out = String::with_capacity(name.len() + 4);

    for c in name.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }

    out
}

fn warn(out: &mut impl Write, message: &str) -> Result<()> {
    rpc::notify(
        out,
        "window/showMessage",
        json!({ "type": 2, "message": message }),
    )
}

/*
The modification time and the size of the entry of each worm.

A rebuilt artifact changes both on every real toolchain, and two stat calls
per worm cost nothing next to a lint pass.
*/
fn stamp_of(pool: &Pool) -> Vec<(std::path::PathBuf, Option<std::time::SystemTime>, u64)> {
    pool.specs()
        .iter()
        .map(|spec| {
            let entry = spec.dir.join(&spec.manifest.entry);
            let meta = std::fs::metadata(&entry).ok();

            (
                entry,
                meta.as_ref().and_then(|m| m.modified().ok()),
                meta.map(|m| m.len()).unwrap_or(0),
            )
        })
        .collect()
}

/// A pool with no worm in it. Every file then takes the Luau route.
pub(super) fn no_worms() -> Pool {
    Pool::new(Vec::new(), 1)
}

impl Server {
    /*
    Open or close the Studio link, to match what the config now says.

    A link that is already open on the right port is left alone, because a
    restart would drop the tree the plugin sent and cost a full resync for
    nothing.

    A port that will not bind is reported once and then let go. Another
    larvae is usually holding it, and the server works without the link, so
    a failure here must not stop the editor from getting its diagnostics.
    */
    pub(super) fn link_studio(&mut self, out: &mut impl Write) -> Result<()> {
        let want = &self.lsp.studio;

        if !want.enabled {
            self.studio = None;

            return Ok(());
        }

        if self.studio.as_ref().is_some_and(|l| l.port() == want.port) {
            return Ok(());
        }

        // The old listener drops first, or the new one cannot take the port.
        self.studio = None;

        match crate::lsp::studio_link::Link::start(want.port) {
            Ok(link) => self.studio = Some(link),

            Err(e) => warn(
                out,
                &format!(
                    "the Roblox Studio link cannot open on port {}: {e}; larvae serves without it",
                    want.port
                ),
            )?,
        }

        Ok(())
    }

    /*
    Give the analyzer the tree the plugin sent, when it changed.

    The listener runs on its own thread and cannot reach the analyzer, which
    lives here behind a `RefCell` and is busy whenever a request is in
    flight. So the listener raises a flag, and this lowers it at the moment
    the analyzer is free. The cost is that a change lands on the next
    request rather than the instant it arrives, and the author is typing
    anyway.
    */
    pub(super) fn refresh_studio(&self) {
        let Some(link) = &self.studio else {
            return;
        };

        if !link.take_dirty() {
            return;
        }

        let Some(text) = link.definitions() else {
            return;
        };

        if let Some(analysis) = self.analysis.borrow_mut().as_mut() {
            analysis.definitions("@studio", &text);
        }
    }
}

impl Server {
    /// True when this binary answers type questions, now or once it has loaded
    pub(super) fn will_analyse(&self) -> bool {
        self.analysis.borrow().is_some() || self.analysis_pending
    }

    /*
    Take the session the builder thread finished, and put it to work.

    Everything the analyzer needs was decided while it did not exist: the
    flags, the DataModel map, the worm hooks, the Studio tree, and the
    sourcemap. `load_config` applies all of them, and it costs about two
    milliseconds, so the session arrives configured rather than bare. This
    is the whole reason the landing is an event and not a poll: a project
    whose editor went quiet would otherwise hold a session that never got
    its mounts.

    Then every open document is checked again. The editor asked for its
    diagnostics before there were types, and nothing else would make it ask
    a second time.
    */
    pub(super) fn take_analysis(
        &mut self,
        built: Box<dyn crate::lsp::analysis::Analysis>,
        out: &mut impl Write,
    ) -> Result<()> {
        *self.analysis.borrow_mut() = Some(built);
        self.analysis_pending = false;

        self.load_config(out)?;

        for uri in self.documents.keys().cloned().collect::<Vec<_>>() {
            self.publish(&uri, out)?;
        }

        /*
        The hints on screen were drawn before there were types, so the
        editor is asked to draw them again. Only a request can say that,
        and the editor's reply says nothing the server acts on.
        */
        rpc::request(out, "workspace/inlayHint/refresh", Value::Null)?;

        Ok(())
    }

    /// True while the session is still being built, so a reply can say so
    pub(super) fn analysis_loading(&self) -> bool {
        self.lsp.analyzer && self.analysis.borrow().is_none() && self.analysis_pending
    }

}
