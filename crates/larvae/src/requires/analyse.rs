/*!
The whole project questions that `check` answers, as diagnostics.

This module is separate from [`graph`](crate::requires::graph), which knows
edges and nothing about larvae: the graph answers "is there a cycle", and
this module decides whether the project wants a report, what the message
calls the modules, and whether the finding fails the run. The linter has the
same split: a lint finds, and the host sets the level.
*/

use std::path::{Path, PathBuf};

use crate::config::check::CheckConfig;
use crate::diag::{Diag, Severity};
use crate::lint::Level;
use crate::requires::datamodel::{ScriptKind, script_kind};
use crate::requires::graph::{Graph, node_of};

/// Run every enabled whole project check and return the findings
pub fn run(
    graph: &Graph,
    cfg: &CheckConfig,
    requires: &crate::config::RequiresConfig,
    root: &Path,
    /*
    The extensions a worm front-end claims, without the dot. A claimed file
    is written as Luau, so `App.client.luaux` is a LocalScript exactly as
    `App.client.luau` is, and the same checks have to reach it.
    */
    claimed: &[String],
) -> Vec<Diag> {
    let mut out = Vec::new();

    cycles(graph, cfg, &mut out);
    unused(graph, cfg, root, claimed, &mut out);
    early_require(graph, cfg, requires, claimed, &mut out);

    out
}

/*
Client scripts that require through an indexing style that does not wait.

The race exists only for the `roblox-instance` target, because the other two
targets emit no indexing. And it exists only on the client: a server script
runs after the DataModel is built, while a LocalScript can start before its
modules have replicated.

One diagnostic per file. The fix is one config line, so a diagnostic per
require site would repeat the same advice.
*/
fn early_require(
    graph: &Graph,
    cfg: &CheckConfig,
    requires: &crate::config::RequiresConfig,
    claimed: &[String],
    out: &mut Vec<Diag>,
) {
    use crate::config::{IndexingStyle, Target};

    let Some(severity) = severity(cfg.early_require) else {
        return;
    };

    if requires.target != Target::RobloxInstance {
        return;
    }

    let style = requires.indexing_style.unwrap_or_default();

    if style == IndexingStyle::WaitForChild {
        return;
    }

    for node in graph.nodes() {
        let name = node.file_name().and_then(|s| s.to_str()).unwrap_or("");

        if script_kind(name, claimed) != ScriptKind::Client || graph.requires_of(node).is_empty() {
            continue;
        }

        out.push(Diag {
            severity,
            file: node.to_path_buf(),
            line_col: None,
            message: "this client script requires without waiting for replication".to_string(),
            help: Some(
                "a LocalScript can run before what it needs has replicated, set [requires] indexing_style = \"wait_for_child\""
                    .to_string(),
            ),
        });
    }
}

fn severity(level: Level) -> Option<Severity> {
    match level {
        Level::Allow => None,

        Level::Info => Some(Severity::Info),

        Level::Warn => Some(Severity::Warning),

        Level::Deny => Some(Severity::Error),
    }
}

/*
One diagnostic per cycle, reported against its lowest member.

The message names the whole cycle, not one edge of it. To break a cycle, the
reader chooses which dependency to invert, and that choice needs the full
loop in view. A report of `a requires b` for a tangle of four modules points
at the wrong file.
*/
fn cycles(graph: &Graph, cfg: &CheckConfig, out: &mut Vec<Diag>) {
    let Some(severity) = severity(cfg.cycles) else {
        return;
    };

    for cycle in graph.cycles() {
        let Some(first) = cycle.first() else {
            continue;
        };

        let names: Vec<String> = cycle.iter().map(|p| crate::ui::rel(p)).collect();

        let message = match cycle.len() {
            1 => format!("{} requires itself", names[0]),

            _ => format!("these {} modules require each other", cycle.len()),
        };

        out.push(Diag {
            severity,
            file: first.clone(),
            line_col: None,
            message,
            help: Some(format!(
                "the cycle is {}, break it by moving the shared part out or requiring lazily inside a function",
                names.join(" -> ")
            )),
        });
    }
}

/*
Modules that nothing requires.

A Script or a LocalScript is never reported: Roblox runs those, it does not
require them, so no incoming edge is normal for them. That rule covers most
of a project with no configuration, and `[check] entries` covers a module
that starts some other way.

When a worm owns the requires of a file, the graph has no edges for that
file, and the file can require any module. Then "nothing requires this" is a
guess for every module, so the analysis reports nothing.
*/
fn unused(graph: &Graph, cfg: &CheckConfig, root: &Path, claimed: &[String], out: &mut Vec<Diag>) {
    let Some(severity) = severity(cfg.unused_modules) else {
        return;
    };

    if graph.has_opaque() {
        return;
    }

    let configured: Vec<PathBuf> = cfg
        .entries
        .iter()
        .map(|e| {
            let joined = root.join(e);

            node_of(&joined).to_path_buf()
        })
        .collect();

    for module in graph.unused(&configured) {
        if !is_module_script(&module, claimed) {
            continue;
        }

        out.push(Diag {
            severity,
            file: module.clone(),
            line_col: None,
            message: "nothing requires this module".to_string(),
            help: Some(
                "delete it, or list it under [check] entries if something runs it another way"
                    .to_string(),
            ),
        });
    }
}

/// True when Roblox would require this file, not run it
fn is_module_script(path: &Path, claimed: &[String]) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");

    script_kind(name, claimed) == ScriptKind::Module
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_of(edges: &[(&str, &str)], extra: &[&str]) -> Graph {
        let mut g = Graph::default();

        for (from, to) in edges {
            g.add(Path::new(from), Path::new(to));
        }

        for node in extra {
            g.see(Path::new(node));
        }

        g
    }

    fn found(graph: &Graph, cfg: &CheckConfig) -> Vec<Diag> {
        run(
            graph,
            cfg,
            &crate::config::RequiresConfig::default(),
            Path::new("/project"),
            &[],
        )
    }

    /// The instance target with a style that does not wait, which is the race
    fn racing() -> crate::config::RequiresConfig {
        crate::config::RequiresConfig {
            target: crate::config::Target::RobloxInstance,
            indexing_style: Some(crate::config::IndexingStyle::FindFirstChild),
            ..Default::default()
        }
    }

    fn found_with(
        graph: &Graph,
        cfg: &CheckConfig,
        requires: &crate::config::RequiresConfig,
    ) -> Vec<Diag> {
        run(graph, cfg, requires, Path::new("/project"), &[])
    }

    fn denying() -> CheckConfig {
        CheckConfig {
            cycles: Level::Deny,
            unused_modules: Level::Deny,
            ..Default::default()
        }
    }

    // --- cycles ------------------------------------------------------------

    #[test]
    fn a_cycle_is_reported_once_naming_every_member() {
        let g = graph_of(&[("a.luau", "b.luau"), ("b.luau", "a.luau")], &[]);
        let diags = found(&g, &CheckConfig::default());

        assert_eq!(diags.len(), 1);
        assert!(
            diags[0].message.contains("2 modules"),
            "{}",
            diags[0].message
        );

        let help = diags[0].help.as_deref().unwrap_or("");

        assert!(help.contains("a.luau") && help.contains("b.luau"), "{help}");
    }

    #[test]
    fn a_module_requiring_itself_says_so_plainly() {
        let g = graph_of(&[("a.luau", "a.luau")], &[]);
        let diags = found(&g, &CheckConfig::default());

        assert!(
            diags[0].message.contains("requires itself"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn cycles_warn_by_default_and_deny_when_asked() {
        let g = graph_of(&[("a.luau", "b.luau"), ("b.luau", "a.luau")], &[]);

        assert_eq!(
            found(&g, &CheckConfig::default())[0].severity,
            Severity::Warning
        );
        assert_eq!(found(&g, &denying())[0].severity, Severity::Error);
    }

    #[test]
    fn a_project_can_turn_cycles_off() {
        let g = graph_of(&[("a.luau", "b.luau"), ("b.luau", "a.luau")], &[]);
        let cfg = CheckConfig {
            cycles: Level::Allow,
            ..Default::default()
        };

        assert!(found(&g, &cfg).is_empty());
    }

    #[test]
    fn an_acyclic_project_reports_nothing() {
        let g = graph_of(&[("a.luau", "b.luau")], &[]);
        let cfg = CheckConfig {
            cycles: Level::Warn,
            ..Default::default()
        };

        assert!(found(&g, &cfg).is_empty());
    }

    // --- unused -------------------------------------------------------------

    #[test]
    fn unused_modules_are_quiet_until_a_project_asks() {
        let g = graph_of(&[("a.luau", "b.luau")], &["orphan.luau"]);

        assert!(found(&g, &CheckConfig::default()).is_empty());
    }

    #[test]
    fn a_module_nothing_requires_is_reported_when_asked() {
        let g = graph_of(&[("main.server.luau", "b.luau")], &["orphan.luau"]);
        let diags = found(&g, &denying());

        assert_eq!(diags.len(), 1);
        assert!(diags[0].file.ends_with("orphan.luau"));
    }

    /// Roblox runs these rather than requiring them, so no incoming edge is normal
    #[test]
    fn a_script_or_localscript_is_never_an_unused_module() {
        let g = graph_of(&[], &["boot.server.luau", "ui.client.luau"]);

        assert!(found(&g, &denying()).is_empty());
    }

    #[test]
    fn a_configured_entry_is_not_unused() {
        let g = graph_of(&[], &["/project/src/boot.luau"]);
        let cfg = CheckConfig {
            unused_modules: Level::Deny,
            entries: vec![PathBuf::from("src/boot.luau")],
            ..Default::default()
        };

        assert!(found(&g, &cfg).is_empty());
    }

    /// The entry names the init file, and the graph keys the module on its directory
    #[test]
    fn an_entry_written_as_an_init_file_matches_the_directory_node() {
        let g = graph_of(&[], &["/project/src/pkg"]);
        let cfg = CheckConfig {
            unused_modules: Level::Deny,
            entries: vec![PathBuf::from("src/pkg/init.luau")],
            ..Default::default()
        };

        assert!(found(&g, &cfg).is_empty());
    }

    #[test]
    fn a_required_module_is_not_unused() {
        let g = graph_of(&[("main.server.luau", "helper.luau")], &[]);

        assert!(found(&g, &denying()).is_empty());
    }

    /// The graph misses the edges of a worm owned file, so a report is a guess
    #[test]
    fn a_worm_owned_file_silences_the_unused_analysis() {
        let mut g = graph_of(&[], &["orphan.luau"]);
        g.see_opaque(Path::new("styled.luau"));

        assert!(found(&g, &denying()).is_empty());
    }

    /// A missing edge can hide a cycle, but it cannot invent one
    #[test]
    fn a_worm_owned_file_does_not_silence_the_cycle_analysis() {
        let mut g = graph_of(&[("a.luau", "b.luau"), ("b.luau", "a.luau")], &[]);
        g.see_opaque(Path::new("styled.luau"));

        assert_eq!(found(&g, &denying()).len(), 1);
    }

    /// Both checks run, and a project with both problems hears about both
    #[test]
    fn the_two_checks_are_independent() {
        let g = graph_of(
            &[("a.luau", "b.luau"), ("b.luau", "a.luau")],
            &["orphan.luau"],
        );
        let diags = found(&g, &denying());

        assert_eq!(diags.len(), 2, "{diags:?}");
    }

    // --- early require -------------------------------------------------------

    fn client_project() -> Graph {
        let mut g = Graph::default();
        g.add(Path::new("ui.client.luau"), Path::new("helper.luau"));

        g
    }

    #[test]
    fn a_client_script_requiring_without_waiting_is_reported() {
        let diags = found_with(&client_project(), &CheckConfig::default(), &racing());

        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].file.ends_with("ui.client.luau"));
        assert!(
            diags[0]
                .help
                .as_deref()
                .unwrap_or("")
                .contains("wait_for_child")
        );
    }

    #[test]
    fn wait_for_child_closes_the_race_and_says_nothing() {
        let requires = crate::config::RequiresConfig {
            indexing_style: Some(crate::config::IndexingStyle::WaitForChild),
            ..racing()
        };

        assert!(found_with(&client_project(), &CheckConfig::default(), &requires).is_empty());
    }

    /// The other targets emit no indexing, so there is nothing to race on
    #[test]
    fn the_string_target_is_never_reported() {
        let requires = crate::config::RequiresConfig::default();

        assert!(found_with(&client_project(), &CheckConfig::default(), &requires).is_empty());
    }

    /// A server script runs after the DataModel is built
    #[test]
    fn a_server_script_is_not_racing_replication() {
        let mut g = Graph::default();
        g.add(Path::new("boot.server.luau"), Path::new("helper.luau"));

        assert!(found_with(&g, &CheckConfig::default(), &racing()).is_empty());
    }

    #[test]
    fn a_client_script_that_requires_nothing_is_not_reported() {
        let mut g = Graph::default();
        g.see(Path::new("ui.client.luau"));

        assert!(found_with(&g, &CheckConfig::default(), &racing()).is_empty());
    }

    #[test]
    fn it_is_one_diagnostic_per_file_rather_than_per_require() {
        let mut g = Graph::default();
        g.add(Path::new("ui.client.luau"), Path::new("a.luau"));
        g.add(Path::new("ui.client.luau"), Path::new("b.luau"));
        g.add(Path::new("ui.client.luau"), Path::new("c.luau"));

        assert_eq!(found_with(&g, &CheckConfig::default(), &racing()).len(), 1);
    }

    #[test]
    fn a_project_can_turn_it_off() {
        let cfg = CheckConfig {
            early_require: Level::Allow,
            ..Default::default()
        };

        assert!(found_with(&client_project(), &cfg, &racing()).is_empty());
    }
}
