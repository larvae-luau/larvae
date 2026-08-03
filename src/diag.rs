use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

/// A user facing diagnostic, line and col captured at creation so rendering needs no source
#[derive(Debug, Clone)]
pub struct Diag {
    pub severity: Severity,
    pub file: PathBuf,
    /// 1-based line and column, when the diagnostic points into a file
    pub line_col: Option<(u32, u32)>,
    pub message: String,
    pub help: Option<String>,
}

impl Diag {
    pub fn error(file: &Path, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            file: file.to_owned(),
            line_col: None,
            message: message.into(),
            help: None,
        }
    }

    pub fn warning(file: &Path, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(file, message)
        }
    }

    pub fn at(mut self, source: &str, byte_offset: usize) -> Self {
        self.line_col = Some(line_col(source, byte_offset));

        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());

        self
    }
}

impl Diag {
    fn location(&self) -> String {
        match self.line_col {
            Some((line, col)) => format!("{}:{line}:{col}", crate::ui::rel(&self.file)),

            None => crate::ui::rel(&self.file),
        }
    }

    /**
    Render as a small block instead of one long line

    ```text
    ✗ error: require("@nope/x"): unknown alias @nope
        at src/shared/bad.luau:1:8
        help: define it under [aliases] in larvae.toml or in a .luaurc
    ```

    Color stays on theme, dark blue head then darker steps below,
    warnings get the bright brand head
    */
    pub fn render(&self, color: bool) -> String {
        if !color {
            return self.to_string();
        }

        use crate::ui::{BOLD, BRAND_FG, DARK_FG, DARKER_FG, DEEP_FG, RESET};

        let (glyph, head_color, sev) = match self.severity {
            Severity::Error => ("✗", DEEP_FG, "error"),

            Severity::Warning => ("!", BRAND_FG, "warning"),
        };

        let mut out = format!("{head_color}{BOLD}{glyph} {sev}:{RESET} {}", self.message);
        out.push_str(&format!("\n    {DARK_FG}at {}{RESET}", self.location()));

        if let Some(help) = &self.help {
            out.push_str(&format!("\n    {DARKER_FG}help: {help}{RESET}"));
        }

        out
    }
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sev = match self.severity {
            Severity::Error => "error",

            Severity::Warning => "warning",
        };

        write!(f, "{sev}: {}", self.message)?;
        write!(f, "\n    at {}", self.location())?;

        if let Some(help) = &self.help {
            write!(f, "\n    help: {help}")?;
        }

        Ok(())
    }
}

pub fn line_col(source: &str, byte_offset: usize) -> (u32, u32) {
    let offset = byte_offset.min(source.len());
    let before = &source[..offset];
    let line = before.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = before[line_start..].chars().count() as u32 + 1;

    (line, col)
}

/// Sort diagnostics for deterministic output regardless of rayon scheduling
pub fn sort(diags: &mut [Diag]) {
    diags.sort_by(|a, b| {
        (&a.file, a.line_col, a.severity)
            .cmp(&(&b.file, b.line_col, b.severity))
            .then_with(|| a.message.cmp(&b.message))
    });
}
