/*!
Require resolution and rewrite emission. The input follows the Luau RFCs,
and the output is the configured target. A bad alias or a realm violation
always gives an error. A missing target gives a warning, unless strict is
set.
*/

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::config::{IndexingStyle, Target};
use crate::diag::Diag;
use crate::project::luaurc::LuaurcIndex;
use crate::requires::datamodel::{
    DmPath, MountTable, Realm, ScriptKind, script_instance_name, script_kind,
};

mod alias;
pub(crate) mod emit;
mod instance;
mod spec;
mod validate;

pub use emit::lua_quote;
use spec::*;

pub struct Resolver<'a> {
    /// Absolute project root
    pub root: &'a Path,
    /// `larvae.toml` aliases, with lowercase keys
    pub toml_aliases: &'a HashMap<String, String>,
    pub luaurc: &'a LuaurcIndex,
    pub mounts: &'a MountTable,
    pub target: Target,
    /// Child indexing for the roblox-instance target
    pub style: IndexingStyle,
    /// The quote character for generated string literals
    pub quote: char,
    pub strict: bool,
}

/// The context for one file, computed once
pub struct FileCtx<'a> {
    /// The absolute path of the file in work
    pub path: &'a Path,
    pub dir: PathBuf,
    pub is_init: bool,
    pub kind: ScriptKind,
    pub dm: Option<DmPath>,
    /// The output form for this file; an override can move it off the default
    pub target: Target,
    pub style: IndexingStyle,
    /*
    The modules this file resolved a require to, for the require graph.

    The list lives here, not as one more `&mut` beside `diags`, because a
    worker builds a `FileCtx` per file and shares nothing. So a `RefCell`
    costs nothing, and every resolver function that can resolve a module
    already holds the context.
    */
    pub required: std::cell::RefCell<Vec<PathBuf>>,
}

impl<'a> FileCtx<'a> {
    pub fn new(path: &'a Path, mounts: &MountTable, target: Target, style: IndexingStyle) -> Self {
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let is_init = script_instance_name(file_name).is_none();
        let dir = path.parent().unwrap_or(Path::new("")).to_owned();

        Self {
            path,
            required: std::cell::RefCell::default(),
            dm: mounts.dm_of(path),
            is_init,
            kind: script_kind(file_name),
            dir,
            target,
            style,
        }
    }

    /// The directory that ./ resolves against; an init file resolves from the parent of its directory
    fn dot_base(&self) -> &Path {
        if self.is_init {
            self.dir.parent().unwrap_or(&self.dir)
        } else {
            &self.dir
        }
    }

    /// The effective runtime realm of this file
    fn realm(&self) -> Option<Realm> {
        match self.kind {
            ScriptKind::Server => Some(Realm::ServerOnly),

            ScriptKind::Client => Some(Realm::StarterClone),

            ScriptKind::Module => self.dm.as_ref().map(|d| d.realm()),
        }
    }
}

pub enum Rewrite {
    Keep,
    /// Replace the require spec (the text between the quotes)
    Replace(String),
    /// Replace the whole argument with an instance expression
    Expr(String),
}

/// The filesystem node that a require resolved to
#[derive(Debug, PartialEq, Eq)]
enum ModuleNode {
    /// A plain module file (`foo.luau`)
    File(PathBuf),
    /// A directory module (`foo/init.luau` exists); the *directory* is the node
    Dir(PathBuf),
}

impl ModuleNode {
    fn dm_key_path(&self) -> &Path {
        match self {
            ModuleNode::File(p) | ModuleNode::Dir(p) => p,
        }
    }
}

impl<'a> Resolver<'a> {
    /// Resolve one require spec; returns the rewrite decision and pushes diagnostics
    pub fn resolve(
        &self,
        ctx: &FileCtx,
        spec: &str,
        src: &str,
        offset: usize,
        diags: &mut Vec<Diag>,
    ) -> Rewrite {
        if let Some(rest) = strip_alias(spec, "self") {
            return self.resolve_self(ctx, spec, rest, src, offset, diags);
        }

        if let Some(rest) = strip_alias(spec, "game") {
            return self.resolve_game_passthrough(ctx, rest, src, offset, diags);
        }

        if let Some(alias_rest) = spec.strip_prefix('@') {
            return self.resolve_alias(ctx, spec, alias_rest, src, offset, diags);
        }

        if spec.starts_with("./") || spec.starts_with("../") {
            let base = ctx.dot_base().to_owned();
            let Some(target_base) = normalize_join(&base, spec) else {
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

        diags.push(
            Diag::error(
                ctx.path,
                format!(
                    "require(\"{spec}\") is not RFC-valid: paths must start with ./, ../, or @alias"
                ),
            )
            .at(src, offset)
            .with_help("write it as an explicitly relative path or define an alias"),
        );

        Rewrite::Keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emit::{absolute_instance, is_luau_ident};

    #[test]
    fn instance_expressions() {
        let segs: Vec<String> = ["ReplicatedStorage", "shared", "for"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            absolute_instance(IndexingStyle::Property, '"', &segs),
            "game.ReplicatedStorage.shared[\"for\"]"
        );
        assert_eq!(
            absolute_instance(IndexingStyle::FindFirstChild, '"', &segs),
            "game:GetService(\"ReplicatedStorage\"):FindFirstChild(\"shared\"):FindFirstChild(\"for\")"
        );
        assert_eq!(
            absolute_instance(IndexingStyle::WaitForChild, '"', &segs),
            "game:GetService(\"ReplicatedStorage\"):WaitForChild(\"shared\"):WaitForChild(\"for\")"
        );
    }

    #[test]
    fn ident_rules() {
        assert!(is_luau_ident("math"));
        assert!(is_luau_ident("_Foo2"));
        assert!(!is_luau_ident("for"));
        assert!(!is_luau_ident("2fast"));
        assert!(!is_luau_ident("has space"));
        assert!(!is_luau_ident(""));
    }

    #[test]
    fn spec_helpers() {
        assert_eq!(strip_alias("@self/x", "self"), Some("x"));
        assert_eq!(strip_alias("@Self", "self"), Some(""));
        assert_eq!(strip_alias("@selfish/x", "self"), None);
        assert_eq!(split_alias("pkg/a/b"), ("pkg", "a/b"));
        assert_eq!(split_alias("pkg"), ("pkg", ""));
        assert_eq!(join_specs("a/", "b"), "a/b");
    }

    #[test]
    fn normalize() {
        assert_eq!(
            normalize_join(Path::new("/a/b"), "../c/./d").unwrap(),
            PathBuf::from("/a/c/d")
        );
        assert!(normalize_join(Path::new("/"), "../..").is_none());
    }

    #[test]
    fn fs_relative_paths() {
        assert_eq!(fs_relative(Path::new("/a/b"), Path::new("/a/b/c")), "./c");
        assert_eq!(
            fs_relative(Path::new("/a/b"), Path::new("/a/x/y")),
            "../x/y"
        );
        assert_eq!(fs_relative(Path::new("/a/b/c"), Path::new("/a")), "../..");
    }
}
