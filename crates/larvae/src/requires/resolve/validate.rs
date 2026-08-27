//! The datamodel checks that stop a rewrite from a runtime failure

use crate::diag::Diag;
use crate::requires::datamodel::{DmPath, Realm};

use super::*;

/*
The name a cross-realm require answers to, and why no `Lint` carries it.

A lint reads one file and nothing else. [`crate::lint::analyze`] takes the
source text and the lint config, and [`crate::lint::LintCtx`] holds neither
the path of the file nor the mount table. A cross-realm require is a fact
about two files and the DataModel that holds them, so no lint can see it.

The resolver maps both ends of every require already, so the check lives
here and the finding reads like a lint finding: the message ends with the
name in parentheses, which is the form [`crate::lint::Finding`] writes. The
level is `deny`, and a deny reaches a report as an error.
*/
pub(crate) const CROSS_REALM_REQUIRE: &str = "cross_realm_require";

/// A require whose two ends run on different halves of the game
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Crossing {
    /// Client code reaches for a server container
    ServerFromClient,
    /// Shared code reaches for a server container
    ServerFromShared,
    /// Shared code reaches for a Starter container
    ClientFromShared,
}

/*
The forbidden pairs, from the realm of the requirer and the realm of the target.

Three pairs stay silent. Server code reaches every container, because the
server holds the whole DataModel. Client code reaches client code, which one
clone carries. And every realm reaches shared code, which is what
ReplicatedStorage is for.

A file that no mount covers has no realm, and its requires stay untouched.
larvae does not know which half of the game runs that file, and a guess
reports correct code.
*/
pub(crate) fn crossing(requirer: Option<Realm>, target: Realm) -> Option<Crossing> {
    match (requirer?, target) {
        (Realm::StarterClone, Realm::ServerOnly) => Some(Crossing::ServerFromClient),

        (Realm::Shared, Realm::ServerOnly) => Some(Crossing::ServerFromShared),

        (Realm::Shared, Realm::StarterClone) => Some(Crossing::ClientFromShared),

        _ => None,
    }
}

impl Crossing {
    /// The sentence and the help, without the require that the reader wrote
    fn words(self, service: &str) -> (String, &'static str) {
        match self {
            Self::ServerFromClient => (
                format!(
                    "client code cannot require from {service}, the server does not replicate it to the client"
                ),
                "move the module to ReplicatedStorage",
            ),

            Self::ServerFromShared => (
                format!(
                    "shared code cannot require from {service}, the server does not replicate it to the client"
                ),
                "move the module to ReplicatedStorage",
            ),

            Self::ClientFromShared => (
                format!(
                    "shared code cannot reference {service} reliably, because a path names the template and the client runs a clone"
                ),
                "consider moving the client folder to ReplicatedStorage",
            ),
        }
    }

    /// The finding, under the name that `larvae check` and the editor print
    fn diag(self, path: &Path, subject: &str, service: &str) -> Diag {
        let (message, help) = self.words(service);

        Diag::error(path, format!("{subject}{message} ({CROSS_REALM_REQUIRE})")).with_help(help)
    }
}

impl<'a> Resolver<'a> {
    pub(super) fn check_dm(
        &self,
        ctx: &FileCtx,
        spec: &str,
        node: &ModuleNode,
        src: &str,
        offset: usize,
        diags: &mut Vec<Diag>,
    ) -> Option<(DmPath, DmPath)> {
        if self.mounts.is_empty() {
            diags.push(
                Diag::error(ctx.path, format!("require(\"{spec}\") needs a filesystem-to-DataModel mapping, but none is configured"))
                    .at(src, offset)
                    .with_help("add a Rojo project file (default.project.json) or [requires.mounts] to larvae.toml"),
            );

            return None;
        }

        let Some(target_dm) = self.mounts.dm_of(node.dm_key_path()) else {
            diags.push(
                Diag::error(
                    ctx.path,
                    format!(
                        "require(\"{spec}\"): no mount covers {}",
                        crate::ui::rel(node.dm_key_path())
                    ),
                )
                .at(src, offset)
                .with_help("add it to [requires.mounts] or mount it in the Rojo project file"),
            );

            return None;
        };

        let Some(req_dm) = ctx.dm.clone() else {
            diags.push(
                Diag::error(ctx.path, format!("this file is not covered by any mount, so require(\"{spec}\") cannot be rewritten"))
                    .at(src, offset),
            );

            return None;
        };

        // Container/realm checks (plan §3.3)
        let target_realm = target_dm.realm();

        /*
        The two ends run on different halves of the game, so the require
        compiles and then fails in a live game. Both `larvae process` and
        `larvae check` stop on it.
        */
        if let Some(crossing) = crossing(ctx.realm(), target_realm) {
            diags.push(
                crossing
                    .diag(
                        ctx.path,
                        &format!("require(\"{spec}\"): "),
                        target_dm.service(),
                    )
                    .at(src, offset),
            );

            return None;
        }

        if target_realm == Realm::StarterClone {
            /*
            An absolute @game path into a Starter container reaches the
            template, so only a relative require inside the same container
            works. The realm rule above already stopped a shared requirer.
            This stops a client one that names another Starter container,
            which no relative walk reaches after the clone.
            */
            if req_dm.service() != target_dm.service() {
                diags.push(
                    Diag::error(ctx.path, format!("require(\"{spec}\") targets {} from outside it; Starter containers run as clones, so this would resolve to the template", target_dm.service()))
                        .at(src, offset)
                        .with_help("move the module to ReplicatedStorage"),
                );

                return None;
            }
        }

        // A module must not require itself.
        if req_dm.segments == target_dm.segments {
            diags.push(
                Diag::error(
                    ctx.path,
                    format!("require(\"{spec}\"): module requires itself"),
                )
                .at(src, offset),
            );

            return None;
        }

        Some((req_dm, target_dm))
    }

    pub(super) fn validate_dm_target_on_disk(
        &self,
        ctx: &FileCtx,
        spec: &str,
        segments: &[String],
        src: &str,
        offset: usize,
        diags: &mut Vec<Diag>,
    ) {
        for mount in self.mounts.mounts() {
            if segments.len() < mount.dm.len() || segments[..mount.dm.len()] != mount.dm[..] {
                continue;
            }

            let mut fs = mount.fs.clone();

            for seg in &segments[mount.dm.len()..] {
                fs.push(seg);
            }

            match resolve_module(&fs, self.claimed) {
                Ok(Some(_)) => return,

                _ => {
                    self.report(
                        ctx, src, offset, diags,
                        format!("require(\"{spec}\"): expansion targets @game/{} but no module exists at {}", segments.join("/"), crate::ui::rel(&fs)),
                    );

                    return;
                }
            }
        }

        /*
        The project puts no module at that path, so the rewrite compiles and
        then fails in a live game. The usual cause is a package directory
        that a user did not mount, and this tool exists to stop that failure.
        So larvae reports it, although a second project file or a plugin
        could in theory put a module there.
        */
        self.report(
            ctx,
            src,
            offset,
            diags,
            format!(
                "require(\"{spec}\"): expansion targets @game/{} but nothing in the project maps there, add the directory to your project file or to [requires.mounts]",
                segments.join("/")
            ),
        );
    }

    /// Warn, or fail when the config sets strict
    fn report(
        &self,
        ctx: &FileCtx,
        src: &str,
        offset: usize,
        diags: &mut Vec<Diag>,
        message: String,
    ) {
        let d = Diag::warning(ctx.path, message).at(src, offset);

        diags.push(if self.strict {
            Diag {
                severity: crate::diag::Severity::Error,
                ..d
            }
        } else {
            d
        });
    }

    pub(super) fn check_container_rules(
        &self,
        ctx: &FileCtx,
        service: &str,
        src: &str,
        offset: usize,
        diags: &mut Vec<Diag>,
    ) {
        let target = crate::requires::datamodel::realm_of_container(service);

        /*
        The absolute form is wrong for every requirer here, a client one
        included, so this rule comes before the realm rule and reports on
        its own.
        */
        if target == Realm::StarterClone {
            diags.push(
                Diag::error(ctx.path, format!("absolute require into {service}: Starter containers run as clones, so @game paths into them resolve to the template"))
                    .at(src, offset)
                    .with_help("use a relative require from within the container, or move the module to ReplicatedStorage"),
            );

            return;
        }

        // The same realm rule that [`check_dm`] applies, on a path with no file.
        if let Some(crossing) = crossing(ctx.realm(), target) {
            diags.push(crossing.diag(ctx.path, "", service).at(src, offset));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixture::Project;
    use super::*;
    use crate::diag::Severity;

    const CLIENT: Option<Realm> = Some(Realm::StarterClone);
    const SHARED: Option<Realm> = Some(Realm::Shared);
    const SERVER: Option<Realm> = Some(Realm::ServerOnly);

    // --- the forbidden pairs ------------------------------------------------

    #[test]
    fn client_code_cannot_require_the_server() {
        assert_eq!(
            crossing(CLIENT, Realm::ServerOnly),
            Some(Crossing::ServerFromClient)
        );
    }

    #[test]
    fn shared_code_cannot_require_the_server() {
        assert_eq!(
            crossing(SHARED, Realm::ServerOnly),
            Some(Crossing::ServerFromShared)
        );
    }

    #[test]
    fn shared_code_cannot_require_the_client() {
        assert_eq!(
            crossing(SHARED, Realm::StarterClone),
            Some(Crossing::ClientFromShared)
        );
    }

    // --- the pairs that stay silent ------------------------------------------

    /// The server holds the whole DataModel, so no realm rule stops it.
    #[test]
    fn server_code_requires_every_realm() {
        for target in [Realm::ServerOnly, Realm::StarterClone, Realm::Shared] {
            assert_eq!(crossing(SERVER, target), None, "{target:?}");
        }
    }

    /// One clone carries both ends, so client code reaches client code.
    #[test]
    fn client_code_requires_client_code() {
        assert_eq!(crossing(CLIENT, Realm::StarterClone), None);
    }

    /// ReplicatedStorage exists for this, so every realm reads it.
    #[test]
    fn every_realm_requires_shared_code() {
        for requirer in [CLIENT, SHARED, SERVER] {
            assert_eq!(crossing(requirer, Realm::Shared), None, "{requirer:?}");
        }
    }

    /// A file that no mount covers has no realm, so the rule leaves it alone.
    #[test]
    fn a_file_outside_the_mounts_has_no_realm() {
        for target in [Realm::ServerOnly, Realm::StarterClone, Realm::Shared] {
            assert_eq!(crossing(None, target), None, "{target:?}");
        }
    }

    // --- the finding that a reader sees --------------------------------------

    /// One require, so the offset of the spec needs no arithmetic.
    const SRC: &str = "return require(\"../x\")\n";

    /// Run the datamodel checks over one require, from one file to another
    fn check(from: &str, to: &str) -> Vec<Diag> {
        let project = Project::new();
        let resolver = project.resolver(false);
        let path = std::path::PathBuf::from(from);
        let ctx = project.ctx(&path);
        let mut diags = Vec::new();

        resolver.check_dm(
            &ctx,
            "../x",
            &ModuleNode::File(to.into()),
            SRC,
            15,
            &mut diags,
        );

        diags
    }

    /*
    The finding names the lint, at the level a deny reports.

    A `Finding` writes its name in parentheses at the end of the message, so
    the two read the same in `larvae check` and in the editor. See
    [`CROSS_REALM_REQUIRE`] for why the check is not a `Lint`.
    */
    #[test]
    fn the_finding_carries_cross_realm_require_at_deny() {
        let diags = check("/proj/src/client/main.luau", "/proj/src/server/keys.luau");
        let found = diags.first().expect("the client reaches for the server");

        assert_eq!(found.severity, Severity::Error, "a deny is an error");
        assert!(
            found.message.ends_with("(cross_realm_require)"),
            "{}",
            found.message
        );
        assert!(
            found.message.contains("does not replicate"),
            "{}",
            found.message
        );
    }

    #[test]
    fn shared_code_that_reaches_the_client_names_the_container() {
        let diags = check("/proj/src/shared/util.luau", "/proj/src/client/hud.luau");
        let found = diags.first().expect("shared reaches for StarterPlayer");

        assert_eq!(found.severity, Severity::Error);
        assert!(
            found.message.contains("StarterPlayer")
                && found.message.ends_with("(cross_realm_require)"),
            "{}",
            found.message
        );
        assert_eq!(
            found.help.as_deref(),
            Some("consider moving the client folder to ReplicatedStorage")
        );
    }

    /// Every allowed pair passes the checks and reports nothing.
    #[test]
    fn an_allowed_pair_reports_nothing() {
        for (from, to) in [
            // server -> anything
            (
                "/proj/src/server/main.server.luau",
                "/proj/src/shared/util.luau",
            ),
            (
                "/proj/src/server/main.server.luau",
                "/proj/src/server/keys.luau",
            ),
            // client -> client, and client -> shared
            ("/proj/src/client/main.luau", "/proj/src/client/hud.luau"),
            ("/proj/src/client/main.luau", "/proj/src/ui/button.luau"),
            ("/proj/src/client/main.luau", "/proj/src/shared/util.luau"),
            // shared -> shared
            ("/proj/src/shared/util.luau", "/proj/src/shared/math.luau"),
        ] {
            assert!(check(from, to).is_empty(), "{from} -> {to}");
        }
    }
}
