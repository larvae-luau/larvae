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

/*
A progress bar for a job with a known number of steps.

`larvae worm install` is the one caller. Installing a worm is a download, and
a download with no sign of life reads as a hang, so the bar exists to say that
something is happening and how much of it is left.

The bar draws only where a terminal will redraw it. Under a pipe or in CI it
prints one line per step instead, because a carriage return in a log file
leaves the whole run on one unreadable line.
*/
pub struct Progress {
    total: usize,
    done: usize,
    color: bool,
    /// Redraw in place. False under a pipe, where a line per step is right.
    live: bool,
    width: usize,
}

impl Progress {
    pub fn new(total: usize) -> Self {
        Self {
            total,
            done: 0,
            color: want_color_stderr(),
            live: std::io::IsTerminal::is_terminal(&std::io::stderr()),
            width: term_width().clamp(40, 100),
        }
    }

    /// Redraw with the step that is starting now.
    pub fn start(&self, label: &str) {
        self.draw(label, self.done);
    }

    /// Record a finished step, and say how it went.
    pub fn finish_step(&mut self, label: &str, note: &str) {
        self.done += 1;

        match self.live {
            true => {
                self.clear();
                eprintln!("  {} {label} {note}", accent("✓", self.color));
                self.draw("", self.done);
            }

            false => eprintln!("  {label} {note}"),
        }
    }

    /// Take the bar down. Every line printed after this starts clean.
    pub fn done(&self) {
        if self.live {
            self.clear();
        }
    }

    fn clear(&self) {
        use std::io::Write;

        let _ = write!(std::io::stderr(), "\r{}\r", " ".repeat(self.width));
        let _ = std::io::stderr().flush();
    }

    fn draw(&self, label: &str, done: usize) {
        use std::io::Write;

        if !self.live {
            if !label.is_empty() {
                eprintln!("  {label}...");
            }

            return;
        }

        // The bar takes what the counter and the label leave.
        let counter = format!(" {done}/{}", self.total);
        let room = self
            .width
            .saturating_sub(counter.len() + label.len() + 6)
            .clamp(10, 40);

        let filled = match self.total {
            0 => room,

            total => room * done / total,
        };

        let bar = format!(
            "{}{}",
            "█".repeat(filled),
            "░".repeat(room.saturating_sub(filled))
        );

        self.clear();

        let _ = write!(
            std::io::stderr(),
            "  {}{counter} {label}",
            accent(&bar, self.color)
        );
        let _ = std::io::stderr().flush();
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
