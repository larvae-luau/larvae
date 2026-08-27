/*!
The decorations that an editor draws on top of a document: a link on every
require, and a swatch on every colour literal.

Both answers come from larvae's own parser. Neither needs a type checker,
because both read literal syntax: the text between the quotes of a require,
and the numbers inside a `Color3` constructor. A value that the file does not
state, for example `Color3.fromRGB(shade, 0, 0)`, gets no decoration. A guess
there paints the wrong colour, and the user trusts the swatch.

The functions here are pure. Each one takes source text and returns spans in
bytes. The server turns those spans into protocol ranges and owns the
capability list.
*/

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::{IndexingStyle, Target};
use crate::lint::LintConfig;
use crate::lint::ctx::LintCtx;
use crate::lint::lints::roblox::{constructor, number};
use crate::project::luaurc::LuaurcIndex;
use crate::requires::datamodel::MountTable;
use crate::requires::resolve::{FileCtx, Resolver};
use crate::syntax::ast::Expr;
use crate::syntax::lexer::{self, TokKind};
use crate::syntax::parser;

/// One `require("...")` that names a file, and the file it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The byte range of the spec, between the quotes and not including them.
    pub range: (u32, u32),
    /// The module on disk. A directory module resolves to its init file.
    pub target: PathBuf,
}

/// The constructor that wrote a colour. A presentation offers this form first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// `Color3.new(r, g, b)`, channels from 0 to 1.
    New,
    /// `Color3.fromRGB(r, g, b)`, channels from 0 to 255.
    FromRgb,
    /// `Color3.fromHex("#RRGGBB")`.
    FromHex,
}

/// One colour literal, with its channels on the 0 to 1 scale the protocol uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorInfo {
    /// The byte range of the whole constructor call, which an edit replaces.
    pub range: (u32, u32),
    /*
    The form the author wrote.

    The server passes it back to [`color_presentation`], so an edit keeps the
    spelling that the file already uses.
    */
    pub form: Form,
    pub red: f32,
    pub green: f32,
    pub blue: f32,
}

// --- document links ---------------------------------------------------------

/*
A link on every require that names a module on disk.

The resolution is the resolver of `larvae check`, not a second reader of the
same paths. Two readers drift, and then the editor opens one file while the
build reads another. So this function builds the same [`Resolver`] and asks
it, and the answer is the module that the resolver recorded for the require
graph.

The diagnostics of the resolver are dropped here. A require that does not
resolve is a warning that the editor already shows, from `larvae check`. A
second report of it as a dead link says nothing new.
*/
pub fn links(src: &str, path: &Path, root: &Path) -> Vec<Link> {
    links_with_aliases(src, path, root, &HashMap::new())
}

/*
The same links, with the `[aliases]` table of `larvae.toml`.

The plain [`links`] reads `.luaurc` alone, because it holds no config. A
caller that already loaded the config passes the table here, and then an
alias that only `larvae.toml` defines gets a link too.
*/
pub fn links_with_aliases(
    src: &str,
    path: &Path,
    root: &Path,
    toml_aliases: &HashMap<String, String>,
) -> Vec<Link> {
    let Ok(lexed) = lexer::lex(src) else {
        return Vec::new();
    };

    let Ok(chunk) = parser::parse(src, &lexed.toks) else {
        return Vec::new();
    };

    let cfg = LintConfig::default();
    let ctx = LintCtx::new(src, &lexed.toks, &lexed.comments, &chunk, &cfg);
    let scanned = crate::syntax::scan::scan(src, &lexed.toks);

    let luaurc = luaurc_upward(path, root);
    let mounts = MountTable::default();

    /*
    The path target, because it is the one form that needs no DataModel.

    The rewrite that the resolver returns is thrown away. Only the module it
    found is kept, and the resolver records that for every target at the one
    place where a require becomes a file. So the cheapest target gives the
    same answer, and an editor that opens a project with no Rojo file still
    gets its links.
    */
    let resolver = Resolver {
        root,
        toml_aliases,
        luaurc: &luaurc,
        mounts: &mounts,
        target: Target::Path,
        style: IndexingStyle::default(),
        quote: '"',
        strict: false,
        claimed: &[],
        // Links only resolve; the relative rewrite changes what a build emits.
        client_relative_requires: false,
    };

    let file = FileCtx::new(path, &mounts, Target::Path, IndexingStyle::default());
    let mut out = Vec::new();

    for site in &scanned.sites {
        // A file that binds its own `require` does not call the global one.
        if !ctx.names.is_global(site.require_idx as u32) {
            continue;
        }

        let spec = &src[site.inner_start as usize..site.inner_end as usize];

        /*
        The resolver appends to `required` when a require resolves. So the
        entry at the old length, when one exists, is the module of this site.
        `larvae process` reads the answer the same way.
        */
        let before = file.required.borrow().len();
        let mut diags = Vec::new();
        resolver.resolve(&file, spec, src, site.at as usize, &mut diags);

        let Some(target) = file.required.borrow().get(before).cloned() else {
            continue;
        };

        let Some(target) = openable(&target) else {
            continue;
        };

        out.push(Link {
            range: (site.inner_start, site.inner_end),
            target,
        });
    }

    out
}

/*
The file that an editor opens for a resolved module.

The resolver keys a directory module on its directory, because the require
graph needs one node per module. An editor cannot open a directory, so the
init file inside it is the link target.
*/
fn openable(target: &Path) -> Option<PathBuf> {
    if target.is_file() {
        return Some(target.to_path_buf());
    }

    ["luau", "lua"]
        .iter()
        .map(|ext| target.join(format!("init.{ext}")))
        .find(|p| p.is_file())
}

/*
The `.luaurc` files that cover one document, from its own directory upward.

`larvae process` indexes every `.luaurc` of the project, because it resolves
every file. The server resolves one file per request, and a lookup only ever
walks up from that file. So this walk stops at the root, and an editor does
not read the whole tree on each keystroke.
*/
pub(super) fn luaurc_upward(path: &Path, root: &Path) -> LuaurcIndex {
    let mut index = LuaurcIndex::new(root);
    let mut dir = path.parent();

    while let Some(current) = dir {
        let file = current.join(".luaurc");

        if file.is_file() {
            // A broken .luaurc gives no aliases here. `larvae check` reports it.
            let _ = index.add_file(&file);
        }

        if current == root {
            break;
        }

        dir = current.parent();
    }

    index
}

// --- document colours -------------------------------------------------------

/*
Every colour literal in the file, on the 0 to 1 scale of the protocol.

A colour is reported only when the file states each channel. A variable or a
call as an argument gives nothing, because larvae does not evaluate the file,
and the swatch has to show the colour that runs.

`Color3` has to be a global, for the same reason the Roblox lints require it:
a local of that name belongs to the author, and larvae knows nothing about
what it constructs.
*/
pub fn colors(src: &str) -> Vec<ColorInfo> {
    let Ok(lexed) = lexer::lex(src) else {
        return Vec::new();
    };

    let Ok(chunk) = parser::parse(src, &lexed.toks) else {
        return Vec::new();
    };

    let cfg = LintConfig::default();
    let ctx = LintCtx::new(src, &lexed.toks, &lexed.comments, &chunk, &cfg);
    let mut out = Vec::new();

    for e in &ctx.exprs {
        let Some((path, args, span)) = constructor(&ctx, e) else {
            continue;
        };

        let Some(form) = form_of(&path) else {
            continue;
        };

        let Some((red, green, blue)) = channels(&ctx, form, args) else {
            continue;
        };

        out.push(ColorInfo {
            range: ctx.bytes(span),
            form,
            red,
            green,
            blue,
        });
    }

    out
}

fn form_of(path: &str) -> Option<Form> {
    match path {
        "Color3.new" => Some(Form::New),

        "Color3.fromRGB" => Some(Form::FromRgb),

        "Color3.fromHex" => Some(Form::FromHex),

        _ => None,
    }
}

/*
The three channels of one constructor call, or nothing.

An out of range value is clamped. `Color3.fromRGB(300, 0, 0)` renders as red
in Roblox, which clamps too, and the protocol takes 0 to 1. The
`roblox_incorrect_color3_new_bounds` lint reports the mistake itself.
*/
fn channels(ctx: &LintCtx<'_>, form: Form, args: &[Expr]) -> Option<(f32, f32, f32)> {
    let scaled = |v: f64, over: f64| (v / over).clamp(0.0, 1.0) as f32;

    match form {
        Form::New | Form::FromRgb => {
            let [r, g, b] = args else {
                return None;
            };

            let over = match form {
                Form::FromRgb => 255.0,

                _ => 1.0,
            };

            Some((
                scaled(number(ctx, r)?, over),
                scaled(number(ctx, g)?, over),
                scaled(number(ctx, b)?, over),
            ))
        }

        Form::FromHex => {
            let [Expr::String(span)] = args else {
                return None;
            };

            let TokKind::Str {
                inner_start,
                inner_end,
            } = ctx.toks[span.start as usize].kind
            else {
                return None;
            };

            hex(&ctx.src[inner_start as usize..inner_end as usize])
        }
    }
}

/*
`#RRGGBB` and `RRGGBB`, plus the three digit short form.

Roblox accepts all four spellings, so the swatch reads all four. No escape
sequence is a hex digit, so the raw text between the quotes is what the parse
reads.
*/
fn hex(text: &str) -> Option<(f32, f32, f32)> {
    let digits = text.strip_prefix('#').unwrap_or(text);

    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }

    let width = match digits.len() {
        3 => 1,

        6 => 2,

        _ => return None,
    };

    let byte_at = |i: usize| {
        let part = digits.get(i * width..i * width + width)?;
        let value = u8::from_str_radix(part, 16).ok()?;

        // A short digit repeats: `f` is `ff`, which is how CSS reads it too.
        Some(match width {
            1 => value * 17,

            _ => value,
        })
    };

    Some((
        byte_at(0)? as f32 / 255.0,
        byte_at(1)? as f32 / 255.0,
        byte_at(2)? as f32 / 255.0,
    ))
}

// --- colour presentations ---------------------------------------------------

/*
The replacement texts for a colour that the user picked from the swatch.

The form the author wrote comes first, because the editor applies the first
entry when the user drags in the picker. A `fromRGB` that turns into a `new`
on every drag rewrites a file that the author already spelled the way the
project spells it. The other two forms follow, so a deliberate change of
spelling stays one click away.
*/
pub fn color_presentation(red: f32, green: f32, blue: f32, original_form: Form) -> Vec<String> {
    let mut order = vec![original_form];

    order.extend(
        [Form::New, Form::FromRgb, Form::FromHex]
            .into_iter()
            .filter(|f| *f != original_form),
    );

    order
        .into_iter()
        .map(|form| match form {
            Form::New => format!(
                "Color3.new({}, {}, {})",
                scale(red),
                scale(green),
                scale(blue)
            ),

            Form::FromRgb => format!(
                "Color3.fromRGB({}, {}, {})",
                byte_of(red),
                byte_of(green),
                byte_of(blue)
            ),

            Form::FromHex => format!(
                "Color3.fromHex(\"#{:02X}{:02X}{:02X}\")",
                byte_of(red),
                byte_of(green),
                byte_of(blue)
            ),
        })
        .collect()
}

/// A 0 to 255 channel. The picker moves in those steps, so the rounding is exact.
fn byte_of(channel: f32) -> u8 {
    (channel.clamp(0.0, 1.0) * 255.0).round() as u8
}

/*
A 0 to 1 channel with the trailing zeros removed.

Three decimals hold one step of the picker, which is one part in 255. More
decimals write noise into the file: a full precision print of one byte gives
`Color3.new(0.501960813999176, 0, 0)`.
*/
fn scale(channel: f32) -> String {
    let text = format!("{:.3}", channel.clamp(0.0, 1.0));
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');

    match trimmed.is_empty() {
        true => "0".to_string(),

        false => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp dir");

        for (name, body) in files {
            let path = dir.path().join(name);

            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("the parent exists");
            }

            std::fs::write(&path, body).expect("the file writes");
        }

        dir
    }

    /// The editor sends every keystroke, so many requests arrive mid-edit.
    #[test]
    fn a_file_that_does_not_parse_gives_nothing() {
        let broken = "local x = Color3.fromRGB(255, 0,\nrequire(\"./a\"";

        assert!(colors(broken).is_empty());
        assert!(links(broken, Path::new("/p/main.luau"), Path::new("/p")).is_empty());
    }

    #[test]
    fn from_rgb_divides_by_255() {
        let found = colors("local red = Color3.fromRGB(255, 0, 0)");

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].red, 1.0);
        assert_eq!(found[0].green, 0.0);
        assert_eq!(found[0].blue, 0.0);
        assert_eq!(found[0].form, Form::FromRgb);
    }

    #[test]
    fn new_reads_the_channels_as_a_scale() {
        let found = colors("local c = Color3.new(1, 0.5, 0)");

        assert_eq!(found[0].red, 1.0);
        assert_eq!(found[0].green, 0.5);
        assert_eq!(found[0].form, Form::New);
    }

    #[test]
    fn the_range_covers_the_whole_call() {
        let src = "local c = Color3.new(1, 0, 0)";
        let found = colors(src);
        let (a, b) = found[0].range;

        assert_eq!(&src[a as usize..b as usize], "Color3.new(1, 0, 0)");
    }

    /// larvae does not evaluate the file, so it cannot know what `x` holds.
    #[test]
    fn a_name_argument_gives_no_colour() {
        assert!(colors("local c = Color3.fromRGB(x, 0, 0)").is_empty());
        assert!(colors("local c = Color3.new(shade(), 0, 0)").is_empty());
        assert!(colors("local c = Color3.fromHex(name)").is_empty());
    }

    /// A local of that name belongs to the author, not to Roblox.
    #[test]
    fn a_local_color3_is_not_the_roblox_one() {
        let src = "local Color3 = require(\"./mine\")\nlocal c = Color3.fromRGB(255, 0, 0)\n";

        assert!(colors(src).is_empty());
    }

    #[test]
    fn from_hex_reads_every_spelling() {
        let of = |text: &str| {
            let found = colors(&format!("local c = Color3.fromHex(\"{text}\")"));

            (found[0].red, found[0].green, found[0].blue)
        };

        assert_eq!(of("#FF0000"), (1.0, 0.0, 0.0));
        assert_eq!(of("00ff00"), (0.0, 1.0, 0.0));
        assert_eq!(of("#00f"), (0.0, 0.0, 1.0));
        assert!(colors("local c = Color3.fromHex(\"nothex\")").is_empty());
    }

    /// Roblox clamps too, and the protocol takes 0 to 1.
    #[test]
    fn an_out_of_range_channel_is_clamped() {
        let found = colors("local c = Color3.fromRGB(300, -20, 0)");

        assert_eq!((found[0].red, found[0].green), (1.0, 0.0));
    }

    #[test]
    fn the_written_form_comes_first() {
        let offered = color_presentation(1.0, 0.0, 0.0, Form::FromRgb);

        assert_eq!(offered.len(), 3);
        assert_eq!(offered[0], "Color3.fromRGB(255, 0, 0)");
        assert_eq!(
            color_presentation(1.0, 0.0, 0.0, Form::FromHex)[0],
            "Color3.fromHex(\"#FF0000\")"
        );
        assert_eq!(
            color_presentation(1.0, 0.0, 0.0, Form::New)[0],
            "Color3.new(1, 0, 0)"
        );
    }

    /*
    A colour that the user picks has to survive the write and the read.

    The editor writes one presentation and asks for the colours again. A
    channel that shifts on that trip moves the swatch off the pick.
    */
    #[test]
    fn a_colour_survives_a_round_trip() {
        let start = colors("local c = Color3.fromRGB(255, 128, 64)")[0];

        for form in [Form::New, Form::FromRgb, Form::FromHex] {
            for text in color_presentation(start.red, start.green, start.blue, form) {
                let again = colors(&format!("local c = {text}"));

                assert_eq!(again.len(), 1, "{text} reads back as one colour");
                assert!(
                    (again[0].red - start.red).abs() < 1e-3
                        && (again[0].green - start.green).abs() < 1e-3
                        && (again[0].blue - start.blue).abs() < 1e-3,
                    "{text} gives {:?}, not {start:?}",
                    again[0]
                );
            }
        }
    }

    #[test]
    fn a_relative_require_links_to_its_file() {
        let dir = tree(&[
            ("src/main.luau", ""),
            ("src/helper.luau", "return {}"),
            ("shared/util.luau", "return {}"),
        ]);
        let root = dir.path();
        let src = "local a = require(\"./helper\")\nlocal b = require(\"../shared/util\")\n";
        let found = links(src, &root.join("src/main.luau"), root);

        assert_eq!(found.len(), 2);
        assert_eq!(found[0].target, root.join("src/helper.luau"));
        assert_eq!(found[1].target, root.join("shared/util.luau"));

        // The range is the spec alone, so the underline stops at the quotes.
        let (a, b) = found[0].range;
        assert_eq!(&src[a as usize..b as usize], "./helper");
    }

    /// A missing module is a `larvae check` warning. A dead link says it twice.
    #[test]
    fn a_require_that_does_not_resolve_gives_no_link() {
        let dir = tree(&[("src/main.luau", "")]);
        let root = dir.path();
        let src = "local a = require(\"./gone\")\nlocal b = require(\"@unknown/x\")\nlocal c = require(script.Parent.Foo)\n";

        assert!(links(src, &root.join("src/main.luau"), root).is_empty());
    }

    /// An editor cannot open a directory, so the link names the init file.
    #[test]
    fn a_directory_module_links_to_its_init_file() {
        let dir = tree(&[("src/main.luau", ""), ("src/pkg/init.luau", "return {}")]);
        let root = dir.path();
        let found = links("require(\"./pkg\")", &root.join("src/main.luau"), root);

        assert_eq!(found[0].target, root.join("src/pkg/init.luau"));
    }

    #[test]
    fn a_luaurc_alias_resolves() {
        let dir = tree(&[
            (".luaurc", r#"{ "aliases": { "pkg": "./Packages" } }"#),
            ("src/main.luau", ""),
            ("Packages/Signal.luau", "return {}"),
        ]);
        let root = dir.path();
        let found = links(
            "require(\"@pkg/Signal\")",
            &root.join("src/main.luau"),
            root,
        );

        assert_eq!(found[0].target, root.join("Packages/Signal.luau"));
    }

    /// The alias table of `larvae.toml` reaches the resolver the same way.
    #[test]
    fn a_toml_alias_resolves() {
        let dir = tree(&[("src/main.luau", ""), ("vendor/Signal.luau", "return {}")]);
        let root = dir.path();
        let aliases = HashMap::from([("pkg".to_string(), "./vendor".to_string())]);
        let found = links_with_aliases(
            "require(\"@pkg/Signal\")",
            &root.join("src/main.luau"),
            root,
            &aliases,
        );

        assert_eq!(found[0].target, root.join("vendor/Signal.luau"));
    }

    /// `@self` names the directory of an init file, and only an init file has one.
    #[test]
    fn self_resolves_inside_an_init_module() {
        let dir = tree(&[("pkg/init.luau", ""), ("pkg/child.luau", "return {}")]);
        let root = dir.path();
        let found = links(
            "require(\"@self/child\")",
            &root.join("pkg/init.luau"),
            root,
        );

        assert_eq!(found[0].target, root.join("pkg/child.luau"));
    }

    /// A file that binds its own `require` does not call the global one.
    #[test]
    fn a_local_require_gets_no_link() {
        let dir = tree(&[("src/main.luau", ""), ("src/helper.luau", "return {}")]);
        let root = dir.path();
        let src = "local require = load\nlocal a = require(\"./helper\")\n";

        assert!(links(src, &root.join("src/main.luau"), root).is_empty());
    }
}
