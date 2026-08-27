/*!
The generator: how a finished output text is printed.

Every transform in larvae is a byte edit against the source, so the spliced
output keeps the lines of the input. That is `retain-lines`, the default,
and it costs nothing. The other two generators re-print that spliced text as
a last step: `dense` through the minifying emitter, and `readable` through
the formatter with the `[fmt]` style of the project.

`larvae process` runs one generator per output file, and `larvae bundle`
runs the same generator once over the bundled file. The generator sees only
final text, so a worm output and a plain file print the same way.
*/

use std::borrow::Cow;
use std::path::Path;

use anyhow::Result;

use crate::config::Config;

pub enum Generator {
    /// The spliced text as it is; the output keeps the lines of the input
    RetainLines,

    /// The minifier, tuned by `[minify]`. With `obfuscate`, names and
    /// strings go through [`crate::obfuscate`] before the emitter runs.
    Dense { column_span: usize, obfuscate: bool },

    /// The formatter, with the `[fmt]` style of the project
    Readable(Box<crate::fmt::FmtConfig>),
}

impl Generator {
    /*
    Resolved once per run, not once per file. The readable arm reads the
    `[fmt]` table, and a stylua.toml under it, exactly as `larvae fmt`
    does. So `generator = "readable"` and a later `larvae fmt` agree, and
    a formatted output does not thrash between two styles.
    */
    pub fn from_config(root: &Path, config: &Config) -> Result<Self> {
        /*
        `obfuscate` prints through the dense emitter whatever `generator`
        says. A readable obfuscated file is a contradiction, and a project
        that asked for one is asking for the dense one.
        */
        if config.minify.obfuscate {
            return Ok(Self::Dense {
                column_span: config.minify.span(),
                obfuscate: true,
            });
        }

        Ok(match config.process.generator.as_str() {
            "dense" => Self::Dense {
                column_span: config.minify.span(),
                obfuscate: false,
            },

            "readable" => Self::Readable(Box::new(crate::fmt::FmtConfig::discover(
                root,
                config.fmt.as_ref(),
            )?)),

            // The config validated the name, so everything else is retain-lines.
            _ => Self::RetainLines,
        })
    }

    /// The printed text; an error names what failed and keeps the file out of it
    pub fn apply<'a>(&self, text: &'a str) -> Result<Cow<'a, str>, String> {
        match self {
            Self::RetainLines => Ok(Cow::Borrowed(text)),

            Self::Dense {
                column_span,
                obfuscate: false,
            } => crate::syntax::dense::dense(text, *column_span)
                .map(Cow::Owned)
                .map_err(|e| format!("the dense generator cannot lex the output: {e}")),

            /*
            Obfuscation needs a parse, because only a parse says which
            names are locals. So it fails on a file the dense emitter alone
            would have printed, and the message says which step stopped.
            */
            Self::Dense { column_span, .. } => crate::obfuscate::obfuscate(text, *column_span)
                .map(Cow::Owned)
                .map_err(|e| format!("the obfuscator cannot read the output: {e}")),

            Self::Readable(cfg) => crate::fmt::format(text, cfg)
                .map(Cow::Owned)
                .map_err(|e| format!("the readable generator cannot format the output: {e:#}")),
        }
    }
}
