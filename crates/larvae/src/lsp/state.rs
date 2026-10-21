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

    The project file wins wherever it names the same setting, so a setting
    the project leaves out stays the editor's. A setting in the repo is
    shared by everyone who opens it, and a setting in the editor belongs to
    one person on one machine. An id the server does not know is ignored,
    so an editor that is ahead of the server still starts it.
    */
    pub(super) fn apply_editor_settings(&mut self, project: Option<&toml::Value>) {
        let settings = &self.editor["larvae-lsp"];
        let lsp = project.and_then(|value| value.get("lsp"));

        // The editor writes camelCase and the project file writes snake_case.
        let editor = |id: &str, key: &str| match lsp.and_then(|t| t.get(key)) {
            Some(_) => None,

            None => settings[id].as_bool(),
        };

        if let Some(on) = editor("enabled", "enabled") {
            self.lsp.enabled = on;
        }

        if let Some(on) = editor("claimOnly", "claim_only") {
            self.lsp.claim_only = on;
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
        // The load of the worms takes `&mut self`, so the root arrives as a copy.
        let Some(root) = self.root.clone() else {
            self.apply_editor_settings(None);

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

        /*
        The realm check reads the DataModel map the way the pipeline builds
        it, from `[requires.mounts]` and the rojo project, so the editor and
        `larvae check` judge one require the same way. A broken mount is a
        `larvae check` diagnostic and not a keystroke one, so the build
        diagnostics go nowhere here.
        */
        let fallback = crate::config::Config::default();
        let cfg = project.as_ref().unwrap_or(&fallback);

        let rojo = crate::project::rojo::find_project(&root, cfg.rojo.project.as_deref())
            .and_then(|path| crate::project::rojo::load(&path).ok());

        self.mounts =
            crate::pipeline::setup::mount_table(&root, cfg, rojo.as_ref(), &mut Vec::new());

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

        self.load_worms(&root, out)
    }

    /*
    Build the pool of worms again.

    The pool fills the `[fmt]` options that the worms declare, and fills each
    missing option. So the server takes the new fmt config only when the
    build succeeds.
    */
    fn load_worms(&mut self, root: &Path, out: &mut impl Write) -> Result<()> {
        let mut fmt = self.fmt.clone();

        // the editor never downloads a worm, because a keystroke cannot wait
        match pool_with(root, None, &mut fmt, crate::worm::registry::Fetch::Quiet) {
            Ok(pool) => {
                let mut complaints = Vec::new();

                /*
                The settings of the analyzer left with it in 0.10. A project
                that still sets one hears why it does nothing, rather than
                that no worm has the name.
                */
                let mut worms = self.lsp.worms.clone();

                worms.retain(|key, _| {
                    let retired = crate::config::lsp::RETIRED.contains(&key.as_str());

                    if retired {
                        complaints.push(format!(
                            "[lsp] {key} does nothing since larvae 0.10: luau-lsp serves the types, and larvae serves lint and format"
                        ));
                    }

                    !retired
                });

                // `[lsp.<worm>]` reaches each worm through its config.
                let pool = pool.with_lsp_settings(&worms, &mut complaints);

                for complaint in complaints {
                    warn(out, &complaint)?;
                }

                self.fmt = fmt;
                self.worm_stamp = stamp_of(&pool);
                self.worms = pool;
                self.worm_error = None;
            }

            /*
            One broken worm takes the whole pool with it, so a file that a
            working worm claims goes back to being read as Luau. That is a big
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

        Ok(())
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
