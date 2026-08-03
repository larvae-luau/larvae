//! All theming, the brand color lives only here

use std::io::IsTerminal;

/// larvae's brand color, #10E694
pub const BRAND: (u8, u8, u8) = (0x10, 0xE6, 0x94);

/// ANSI truecolor foreground escape for the brand color
pub const BRAND_FG: &str = "\x1b[38;2;16;230;148m";
pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";

/// Dark end of the gradient (#0F9E6D), error accents use this instead of an off theme red
pub const DEEP_FG: &str = "\x1b[38;2;15;158;109m";
/// Darker steps of the same green for a diagnostic's secondary lines
pub const DARK_FG: &str = "\x1b[38;2;11;118;81m";
pub const DARKER_FG: &str = "\x1b[38;2;8;89;61m";

/// ANSI truecolor foreground escape for an RGB triple
pub fn fg((r, g, b): (u8, u8, u8)) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

/// Paint text in the brand color (no-op when `color` is false)
pub fn accent(text: &str, color: bool) -> String {
    if color {
        format!("{BRAND_FG}{text}{RESET}")
    } else {
        text.to_owned()
    }
}

/// Color output only for a real terminal, and respect NO_COLOR
pub fn want_color() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

/// Diagnostics and summaries go to stderr, gate their color separately
pub fn want_color_stderr() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
}

/// clap help styling in the brand color
pub fn help_styles() -> clap::builder::Styles {
    use clap::builder::styling::{Color, RgbColor, Style, Styles};
    let brand = Color::Rgb(RgbColor(BRAND.0, BRAND.1, BRAND.2));
    Styles::styled()
        .header(Style::new().bold().fg_color(Some(brand)))
        .usage(Style::new().bold().fg_color(Some(brand)))
        .literal(Style::new().fg_color(Some(brand)))
        .placeholder(Style::new().dimmed())
}

/// Bold only styling for text something else paints
pub fn bold_styles() -> clap::builder::Styles {
    use clap::builder::styling::{Style, Styles};
    Styles::styled()
        .header(Style::new().bold())
        .usage(Style::new().bold())
        .literal(Style::new().bold())
}

/// Terminal width, explicit COLUMNS wins, then the tty, then a sane default
pub fn term_width() -> usize {
    if let Some(cols) = std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()) {
        return cols;
    }

    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(100)
}

/// Path shown relative to the cwd when possible, keeps output short
pub fn rel(path: &std::path::Path) -> String {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|c| c.canonicalize().ok());
    if let Some(cwd) = cwd {
        for candidate in [path.to_path_buf()]
            .into_iter()
            .chain(path.canonicalize().ok())
        {
            if let Ok(stripped) = candidate.strip_prefix(&cwd) {
                let s = stripped.display().to_string();

                return if s.is_empty() { ".".to_owned() } else { s };
            }
        }
    }

    path.display().to_string()
}

/// Visible width of a line, ignoring ANSI escape sequences
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

/// `✓ message` in the brand color, to stderr
pub fn print_success(message: &str) {
    let color = want_color_stderr();
    eprintln!("{} {message}", accent("✓", color));
}

/// `✗ message` with the dark blue error accent, to stderr
pub fn print_error(message: &str) {
    if want_color_stderr() {
        eprintln!("{DEEP_FG}{BOLD}✗{RESET} {message}");
    } else {
        eprintln!("✗ {message}");
    }
}

/// Themed y/N prompt on stderr, empty input or no tty picks the default
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
