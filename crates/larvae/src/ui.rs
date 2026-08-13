//! All the theming. The brand color is defined only here.

use std::io::IsTerminal;

/// The brand color of larvae, #10E694
pub const BRAND: (u8, u8, u8) = (0x10, 0xE6, 0x94);

/// The ANSI truecolor foreground escape for the brand color
pub const BRAND_FG: &str = "\x1b[38;2;16;230;148m";
pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";

/// The dark end of the gradient, #0F9E6D. Error accents use this color
/// instead of a red that does not match the theme.
pub const DEEP_FG: &str = "\x1b[38;2;15;158;109m";
/// The darker steps of the same green, for the secondary lines of a diagnostic
pub const DARK_FG: &str = "\x1b[38;2;11;118;81m";
pub const DARKER_FG: &str = "\x1b[38;2;8;89;61m";

/// The ANSI truecolor foreground escape for an RGB triple
pub fn fg((r, g, b): (u8, u8, u8)) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

/// Paint the text in the brand color. This does nothing when `color` is false.
pub fn accent(text: &str, color: bool) -> String {
    if color {
        format!("{BRAND_FG}{text}{RESET}")
    } else {
        text.to_owned()
    }
}

/// Use color only for a real terminal, and obey NO_COLOR
pub fn want_color() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

/// Diagnostics and summaries go to stderr, so larvae gates their color separately
pub fn want_color_stderr() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
}

/// The clap help styling, in the brand color
pub fn help_styles() -> clap::builder::Styles {
    use clap::builder::styling::{Color, RgbColor, Style, Styles};
    let brand = Color::Rgb(RgbColor(BRAND.0, BRAND.1, BRAND.2));
    Styles::styled()
        .header(Style::new().bold().fg_color(Some(brand)))
        .usage(Style::new().bold().fg_color(Some(brand)))
        .literal(Style::new().fg_color(Some(brand)))
        .placeholder(Style::new().dimmed())
}

/// The bold only styling, for text that an other function paints
pub fn bold_styles() -> clap::builder::Styles {
    use clap::builder::styling::{Style, Styles};
    Styles::styled()
        .header(Style::new().bold())
        .usage(Style::new().bold())
        .literal(Style::new().bold())
}

/// The terminal width. An explicit COLUMNS wins, then the tty, then a
/// reasonable default.
pub fn term_width() -> usize {
    if let Some(cols) = std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()) {
        return cols;
    }

    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(100)
}

/// The path relative to the working directory when that is possible. A
/// relative path keeps the output short.
/*
A path shown relative to the directory where the user ran the command.

Larvae resolves the working directory one time for the process, and not once
per call. The earlier code made two syscalls every time, plus a third syscall
to canonicalize the path it showed. A lint run over a large project calls this
function one time per diagnostic. Measured on a corpus that produced 3952
diagnostics, this cost was most of the run: 68ms to 38ms.

Larvae still canonicalizes the path when the plain form does not match. A
canonical path is how a path through a symlink, or a path with `..` in it,
becomes short. This step now runs only when it can change the answer.
*/
pub fn rel(path: &std::path::Path) -> String {
    static CWD: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();

    let cwd = CWD.get_or_init(|| {
        std::env::current_dir()
            .ok()
            .and_then(|c| c.canonicalize().ok())
    });

    let Some(cwd) = cwd else {
        return path.display().to_string();
    };

    let shown = |stripped: &std::path::Path| {
        let s = stripped.display().to_string();

        if s.is_empty() { ".".to_owned() } else { s }
    };

    if let Ok(stripped) = path.strip_prefix(cwd) {
        return shown(stripped);
    }

    if let Some(canonical) = path.canonicalize().ok()
        && let Ok(stripped) = canonical.strip_prefix(cwd)
    {
        return shown(stripped);
    }

    path.display().to_string()
}

/// The visible width of a line. The count ignores ANSI escape sequences.
pub fn visible_width(line: &str) -> usize {
    let mut width = 0;
    let mut chars = line.chars();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for e in chars.by_ref() {
                if e == 'm' {
                    break;
                }
            }
        } else {
            width += 1;
        }
    }

    width
}

/// Print `✓ message` in the brand color, to stderr
pub fn print_success(message: &str) {
    let color = want_color_stderr();
    eprintln!("{} {message}", accent("✓", color));
}

/// Print `✗ message` with the dark error accent, to stderr
pub fn print_error(message: &str) {
    if want_color_stderr() {
        eprintln!("{DEEP_FG}{BOLD}✗{RESET} {message}");
    } else {
        eprintln!("✗ {message}");
    }
}

/// A themed y/N prompt on stderr. Empty input, or no tty, selects the default.
pub fn confirm(question: &str, default: bool) -> bool {
    use std::io::Write;

    if !std::io::stdin().is_terminal() {
        return default;
    }

    let color = want_color_stderr();
    let hint = if default { "[Y/n]" } else { "[y/N]" };

    eprint!("{} {question} {hint} ", accent("?", color));

    let _ = std::io::stderr().flush();
    let mut line = String::new();

    if std::io::stdin().read_line(&mut line).is_err() {
        return default;
    }

    match line.trim().to_lowercase().as_str() {
        "y" | "yes" => true,

        "n" | "no" => false,

        "" => default,

        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_width_ignores_ansi() {
        assert_eq!(visible_width("abc"), 3);
        assert_eq!(visible_width("\x1b[38;2;1;2;3mab\x1b[0mc"), 3);
        assert_eq!(visible_width(""), 0);
    }

    #[test]
    fn accent_gates_on_color() {
        assert_eq!(accent("x", false), "x");
        assert!(accent("x", true).contains("38;2;16;230;148"));
    }
}
