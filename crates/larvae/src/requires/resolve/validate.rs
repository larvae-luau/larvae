//! The datamodel checks that stop a rewrite from a runtime failure

use crate::diag::Diag;
use crate::requires::datamodel::{DmPath, Realm};

use super::*;

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
        let req_realm = ctx.realm();

        if target_realm == Realm::ServerOnly {
            match req_realm {
                Some(Realm::StarterClone) => {
                    diags.push(
                        Diag::error(ctx.path, format!("require(\"{spec}\"): client code cannot require {} - {} does not replicate to clients", spec, target_dm.service()))
                            .at(src, offset)
                            .with_help("move the module to ReplicatedStorage"),
                    );

                    return None;
                }

                Some(Realm::Shared) | None => {
                    diags.push(
                        Diag::warning(ctx.path, format!("require(\"{spec}\") targets {} from shared code; this breaks if the requirer ever runs on the client", target_dm.service()))
                            .at(src, offset),
                    );
                }

                Some(Realm::ServerOnly) => {}
            }
        }

        if target_realm == Realm::StarterClone {
            // An absolute @game path into a Starter container reaches the template;
            // only relatives in the same container work.
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
        match crate::requires::datamodel::realm_of_container(service) {
            Realm::StarterClone => diags.push(
                Diag::error(ctx.path, format!("absolute require into {service}: Starter containers run as clones, so @game paths into them resolve to the template"))
                    .at(src, offset)
                    .with_help("use a relative require from within the container, or move the module to ReplicatedStorage"),
            ),
            Realm::ServerOnly => {
                if ctx.realm() == Some(Realm::StarterClone) {
                    diags.push(
                        Diag::error(ctx.path, format!("client code cannot require from {service}: it does not replicate to clients"))
                            .at(src, offset),
                    );
                }
            }

            Realm::Shared => {}
        }
    }
}
