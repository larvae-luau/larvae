//! Convert a resolved module into the configured output form.

use std::path::Path;

use crate::config::{IndexingStyle, Target};
use crate::diag::Diag;
use crate::requires::datamodel::Realm;

use super::*;

impl<'a> Resolver<'a> {
    pub(super) fn emit_fs(
        &self,
        ctx: &FileCtx,
        spec: &str,
        target_base: &Path,
        src: &str,
        offset: usize,
        diags: &mut Vec<Diag>,
    ) -> Rewrite {
        if has_module_extension(spec) {
            diags.push(
                Diag::error(
                    ctx.path,
                    format!(
                        "require(\"{spec}\"): require paths are extensionless per the Luau RFC"
                    ),
                )
                .at(src, offset),
            );

            return Rewrite::Keep;
        }

        let node = match resolve_module(target_base, self.claimed) {
            Ok(Some(node)) => node,

            Ok(None) => {
                let d = Diag::warning(
                    ctx.path,
                    format!(
                        "require(\"{spec}\"): no module found at {}",
                        crate::ui::rel(target_base)
                    ),
                )
                .at(src, offset);
                diags.push(if self.strict {
                    Diag {
                        severity: crate::diag::Severity::Error,
                        ..d
                    }
                } else {
                    d
                });

                return Rewrite::Keep;
            }

            Err(msg) => {
                diags.push(
                    Diag::error(ctx.path, format!("require(\"{spec}\"): {msg}")).at(src, offset),
                );

                return Rewrite::Keep;
            }
        };

        /*
        The edge, recorded at the one place where a require becomes a file.

        Every spelling reaches this point: relative, alias and instance
        alike. So the graph keys on the resolved module, not on the written
        form, and two files that name one module add one node.
        */
        ctx.required
            .borrow_mut()
            .push(node.dm_key_path().to_path_buf());

        let out = match ctx.target {
            Target::Path => self.emit_path_target(ctx, &node),

            Target::RobloxString => {
                match self.emit_roblox_string(ctx, spec, &node, src, offset, diags) {
                    Some(s) => s,

                    None => return Rewrite::Keep,
                }
            }

            Target::RobloxInstance => {
                return match self.emit_roblox_instance(ctx, spec, &node, src, offset, diags) {
                    Some(expr) => Rewrite::Expr(expr),

                    None => Rewrite::Keep,
                };
            }
        };

        if out == spec {
            Rewrite::Keep
        } else {
            Rewrite::Replace(out)
        }
    }

    pub(super) fn emit_path_target(&self, ctx: &FileCtx, node: &ModuleNode) -> String {
        let target = match node {
            ModuleNode::Dir(d) => d.clone(),

            ModuleNode::File(f) => f.with_extension(""),
        };

        fs_relative(ctx.dot_base(), &target)
    }

    /// Map both ends into the DataModel and emit the @game form
    pub(super) fn emit_roblox_string(
        &self,
        ctx: &FileCtx,
        spec: &str,
        node: &ModuleNode,
        src: &str,
        offset: usize,
        diags: &mut Vec<Diag>,
    ) -> Option<String> {
        let (req_dm, target_dm) = self.check_dm(ctx, spec, node, src, offset, diags)?;

        // Child of the requirer -> @self
        if target_dm.segments.len() > req_dm.segments.len()
            && target_dm.segments[..req_dm.segments.len()] == req_dm.segments[..]
        {
            let tail = target_dm.segments[req_dm.segments.len()..].join("/");

            return Some(format!("@self/{tail}"));
        }

        // Relative within the same mount, absolute @game otherwise; Starter targets must be relative.
        let same_mount = req_dm.mount == target_dm.mount;
        let base = &req_dm.segments[..req_dm.segments.len() - 1];

        let common = base
            .iter()
            .zip(&target_dm.segments)
            .take_while(|(a, b)| a == b)
            .count();
        let relative_ok = same_mount && common >= req_dm.mount_depth.min(target_dm.mount_depth);

        if relative_ok || target_dm.realm() == Realm::StarterClone {
            if !relative_ok {
                diags.push(
                    Diag::error(ctx.path, format!("require(\"{spec}\") cannot be expressed as a relative require within {}", target_dm.service()))
                        .at(src, offset),
                );

                return None;
            }

            let ups = base.len() - common;
            let mut parts: Vec<&str> = std::iter::repeat_n("..", ups).collect();

            parts.extend(target_dm.segments[common..].iter().map(String::as_str));

            return Some(if ups == 0 {
                format!("./{}", parts.join("/"))
            } else {
                parts.join("/")
            });
        }

        Some(target_dm.game_path())
    }

    /// Build an Instance expression like script.Parent:FindFirstChild("shared")
    pub(super) fn emit_roblox_instance(
        &self,
        ctx: &FileCtx,
        spec: &str,
        node: &ModuleNode,
        src: &str,
        offset: usize,
        diags: &mut Vec<Diag>,
    ) -> Option<String> {
        let (req_dm, target_dm) = self.check_dm(ctx, spec, node, src, offset, diags)?;

        /*
        Script relative chains survive Starter cloning. They are mandatory
        there and preferred in the same mount. All other cases become
        absolute.
        */
        let same_mount = req_dm.mount == target_dm.mount;

        if same_mount || target_dm.realm() == Realm::StarterClone {
            let common = req_dm
                .segments
                .iter()
                .zip(&target_dm.segments)
                .take_while(|(a, b)| a == b)
                .count();
            let ups = req_dm.segments.len() - common;
            let mut expr = String::from("script");

            for _ in 0..ups {
                expr.push_str(".Parent");
            }

            push_downs(
                &mut expr,
                ctx.style,
                self.quote,
                &target_dm.segments[common..],
            );

            return Some(expr);
        }

        Some(absolute_instance(
            ctx.style,
            self.quote,
            &target_dm.segments,
        ))
    }
}

// --- instance expression building --------------------------------------------

/*
The absolute instance path from the root. The property style uses plain
dots. The method styles use GetService, so a service rename does not break
them.
*/
pub(super) fn absolute_instance(style: IndexingStyle, quote: char, segments: &[String]) -> String {
    let mut expr = String::from("game");

    match style {
        IndexingStyle::Property => push_downs(&mut expr, style, quote, segments),

        IndexingStyle::FindFirstChild | IndexingStyle::WaitForChild => {
            expr.push_str(&format!(
                ":GetService({})",
                lua_quote(segments[0].as_str(), quote)
            ));
            push_downs(&mut expr, style, quote, &segments[1..]);
        }
    }

    expr
}

/// Append child-indexing steps in the chosen style
fn push_downs(expr: &mut String, style: IndexingStyle, quote: char, segments: &[String]) {
    for seg in segments {
        match style {
            IndexingStyle::Property => {
                if is_luau_ident(seg) {
                    expr.push('.');
                    expr.push_str(seg);
                } else {
                    expr.push_str(&format!("[{}]", lua_quote(seg, quote)));
                }
            }

            IndexingStyle::FindFirstChild => {
                expr.push_str(&format!(":FindFirstChild({})", lua_quote(seg, quote)));
            }

            IndexingStyle::WaitForChild => {
                expr.push_str(&format!(":WaitForChild({})", lua_quote(seg, quote)));
            }
        }
    }
}

/// A valid Luau identifier that is not a keyword (usable after a dot)
pub(super) fn is_luau_ident(name: &str) -> bool {
    const KEYWORDS: [&str; 21] = [
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in",
        "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ];
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !KEYWORDS.contains(&name)
}

/// A quoted Luau string literal with the chosen quote character
pub fn lua_quote(name: &str, quote: char) -> String {
    let escaped = name
        .replace('\\', "\\\\")
        .replace(quote, &format!("\\{quote}"));
    format!("{quote}{escaped}{quote}")
}
