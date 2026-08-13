//! Alias, self, and passthrough forms: the four RFC input shapes

use std::path::Path;

use crate::config::Target;
use crate::diag::Diag;
use crate::requires::datamodel::parse_game_path;

use super::emit::absolute_instance;
use super::*;

impl<'a> Resolver<'a> {
    pub(super) fn resolve_self(
        &self,
        ctx: &FileCtx,
        spec: &str,
        rest: &str,
        src: &str,
        offset: usize,
        diags: &mut Vec<Diag>,
    ) -> Rewrite {
        if !ctx.is_init {
            diags.push(
                Diag::error(ctx.path, format!("require(\"{spec}\"): @self is only valid inside an init module (a module-with-children)"))
                    .at(src, offset),
            );
        }

        // The instance target resolves @self on disk like any other require.
        if ctx.target == Target::RobloxInstance {
            let Some(target_base) = normalize_join(&ctx.dir, rest) else {
                diags.push(
                    Diag::error(
                        ctx.path,
                        format!("require(\"{spec}\") escapes the filesystem root"),
                    )
                    .at(src, offset),
                );

                return Rewrite::Keep;
            };

            return self.emit_fs(ctx, spec, &target_base, src, offset, diags);
        }

        // @self is natively valid for the other targets, so it passes through.
        Rewrite::Keep
    }

    /// @game input is already native: validate it; the instance target converts it
    pub(super) fn resolve_game_passthrough(
        &self,
        ctx: &FileCtx,
        rest: &str,
        src: &str,
        offset: usize,
        diags: &mut Vec<Diag>,
    ) -> Rewrite {
        if ctx.target == Target::Path {
            diags.push(
                Diag::warning(ctx.path, format!("require(\"@game/{rest}\") cannot be converted for the path target; leaving it alone"))
                    .at(src, offset),
            );

            return Rewrite::Keep;
        }

        let segments: Vec<String> = rest
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if let Some(service) = segments.first() {
            self.check_container_rules(ctx, service, src, offset, diags);
        }

        if ctx.target == Target::RobloxInstance {
            if segments.is_empty() {
                diags.push(
                    Diag::error(ctx.path, "require(\"@game\") has no path".to_string())
                        .at(src, offset),
                );

                return Rewrite::Keep;
            }

            return Rewrite::Expr(absolute_instance(ctx.style, self.quote, &segments));
        }

        Rewrite::Keep
    }

    pub(super) fn resolve_alias(
        &self,
        ctx: &FileCtx,
        spec: &str,
        alias_rest: &str,
        src: &str,
        offset: usize,
        diags: &mut Vec<Diag>,
    ) -> Rewrite {
        let (name, rest) = split_alias(alias_rest);
        let mut name = name.to_lowercase();
        let mut rest = rest.to_string();
        let mut seen: HashSet<String> = HashSet::new();

        loop {
            if !seen.insert(name.clone()) {
                diags.push(
                    Diag::error(
                        ctx.path,
                        format!("require(\"{spec}\"): alias cycle detected through @{name}"),
                    )
                    .at(src, offset),
                );

                return Rewrite::Keep;
            }

            // larvae.toml wins for each key; then a .luaurc walk upward.
            let (value, base_dir): (&str, &Path) = if let Some(v) = self.toml_aliases.get(&name) {
                (v.as_str(), self.root)
            } else if let Some((v, dir)) = self.luaurc.lookup(&ctx.dir, &name) {
                (v, dir)
            } else {
                diags.push(
                    Diag::error(
                        ctx.path,
                        format!("require(\"{spec}\"): unknown alias @{name}"),
                    )
                    .at(src, offset)
                    .with_help("define it under [aliases] in larvae.toml or in a .luaurc"),
                );

                return Rewrite::Keep;
            };

            // A DataModel-valued alias gets a textual expansion.
            if let Some(dm_base) = parse_game_path(value) {
                if ctx.target == Target::Path {
                    diags.push(
                        Diag::error(ctx.path, format!("require(\"{spec}\"): alias @{name} points at the DataModel ({value}), which the path target cannot express"))
                            .at(src, offset),
                    );

                    return Rewrite::Keep;
                }

                let mut segments = dm_base;
                segments.extend(
                    rest.split('/')
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                );

                if segments.is_empty() {
                    diags.push(
                        Diag::error(
                            ctx.path,
                            format!(
                                "require(\"{spec}\"): expansion of @{name} produced an empty path"
                            ),
                        )
                        .at(src, offset),
                    );

                    return Rewrite::Keep;
                }

                self.check_container_rules(ctx, &segments[0], src, offset, diags);
                self.validate_dm_target_on_disk(ctx, spec, &segments, src, offset, diags);

                if ctx.target == Target::RobloxInstance {
                    return Rewrite::Expr(absolute_instance(ctx.style, self.quote, &segments));
                }

                return Rewrite::Replace(format!("@game/{}", segments.join("/")));
            }

            // An alias-to-alias chain (ex: a = "@b/sub")
            if let Some(chained) = value.strip_prefix('@') {
                let (next_name, value_rest) = split_alias(chained);

                if next_name.eq_ignore_ascii_case("game") {
                    unreachable!("handled by parse_game_path");
                }

                name = next_name.to_lowercase();
                rest = join_specs(value_rest, &rest);
                continue;
            }

            // A filesystem-valued alias, relative to the directory that defines it
            let joined = if rest.is_empty() {
                value.to_string()
            } else {
                join_specs(value, &rest)
            };

            let Some(target_base) = normalize_join(base_dir, &joined) else {
                diags.push(
                    Diag::error(ctx.path, format!("require(\"{spec}\"): alias @{name} resolves outside the filesystem root"))
                        .at(src, offset),
                );

                return Rewrite::Keep;
            };

            return self.emit_fs(ctx, spec, &target_base, src, offset, diags);
        }
    }
}
