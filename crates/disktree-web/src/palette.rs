//! Colour that means something, for the web build.
//!
//! A line-for-line port of the desktop `disktree-app/src/palette.rs`, themed
//! by [`Appearance`]: the desktop follows the Omarchy theme, the browser's
//! equivalent is `prefers-color-scheme`, relayed by the shim. The built-in
//! pair is the desktop's own: Tokyo Night dark, Flexoki light — a tile means
//! the same colour on both front ends, in either appearance.

use disktree_core::classify::Category;

/// Dark or light: the browser's `prefers-color-scheme`, relayed by the shim.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    #[default]
    Dark,
    Light,
}

impl Appearance {
    /// One constant or the other.
    const fn pick(self, dark: u32, light: u32) -> u32 {
        match self {
            Self::Dark => dark,
            Self::Light => light,
        }
    }

    /// One constant or the other, as HSL.
    fn themed(self, dark: u32, light: u32) -> Hsl {
        hsl_of(self.pick(dark, light))
    }

    fn inset(self) -> Hsl {
        self.themed(theme::INSET, light::INSET)
    }

    fn bright(self) -> Hsl {
        self.themed(theme::BRIGHT, light::BRIGHT)
    }

    fn foreground(self) -> Hsl {
        self.themed(theme::BRIGHT, light::FOREGROUND)
    }

    fn accent(self) -> Hsl {
        self.themed(theme::ACCENT, light::ACCENT)
    }

    /// The danger colour, in this appearance.
    pub fn danger(self) -> Hsl {
        self.themed(theme::DANGER, light::DANGER)
    }

    /// The one strong colour, in this appearance.
    pub fn warning(self) -> Hsl {
        self.themed(theme::WARNING, light::WARNING)
    }
}

/// One colour in HSL, matching the desktop's colour space (`h` is 0..1).
#[derive(Clone, Copy, Debug)]
pub struct Hsl {
    pub h: f32,
    pub s: f32,
    pub l: f32,
    pub a: f32,
}

/// The theme constants the fills are computed from, as
/// `Theme::tokyo_night()` ships them. The rest of the theme lives in
/// `assets/style.css` verbatim, which is where a browser wants it.
pub mod theme {
    pub const INSET: u32 = 0x0013_141c;
    pub const BRIGHT: u32 = 0x00c0_caf5;
    pub const ACCENT: u32 = 0x007a_a2f7;
    pub const DANGER: u32 = 0x00f7_768e;
    pub const WARNING: u32 = 0x00e0_af68;
}

/// The light constants, as `Theme::flexoki_light()` ships them.
pub mod light {
    pub const INSET: u32 = 0x00e6_e4d9;
    pub const FOREGROUND: u32 = 0x0010_0f0f;
    pub const BRIGHT: u32 = 0x0010_0f0f;
    pub const ACCENT: u32 = 0x0020_5ea6;
    pub const DANGER: u32 = 0x00af_3029;
    pub const WARNING: u32 = 0x0085_5b00;
}

const fn rgb(hex: u32) -> (f32, f32, f32) {
    (
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    )
}

/// HSL of a theme constant, computed so hues (the accent's, for the age
/// ramp) stay exactly the theme's.
fn hsl_of(hex: u32) -> Hsl {
    let (r, g, b) = rgb(hex);
    from_rgb(r, g, b, 1.0)
}

/// RGB channels to HSL.
#[allow(
    clippy::many_single_char_names,
    reason = "h, s, l and r, g, b are the colour space's own vocabulary"
)]
fn from_rgb(r: f32, g: f32, b: f32, a: f32) -> Hsl {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = f32::midpoint(max, min);
    let span = max - min;
    if span < f32::EPSILON {
        return Hsl {
            h: 0.0,
            s: 0.0,
            l,
            a,
        };
    }
    let s = span / (1.0 - 2.0f32.mul_add(l, -1.0).abs()).max(f32::EPSILON);
    let h = if (max - r).abs() < f32::EPSILON {
        ((g - b) / span).rem_euclid(6.0)
    } else if (max - g).abs() < f32::EPSILON {
        (b - r) / span + 2.0
    } else {
        (r - g) / span + 4.0
    } / 6.0;
    Hsl { h, s, l, a }
}

/// `hsl()` as CSS, or `rgb()`/`rgba()` when the alpha is below opaque.
pub fn css(color: Hsl) -> String {
    let (r, g, b) = to_rgb(color);
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if color.a >= 0.999 {
        format!("#{:02x}{:02x}{:02x}", channel(r), channel(g), channel(b))
    } else {
        format!(
            "rgba({},{},{},{:.2})",
            channel(r),
            channel(g),
            channel(b),
            color.a
        )
    }
}

#[allow(
    clippy::many_single_char_names,
    reason = "c, x, m are the textbook names for the HSL-to-RGB intermediates"
)]
fn to_rgb(color: Hsl) -> (f32, f32, f32) {
    let c = (1.0 - 2.0f32.mul_add(color.l, -1.0).abs()) * color.s;
    let x = c * (1.0 - ((color.h * 6.0).rem_euclid(2.0) - 1.0).abs());
    let m = color.l - c / 2.0;
    let (r, g, b) = match (color.h * 6.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (r + m, g + m, b + m)
}

/// Linear interpolation in RGB, as the desktop's `mix`: interpolating hue
/// would drag a colour around the wheel on its way to a grey.
#[must_use]
pub fn mix(from: Hsl, to: Hsl, t: f32) -> Hsl {
    let t = t.clamp(0.0, 1.0);
    let lerp = |a: f32, b: f32| (b - a).mul_add(t, a);
    let (ar, ag, ab) = to_rgb(from);
    let (br, bg, bb) = to_rgb(to);
    // Back through HSL so callers can keep composing in that space.
    from_rgb(lerp(ar, br), lerp(ag, bg), lerp(ab, bb), lerp(from.a, to.a))
}

/// The hue a category is drawn in, and how much colour it carries. The
/// neutral kinds (documents, unknown) carry almost none.
const fn hue(category: Category) -> (f32, f32) {
    match category {
        Category::Code => (0.605, 1.0),
        Category::AgentScratch => (0.065, 1.0),
        Category::Toolchain => (0.415, 1.0),
        Category::Synced => (0.535, 1.0),
        Category::Git => (0.955, 1.0),
        Category::Media => (0.745, 1.0),
        Category::Cache => (0.125, 0.95),
        Category::Documents => (0.6, 0.18),
        Category::Other => (0.6, 0.08),
    }
}

/// The fill for a tile of `category`, `depth` levels into the view.
pub fn category_fill(
    appearance: Appearance,
    category: Category,
    depth: u32,
) -> Hsl {
    let (h, chroma) = hue(category);
    let step = depth.min(4) as f32;
    let (s, l) = match appearance {
        Appearance::Dark => (0.26 * chroma, step.mul_add(0.028, 0.215)),
        Appearance::Light => (0.30 * chroma, step.mul_add(-0.03, 0.84)),
    };
    mix(Hsl { h, s, l, a: 1.0 }, appearance.inset(), 0.12)
}

/// The saturated version of a category's hue: the strip over a top-level
/// directory and the legend swatch.
pub fn category_accent(appearance: Appearance, category: Category) -> Hsl {
    let (h, chroma) = hue(category);
    let (s, l) = match appearance {
        Appearance::Dark => (0.42 * chroma, 0.52),
        Appearance::Light => (0.45 * chroma, 0.46),
    };
    Hsl { h, s, l, a: 1.0 }
}

/// The age ramp, newest first: this week, this month, this half-year, this
/// year, older.
pub const AGE_BUCKETS: [(i64, &str); 5] = [
    (7, "This week"),
    (30, "This month"),
    (182, "Six months"),
    (365, "This year"),
    (i64::MAX, "Older"),
];

/// Which [`AGE_BUCKETS`] entry an age in days falls in.
pub fn age_bucket(days: i64) -> usize {
    AGE_BUCKETS
        .iter()
        .position(|(limit, _)| days <= *limit)
        .unwrap_or(AGE_BUCKETS.len() - 1)
}

/// The fill for age mode: recent writes carry the theme accent, and colour
/// drains out of a tile as it goes untouched.
pub fn age_fill(appearance: Appearance, bucket: usize, depth: u32) -> Hsl {
    let fade = bucket.min(AGE_BUCKETS.len() - 1) as f32 / 4.0;
    let step = depth.min(4) as f32;
    let (s, l) = match appearance {
        Appearance::Dark => (
            (1.0 - fade).mul_add(0.34, 0.03),
            step.mul_add(0.028, 0.29 - fade * 0.09),
        ),
        Appearance::Light => (
            (1.0 - fade).mul_add(0.36, 0.04),
            step.mul_add(-0.03, 0.74 + fade * 0.1),
        ),
    };
    mix(
        Hsl {
            h: appearance.accent().h,
            s,
            l,
            a: 1.0,
        },
        appearance.inset(),
        0.1,
    )
}

/// The age swatch for the legend.
pub fn age_accent(appearance: Appearance, bucket: usize) -> Hsl {
    let fade = bucket.min(AGE_BUCKETS.len() - 1) as f32 / 4.0;
    Hsl {
        h: appearance.accent().h,
        s: (1.0 - fade).mul_add(0.45, 0.04),
        l: match appearance {
            Appearance::Dark => 0.55 - fade * 0.25,
            Appearance::Light => 0.45 + fade * 0.25,
        },
        a: 1.0,
    }
}

/// The one strong colour: selection, the main action, what can be had back.
pub fn highlight(appearance: Appearance) -> Hsl {
    appearance.warning()
}

/// The diagonal hatch over reclaimable space: quiet enough to leave the hue
/// readable, visible on every fill.
pub fn hatch(appearance: Appearance) -> Hsl {
    match appearance {
        Appearance::Dark => Hsl {
            a: 0.16,
            ..appearance.bright()
        },
        Appearance::Light => Hsl {
            a: 0.18,
            ..appearance.themed(theme::BRIGHT, light::FOREGROUND)
        },
    }
}

/// A tile's name on top of its fill.
pub fn label_color(appearance: Appearance, depth: u32) -> Hsl {
    let base = appearance.foreground();
    if depth == 0 {
        base
    } else {
        Hsl { a: 0.88, ..base }
    }
}

/// The fill every marked or covered tile shares: the danger colour, quiet.
pub fn marked_fill(appearance: Appearance) -> Hsl {
    mix(appearance.inset(), appearance.danger(), 0.16)
}

/// A filtered-out fill steps back toward the inset surface.
pub fn filtered_fill(appearance: Appearance, fill: Hsl, out: bool) -> Hsl {
    mix(fill, appearance.inset(), if out { 0.82 } else { 0.55 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsl_round_trips_a_theme_colour() {
        // The accent's hue feeds the age ramp; a drift there would recolour
        // every age tile away from the desktop.
        let hsl = hsl_of(theme::ACCENT);
        let (r, g, b) = to_rgb(hsl);
        let channel = |v: f32| (v * 255.0).round() as u32;
        let back = (channel(r) << 16) | (channel(g) << 8) | channel(b);
        assert_eq!(back, theme::ACCENT, "{hsl:?}");
    }

    #[test]
    fn colourful_categories_share_one_level() {
        let code = category_fill(Appearance::Dark, Category::Code, 0);
        let git = category_fill(Appearance::Dark, Category::Git, 0);
        assert!((code.l - git.l).abs() < 0.02);
        assert!((code.s - git.s).abs() < 0.03);
    }

    #[test]
    fn deeper_tiles_lift_away_from_the_background() {
        for appearance in [Appearance::Dark, Appearance::Light] {
            let top = category_fill(appearance, Category::Code, 0);
            let deep = category_fill(appearance, Category::Code, 3);
            // Away from the background: lighter on dark, darker on light.
            let distance = |fill: Hsl| (fill.l - appearance.inset().l).abs();
            assert!(distance(deep) > distance(top) + 0.05, "{appearance:?}");
        }
    }

    #[test]
    fn the_highlight_is_not_a_category_colour() {
        let highlight = highlight(Appearance::Dark);
        for category in Category::LEGEND {
            let fill = category_fill(Appearance::Dark, category, 0);
            assert!(highlight.s - fill.s > 0.2, "{category:?} competes");
        }
    }

    #[test]
    fn age_buckets_cover_every_age_in_order() {
        assert_eq!(age_bucket(0), 0);
        assert_eq!(age_bucket(8), 1);
        assert_eq!(age_bucket(100), 2);
        assert_eq!(age_bucket(300), 3);
        assert_eq!(age_bucket(5000), 4);
        assert!(
            age_fill(Appearance::Dark, 0, 0).s
                > age_fill(Appearance::Dark, 4, 0).s
        );
    }

    #[test]
    fn opaque_colours_render_as_hex() {
        assert_eq!(css(hsl_of(theme::DANGER)), "#f7768e");
        assert_eq!(
            css(Hsl {
                a: 0.5,
                ..hsl_of(theme::WARNING)
            }),
            "rgba(224,175,104,0.50)",
        );
    }
}
