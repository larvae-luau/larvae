//! The logo art and its brand gradient. Regenerate the art from
//! assets/larvae.png with ImageMagick when the logo changes.

use crate::ui::{BRAND, RESET, fg};

/// The full size logo with the brand gradient. See [`render_gradient`].
pub fn logo(color: bool) -> String {
    render_gradient(LOGO, color)
}

/// The compact logo for the fastfetch style side by side help layout
pub fn logo_small(color: bool) -> String {
    render_gradient(LOGO_SMALL, color)
}

/// The color of one gradient row. The ramp goes from pale mint, through
/// the brand color, to deep green.
pub fn row_color(row: usize, rows: usize) -> (u8, u8, u8) {
    const TOP: (u8, u8, u8) = (0xDE, 0xFF, 0xF0); // near white mint
    const BOTTOM: (u8, u8, u8) = (0x0F, 0x9E, 0x6D); // deep green, matches ui::DEEP_FG

    let last = rows.saturating_sub(1).max(1) as u32;
    let t = row.min(rows.saturating_sub(1)) as u32 * 1000 / last; // 0..=1000
    let mix = |a: u8, b: u8, t: u32, span: u32| -> u8 {
        let a = a as i32;
        let b = b as i32;

        (a + (b - a) * t as i32 / span as i32) as u8
    };

    if t <= 500 {
        (
            mix(TOP.0, BRAND.0, t, 500),
            mix(TOP.1, BRAND.1, t, 500),
            mix(TOP.2, BRAND.2, t, 500),
        )
    } else {
        (
            mix(BRAND.0, BOTTOM.0, t - 500, 500),
            mix(BRAND.1, BOTTOM.1, t - 500, 500),
            mix(BRAND.2, BOTTOM.2, t - 500, 500),
        )
    }
}

/// Paint the art with the gradient. A denser ramp character is brighter.
/// The output is plain text when color is off.
fn render_gradient(art: &str, color: bool) -> String {
    let art = art.trim_matches('\n');

    if !color {
        return art.to_owned();
    }

    // Density shading. A denser ramp character gets a brighter color.
    fn density(c: char) -> Option<u16> {
        match c {
            '.' => Some(45),

            ':' | '-' => Some(60),

            '=' | '+' => Some(78),

            '*' | '#' => Some(92),

            '%' | '@' => Some(100),

            _ => None,
        }
    }

    let lines: Vec<&str> = art.lines().collect();
    let rows = lines.len();
    let mut out = String::with_capacity(art.len() * 3);
    let mut current: Option<(u8, u8, u8)> = None;

    for (row, line) in lines.iter().enumerate() {
        let row_rgb = row_color(row, rows);

        for ch in line.chars() {
            let want = density(ch).map(|pct| {
                let scale = |v: u8| (v as u16 * pct / 100) as u8;
                (scale(row_rgb.0), scale(row_rgb.1), scale(row_rgb.2))
            });

            if want != current {
                match want {
                    Some(rgb) => out.push_str(&fg(rgb)),

                    None => out.push_str(RESET),
                }

                current = want;
            }

            out.push(ch);
        }

        if current.is_some() {
            out.push_str(RESET);
            current = None;
        }

        out.push('\n');
    }

    // The art has no final newline, so remove the newline added above.
    out.pop();

    out
}

pub const LOGO_SMALL: &str = r#"
                   .::::.
               .=***++++***=.
              *#+==**++**==+#*
             #*==+#:    :#+==*#
            .@==+@        @+==@.
             @+==#=      =#==+@
             :@+==**=--=**==+@:
               +#*++++++++*#+
                 :=++++++=:
      :=++++++=:            :=++++++=:
    +#*++++++++*#+        +#*++++++++*#+
  :@+==**=--=**==+@:    :@+==**=--=**==+@:
  @+==#=      =#==+@    @+==#=      =#==+@
 .@==+@        @+==@.  .@==+@        @+==@.
  #*==+#:    :#+==*#    #*==+#:    :#+==*#
   *#+==**++**==+#*      *#+==**++**==+#*
    .+***++++***+.        .+***++++***+.
        .::::.                .::::.
"#;

pub const LOGO: &str = r#"
                       -+**####**+-
                    -##*++======++*##-
                  :%#+===+*####*+===+#%:
                 -@+===+%*-.  .-*%+===+@-
                 @*===+@-        -@+===*@
                :@+===#@          @#===+@:
                 @*===+@+        +@+===*@
                 :@*===+##=:..:=##+===*@:
                  .#%+====**##**====+%#.
                    :*##*+======+*##*:
                       :=+******+=:
         -=+*****+=-                  -=+*****+=-
      =##*++=====+**##=            =##**+=====++*##=
    =@#+===+*###*+===+#@=        =@#+===+*###*+===+#@=
   #@+===*%+-...-+%*===+@*      *@+===*%+-...-+%*===+@#
  =@+===#%.       .%#===+@=    =@+===#%.       .%#===+@=
  ##===+@-         -@+===##    ##===+@-         -@+===##
  +@====##         ##====@+    +@====##         ##====@+
   %#====#%=.   .=%#====#%      %#====#%=.   .=%#====#%
    *@*====*#####*====*@*        *@*====*#####*====*@*
     .*##*+=======+*##+.          .+##*+=======+*##*.
        :=+*#####*+=.                .=+*#####*+=:
"#;
