//! Colours, type and corners. A dark or light base, one accent, and colour for state only where
//! something needs attention: speaking, live, muted, an error. Panes are rounded cards a step
//! above the page, with hairline edges instead of heavy shadows.
//!
//! The palettes are Ciranda's own (Midnight, Dark, Onyx…, plus a custom one built from four
//! colours) and a System palette that follows Windows: light or dark, and its accent colour.

use gpui::{Hsla, Rgba, WindowAppearance, rgb};
use std::cell::Cell;

/// Figtree: open, round and friendly at small sizes, the nearest free cut to Discord's gg sans.
pub const FONT: &str = "Figtree";
pub const MONO: &str = "Geist Mono";

/// The fonts the window draws with, so it looks the same everywhere.
pub fn fonts() -> Vec<std::borrow::Cow<'static, [u8]>> {
    vec![
        include_bytes!("../assets/fonts/Figtree-Regular.ttf").as_slice().into(),
        include_bytes!("../assets/fonts/Figtree-Medium.ttf").as_slice().into(),
        include_bytes!("../assets/fonts/Figtree-SemiBold.ttf").as_slice().into(),
        include_bytes!("../assets/fonts/Figtree-Bold.ttf").as_slice().into(),
        include_bytes!("../assets/fonts/GeistMono-Regular.ttf").as_slice().into(),
        include_bytes!("../assets/fonts/GeistMono-Medium.ttf").as_slice().into(),
        include_bytes!("../assets/fonts/GeistMono-SemiBold.ttf").as_slice().into(),
        // The name styles' (`crate::name_fx::styles`).
        include_bytes!("../assets/fonts/styles/LuckiestGuy-Regular.ttf").as_slice().into(),
        include_bytes!("../assets/fonts/styles/CinzelDecorative-Bold.ttf").as_slice().into(),
        include_bytes!("../assets/fonts/styles/PermanentMarker-Regular.ttf").as_slice().into(),
        include_bytes!("../assets/fonts/styles/ChakraPetch-SemiBold.ttf").as_slice().into(),
        include_bytes!("../assets/fonts/styles/Minecraft.otf").as_slice().into(),
        include_bytes!("../assets/fonts/styles/Rajdhani-Bold.ttf").as_slice().into(),
        include_bytes!("../assets/fonts/styles/Jura-SemiBold.ttf").as_slice().into(),
    ]
}

thread_local! {
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
    static CURRENT: Cell<Option<Theme>> = const { Cell::new(None) };
}

/// The theme the window is drawn in now; the root view sets it before each frame, and views
/// deeper down (text fields, popovers) read it.
pub fn set_current(t: Theme) {
    CURRENT.with(|c| c.set(Some(t)));
}

pub fn current() -> Theme {
    CURRENT.with(|c| c.get()).unwrap_or_else(|| Theme::new("midnight", WindowAppearance::Dark, None))
}

/// The interface size the person picked, 0.7 to 1.8.
pub fn set_scale(scale: f32) {
    SCALE.with(|s| s.set(scale.clamp(0.7, 1.8)));
}

pub fn scale() -> f32 {
    SCALE.with(|s| s.get())
}

/// Logical pixels at the interface size. Every length in the UI goes through this, so the size
/// control scales layout and type together, as the old client's zoom did.
pub fn px(v: f32) -> gpui::Pixels {
    gpui::px(v * scale())
}

/// Type ramp, as (size, line height) in pixels.
pub mod text {
    pub const LABEL: (f32, f32) = (11., 14.);
    pub const CAPTION: (f32, f32) = (12., 16.);
    pub const BODY: (f32, f32) = (14., 20.);
    pub const TITLE: (f32, f32) = (16., 22.);
    pub const DISPLAY: (f32, f32) = (30., 38.);
}

/// Corner radii: the larger the surface, the rounder.
pub mod radius {
    pub const PANE: f32 = 18.;
    pub const CARD: f32 = 15.;
    pub const CONTROL: f32 = 12.;
    /// A square icon button, a tooltip, a small plate over video.
    pub const ICON: f32 = 10.;
    pub const INNER: f32 = 9.;
}

/// The gap between panes and around the window's edge.
pub const GUTTER: f32 = 10.;

/// A palette as Ciranda defines one.
#[derive(Clone, Copy)]
struct Palette {
    bg: u32,
    surface: u32,
    surface2: u32,
    border: u32,
    border_strong: u32,
    text: u32,
    muted: u32,
    accent: u32,
    danger: u32,
    live: u32,
    warn: u32,
    video: u32,
    light: bool,
}

const fn pal(v: [u32; 12], light: bool) -> Palette {
    Palette {
        bg: v[0],
        surface: v[1],
        surface2: v[2],
        border: v[3],
        border_strong: v[4],
        text: v[5],
        muted: v[6],
        accent: v[7],
        danger: v[8],
        live: v[9],
        warn: v[10],
        video: v[11],
        light,
    }
}

/// Every palette: the name settings store, and the one people see. The order is the one the
/// picker shows.
pub fn palettes() -> [(&'static str, &'static str); 10] {
    [
        ("system", tr!("System", "Sistema")),
        ("midnight", tr!("Midnight", "Meia-noite")),
        ("dark", tr!("Dark", "Escuro")),
        ("onyx", tr!("Onyx", "Ônix")),
        ("ocean", tr!("Ocean", "Oceano")),
        ("forest", tr!("Forest", "Floresta")),
        ("ember", tr!("Ember", "Brasa")),
        ("lavender", tr!("Lavender", "Lavanda")),
        ("daylight", tr!("Daylight", "Dia claro")),
        ("custom", tr!("Custom", "Personalizada")),
    ]
}

fn palette(name: &str) -> Option<Palette> {
    Some(match name {
        "midnight" => pal(
            [0x0f1116, 0x171a21, 0x1f232c, 0x2a2f3a, 0x3b4252, 0xe6e8ee, 0x8b93a7, 0x6682ff, 0xe05561, 0x3fd07f, 0xe3a008, 0x07090d],
            false,
        ),
        "dark" => pal(
            [0x1e1f22, 0x2b2d31, 0x313338, 0x3f4147, 0x4e5058, 0xdbdee1, 0x949ba4, 0x5865f2, 0xda373c, 0x23a55a, 0xf0b132, 0x000000],
            false,
        ),
        "onyx" => pal(
            [0x000000, 0x0b0b0d, 0x141418, 0x26262c, 0x3a3a42, 0xe8e8ea, 0x8a8a93, 0x7c6cff, 0xe05561, 0x3fd07f, 0xe3a008, 0x000000],
            false,
        ),
        "forest" => pal(
            [0x0e1512, 0x15201b, 0x1c2a23, 0x273a30, 0x35503f, 0xe3ece6, 0x86a295, 0x3fb984, 0xe05561, 0x6ee7a8, 0xe3a008, 0x050a08],
            false,
        ),
        "ember" => pal(
            [0x17100e, 0x211714, 0x2c1f1a, 0x3d2a23, 0x553a30, 0xf0e5e0, 0xa89086, 0xff7a4d, 0xe05561, 0x4fd198, 0xf0b132, 0x0b0605],
            false,
        ),
        "lavender" => pal(
            [0x14111d, 0x1c1829, 0x252036, 0x342c4a, 0x493e66, 0xe9e4f5, 0x998fb5, 0xa78bfa, 0xe05561, 0x5fd3a6, 0xe3a008, 0x0a0810],
            false,
        ),
        "ocean" => pal(
            [0x0b141c, 0x111e29, 0x172836, 0x22394b, 0x2f4e67, 0xdfeaf2, 0x7f99ac, 0x38bdf8, 0xe05561, 0x3fd07f, 0xe3a008, 0x04090d],
            false,
        ),
        "daylight" => pal(
            [0xf2f3f5, 0xffffff, 0xebedef, 0xd4d7dc, 0xb6bbc4, 0x22262b, 0x5c6670, 0x0b2ecb, 0xc4303a, 0x1a9550, 0x9a6a00, 0x16181d],
            true,
        ),
        _ => return None,
    })
}

/// A gradient over the whole window, as a colour theme paints it. Panes are see-through and only
/// darken it, so it shows everywhere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Backdrop {
    /// As CSS has it: 0 runs up the window, 90 to the right, turning clockwise.
    pub angle: f32,
    /// Colours and where they sit along the gradient (0..=1), in order.
    pub stops: &'static [(u32, f32)],
}

impl Backdrop {
    /// The colour at `(x, y)` of a `w` by `h` area, as a CSS `linear-gradient` paints it.
    pub fn at(&self, x: f32, y: f32, w: f32, h: f32) -> u32 {
        let a = self.angle.to_radians();
        let (dx, dy) = (a.sin(), -a.cos());
        let len = (w * dx).abs() + (h * dy).abs();
        self.along(((x - w / 2.) * dx + (y - h / 2.) * dy) / len.max(1e-3) + 0.5)
    }

    /// The colour `t` of the way along the gradient.
    pub fn along(&self, t: f32) -> u32 {
        let s = self.stops;
        if t <= s[0].1 {
            return s[0].0;
        }
        for w in s.windows(2) {
            let ((a, at), (b, bt)) = (w[0], w[1]);
            if t <= bt {
                return mix(a, b, if bt > at { (t - at) / (bt - at) } else { 1. });
            }
        }
        s[s.len() - 1].0
    }

    pub fn first(&self) -> u32 {
        self.stops[0].0
    }

    pub fn last(&self) -> u32 {
        self.stops[self.stops.len() - 1].0
    }
}

/// A colour theme in the manner of Discord's: a gradient behind everything, the panes darkening
/// it, and the greys of Discord's dark theme tinted by its two ends.
#[derive(Clone, Copy)]
struct Scene {
    backdrop: Backdrop,
    /// How strongly the panes darken the gradient (Discord's overlay multiplier): lower shows
    /// more of it.
    dim: f32,
    accent: u32,
}

/// Every colour theme: the name settings store, and the one people see, in the picker's order.
pub fn scenes() -> [(&'static str, &'static str); 7] {
    [
        ("vicecity", "Vice City"),
        ("middleearth", tr!("Middle-earth", "Terra Média")),
        ("bullworth", "Bullworth"),
        ("division", tr!("Division", "Divisão")),
        ("blocks", tr!("Blocks", "Blocos")),
        ("nightcity", "Night City"),
        ("outerwilds", "Outer Wilds"),
    ]
}

fn scene(name: &str) -> Option<Scene> {
    let s = |angle, stops, dim, accent| Scene { backdrop: Backdrop { angle, stops }, dim, accent };
    Some(match name {
        // Discord's own, colour for colour (its HEXAGON preset), on its blurple.
        "vicecity" => s(135., &[(0x4645ff, 0.), (0xaf01f3, 0.3333), (0xff6698, 0.6667), (0xf89a03, 1.)], 0.8, 0x5865f2),
        // Night in an old forest, and a ring's gold.
        "middleearth" => s(160., &[(0x0b1f17, 0.), (0x1f4a35, 0.4), (0x8a6a2c, 0.78), (0xe2b85c, 1.)], 0.85, 0xd4a64a),
        // A boarding school in autumn: blazer navy, brick and mustard.
        "bullworth" => s(120., &[(0x1b2a55, 0.), (0x6b1f3a, 0.45), (0xc0392b, 0.75), (0xe8a33d, 1.)], 0.85, 0xe0533f),
        // A dark city in winter and an agent's orange.
        "division" => s(150., &[(0x0d0f12, 0.), (0x1f242b, 0.5), (0x7a3510, 0.82), (0xff6d10, 1.)], 0.85, 0xff6d10),
        // Sky, grass and dirt, top to bottom; its world of blocks is drawn over it.
        "blocks" => s(180., &[(0x7fb2ff, 0.), (0x9cc4ff, 0.55), (0x5f9b3a, 0.7), (0x79553a, 0.8), (0x3b2a1c, 1.)], 0.7, 0x62b33a),
        // Night City: a dark street under violet haze, its neon red, and the yellow the game made
        // its own (#fcee0a) in a corner.
        "nightcity" => s(135., &[(0x02030a, 0.), (0x0b1033, 0.3), (0x3a0b52, 0.55), (0xc4003a, 0.82), (0xfcee0a, 1.)], 0.85, 0xfcee0a),
        // Outer Wilds: deep space over Timber Hearth, the campfire's orange low in the corner.
        "outerwilds" => s(160., &[(0x05070f, 0.), (0x111a3a, 0.4), (0x2b2550, 0.66), (0x7a3a2a, 0.88), (0xef7b39, 1.)], 0.85, 0xef7b39),
        _ => return None,
    })
}

/// The gradient of a colour theme, for its swatch.
pub fn scene_backdrop(name: &str) -> Option<Backdrop> {
    scene(name).map(|s| s.backdrop)
}

/// Discord's dark greys, before a colour theme tints them.
mod discord {
    pub const TEXT: u32 = 0xf3f3f4;
    pub const MUTED: u32 = 0xabacb2;
    pub const FAINT: u32 = 0x999aa1;
    pub const CARD: u32 = 0x36373e;
    pub const POPOVER: u32 = 0x3c3d45;
    pub const BORDER: u32 = 0x97979f;
    pub const DANGER: u32 = 0xf23f43;
    pub const LIVE: u32 = 0x23a55a;
    pub const WARN: u32 = 0xf0b232;
}

/// The two tints Discord takes from a gradient's ends for its dark theme: a near-black of the
/// darker end's hue for surfaces, and a pale one of the lighter end's for text.
fn tints(a: u32, b: u32) -> (u32, u32) {
    let (light, dark) = if relative_luminance(a) > relative_luminance(b) { (a, b) } else { (b, a) };
    let dark_hue = to_hsl(dark)[0];
    let dark = if dark_hue == 0. || dark_hue.is_nan() { light } else { dark };
    let hue = to_hsl(light)[0];
    let hue = if hue.is_nan() { 0. } else { hue };
    let text = from_hsl([hue, 1., if (200. ..=300.).contains(&hue) { 0.8 } else { 0.95 }]);
    let base_hue = to_hsl(dark)[0];
    (from_hsl([if base_hue.is_nan() { 0. } else { base_hue }, 1., 0.05]), text)
}

impl Theme {
    fn from_scene(sc: Scene) -> Theme {
        let (tint, ink) = tints(sc.backdrop.first(), sc.backdrop.last());
        // Discord's `color-mix(in oklab, grey 100%, tint 50%)` for surfaces, 38% for text.
        let surface = |c| oklab_mix(c, tint, 50. / 150.);
        let text = |c| oklab_mix(c, ink, 38. / 138.);
        // The page darkens the gradient as Discord's frame does, a pane as its chat does; a
        // pane lies on the page, so it adds only the difference.
        let (frame, chat) = (0.7 * sc.dim, 0.8 * sc.dim);
        let pane = 1. - (1. - chat) / (1. - frame);
        let grey = |a: f32| gpui::hsla(240. / 360., 0.04, 0.6, a);
        let border = c(surface(discord::BORDER));
        let accent = c(sc.accent);
        let on_pane = Self::scene_panes(sc);
        let t = text(discord::TEXT);
        Theme {
            dark: true,
            base: gpui::black().opacity(frame),
            pane: gpui::black().opacity(pane),
            layer: grey(0.08),
            layer_hover: grey(0.12),
            control: grey(0.12),
            control_hover: grey(0.2),
            well: gpui::black().opacity(0.25),
            stroke: border.opacity(0.12),
            stroke_strong: border.opacity(0.44),
            text: c(t),
            text2: c(readable(text(discord::MUTED), t, &on_pane, 4.5)),
            text3: c(readable(text(discord::FAINT), t, &on_pane, 4.5)),
            accent,
            accent_soft: accent.opacity(0.16),
            accent_text: c(readable(sc.accent, t, &on_pane, 4.5)),
            on_accent: if luminance(sc.accent) > 0.55 { c(0x0b0c10) } else { gpui::white() },
            critical: c(discord::DANGER),
            quiet: c(mix(discord::DANGER, t, 0.28)),
            success: c(discord::LIVE),
            caution: c(discord::WARN),
            stage: c(mix(tint, 0x000000, 0.4)),
            scrim: gpui::black().opacity(0.55),
            popover: c(surface(discord::POPOVER)),
            card: c(surface(discord::CARD)),
            backdrop: Some(sc.backdrop),
            scene: None,
        }
    }

    /// What a pane looks like over the gradient, from one end to the other, for checking that
    /// text reads on all of it.
    fn scene_panes(sc: Scene) -> Vec<u32> {
        let keep = 1. - 0.8 * sc.dim;
        (0..=20).map(|i| mix(0x000000, sc.backdrop.along(i as f32 / 20.), keep)).collect()
    }

    /// The page's own fill, for a screen that covers it: none on a colour theme, whose page is
    /// the window's gradient.
    pub fn page(&self) -> Hsla {
        if self.backdrop.is_some() { gpui::transparent_black() } else { self.base }
    }
}

fn to_hsl(c: u32) -> [f32; 3] {
    let ch = |s: u32| ((c >> s) & 0xff) as f32 / 255.;
    let (r, g, b) = (ch(16), ch(8), ch(0));
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.;
    let d = max - min;
    if d == 0. {
        return [f32::NAN, 0., l];
    }
    let s = if l > 0.5 { d / (2. - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6. } else { 0. }
    } else if max == g {
        (b - r) / d + 2.
    } else {
        (r - g) / d + 4.
    };
    [h * 60., s, l]
}

fn from_hsl([h, s, l]: [f32; 3]) -> u32 {
    let k = |n: f32| (n + h / 30.).rem_euclid(12.);
    let a = s * l.min(1. - l);
    let f = |n: f32| l - a * (k(n) - 3.).min(9. - k(n)).clamp(-1., 1.);
    let ch = |v: f32| ((v * 255.).round().clamp(0., 255.)) as u32;
    ch(f(0.)) << 16 | ch(f(8.)) << 8 | ch(f(4.))
}

fn to_oklab(c: u32) -> [f32; 3] {
    let lin = |s: u32| {
        let v = ((c >> s) & 0xff) as f32 / 255.;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(16), lin(8), lin(0));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn from_oklab([l, a, b]: [f32; 3]) -> u32 {
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let r = 4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_;
    let g = -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_;
    let bl = -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_;
    let ch = |v: f32| {
        let v = v.clamp(0., 1.);
        let v = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1. / 2.4) - 0.055 };
        (v * 255.).round() as u32
    };
    ch(r) << 16 | ch(g) << 8 | ch(bl)
}

/// `a` with `t` of `b` mixed in, in Oklab, as CSS `color-mix(in oklab, …)` does.
fn oklab_mix(a: u32, b: u32, t: f32) -> u32 {
    let (a, b) = (to_oklab(a), to_oklab(b));
    from_oklab([0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t))
}

/// The swatches drawn for a palette in the picker: background, surface, accent.
pub fn swatches(name: &str, window: WindowAppearance, custom: Option<&CustomColors>) -> [Hsla; 3] {
    let t = Theme::new(name, window, custom);
    [t.base, t.pane, t.accent]
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CustomColors {
    pub bg: u32,
    pub surface: u32,
    pub text: u32,
    pub accent: u32,
}

impl CustomColors {
    pub fn parse(bg: &str, surface: &str, text: &str, accent: &str) -> Option<CustomColors> {
        Some(CustomColors { bg: hex(bg)?, surface: hex(surface)?, text: hex(text)?, accent: hex(accent)? })
    }

    pub fn from_settings(c: &crate::core::settings::CustomTheme) -> Option<CustomColors> {
        CustomColors::parse(&c.bg, &c.surface, &c.text, &c.accent)
    }

    pub fn midnight() -> CustomColors {
        CustomColors { bg: 0x0f1116, surface: 0x171a21, text: 0xe6e8ee, accent: 0x6682ff }
    }
}

pub fn hex(s: &str) -> Option<u32> {
    let s = s.trim().trim_start_matches('#');
    let s = if s.len() == 3 { s.chars().flat_map(|c| [c, c]).collect::<String>() } else { s.to_string() };
    (s.len() == 6).then(|| u32::from_str_radix(&s, 16).ok()).flatten()
}

pub fn to_hex(c: u32) -> String {
    format!("#{c:06x}")
}

/// A colour as hue (0..1, round the wheel), saturation and value (0..=1).
pub fn to_hsv(c: u32) -> [f32; 3] {
    let ch = |s: u32| ((c >> s) & 0xff) as f32 / 255.;
    let (r, g, b) = (ch(16), ch(8), ch(0));
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let h = if d == 0. {
        0.
    } else if max == r {
        ((g - b) / d).rem_euclid(6.)
    } else if max == g {
        (b - r) / d + 2.
    } else {
        (r - g) / d + 4.
    };
    [h / 6., if max == 0. { 0. } else { d / max }, max]
}

pub fn from_hsv([h, s, v]: [f32; 3]) -> u32 {
    let h = h.rem_euclid(1.) * 6.;
    let (s, v) = (s.clamp(0., 1.), v.clamp(0., 1.));
    let c = v * s;
    let x = c * (1. - (h % 2. - 1.).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.),
        1 => (x, c, 0.),
        2 => (0., c, x),
        3 => (0., x, c),
        4 => (x, 0., c),
        _ => (c, 0., x),
    };
    let ch = |u: f32| ((u + v - c) * 255.).round().clamp(0., 255.) as u32;
    ch(r) << 16 | ch(g) << 8 | ch(b)
}

/// `a` with `t` of `b` mixed in, channel by channel.
pub fn mix_rgb(a: u32, b: u32, t: f32) -> u32 {
    mix(a, b, t)
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |v: u32, s: u32| ((v >> s) & 0xff) as f32;
    let m = |s: u32| ((ch(a, s) + (ch(b, s) - ch(a, s)) * t).round().clamp(0., 255.) as u32) << s;
    m(16) | m(8) | m(0)
}

fn luminance(c: u32) -> f32 {
    let ch = |s: u32| ((c >> s) & 0xff) as f32 / 255.;
    0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0)
}

/// WCAG relative luminance of an sRGB colour.
fn relative_luminance(c: u32) -> f32 {
    let ch = |s: u32| {
        let v = ((c >> s) & 0xff) as f32 / 255.;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0)
}

/// WCAG contrast ratio between two colours, 1 to 21.
fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `fg`, moved toward `toward` in small steps until it reads at `min`:1 on every colour in `on`.
/// Colours that already do are returned as they are, so a palette keeps its own shade.
fn readable(fg: u32, toward: u32, on: &[u32], min: f32) -> u32 {
    let mut c = fg;
    for step in 1..=25 {
        if on.iter().all(|&bg| contrast(c, bg) >= min) {
            return c;
        }
        c = mix(fg, toward, step as f32 / 25.);
    }
    toward
}

/// The four custom colours turned into a whole palette, the same way the old client derived it.
fn derive(c: CustomColors) -> Palette {
    let light = luminance(c.bg) > 0.5;
    let status = palette(if light { "daylight" } else { "midnight" }).unwrap();
    Palette {
        bg: c.bg,
        surface: c.surface,
        surface2: mix(c.surface, c.text, 0.06),
        border: mix(c.surface, c.text, 0.13),
        border_strong: mix(c.surface, c.text, 0.24),
        text: c.text,
        muted: mix(c.text, c.bg, 0.4),
        accent: c.accent,
        danger: status.danger,
        live: status.live,
        warn: status.warn,
        video: mix(c.bg, 0x000000, if light { 0.88 } else { 0.5 }),
        light,
    }
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub dark: bool,
    /// The page behind the panes.
    pub base: Hsla,
    /// A pane: channels, chat, members.
    pub pane: Hsla,
    /// Something raised inside a pane: a card, a hovered row.
    pub layer: Hsla,
    pub layer_hover: Hsla,
    /// A control: button, field.
    pub control: Hsla,
    pub control_hover: Hsla,
    /// A sunken track, behind segmented choices and sliders.
    pub well: Hsla,
    pub stroke: Hsla,
    pub stroke_strong: Hsla,
    pub text: Hsla,
    pub text2: Hsla,
    pub text3: Hsla,
    pub accent: Hsla,
    pub accent_soft: Hsla,
    /// The accent as text or a thin line on a pane: nudged toward the text colour until it reads
    /// (4.5:1), for palettes whose accent is a fill first (the blurple on Dark).
    pub accent_text: Hsla,
    /// Text on an accent fill.
    pub on_accent: Hsla,
    pub critical: Hsla,
    /// Muted or deafened: the critical colour softened toward the text, a state to notice
    /// rather than an alarm.
    pub quiet: Hsla,
    /// Speaking, connected, good.
    pub success: Hsla,
    pub caution: Hsla,
    /// Behind video.
    pub stage: Hsla,
    pub scrim: Hsla,
    /// A floating surface: menus, popovers, tooltips.
    pub popover: Hsla,
    /// A surface that must hide what is behind it, a dialog over the dimmed window: the pane on
    /// a palette, and a solid shade of it on a colour theme, whose panes are see-through.
    pub card: Hsla,
    /// The colour theme's gradient behind the window, or none on a plain palette.
    pub backdrop: Option<Backdrop>,
    /// The colour theme's name, for what it draws over its gradient (`ui::scene_fx`).
    pub scene: Option<&'static str>,
}

fn c(v: u32) -> Hsla {
    rgb(v).into()
}

impl Theme {
    pub fn new(name: &str, window: WindowAppearance, custom: Option<&CustomColors>) -> Theme {
        let p = match name {
            "system" => system(window),
            "custom" => derive(custom.copied().unwrap_or_else(CustomColors::midnight)),
            other => match scene(other) {
                Some(sc) => return Theme { scene: scenes().into_iter().map(|(k, _)| k).find(|k| *k == other), ..Theme::from_scene(sc) },
                None => palette(other).unwrap_or_else(|| palette("midnight").unwrap()),
            },
        };
        Theme::from_palette(p)
    }

    fn from_palette(p: Palette) -> Theme {
        let dark = !p.light;
        let ink = if dark { gpui::white() } else { gpui::black() };
        let accent = c(p.accent);
        Theme {
            dark,
            base: c(p.bg),
            pane: c(p.surface),
            layer: ink.opacity(0.035),
            layer_hover: ink.opacity(0.06),
            control: c(p.surface2),
            control_hover: c(mix(p.surface2, p.text, 0.06)),
            well: if dark { gpui::black().opacity(0.25) } else { ink.opacity(0.05) },
            stroke: c(p.border),
            stroke_strong: c(p.border_strong),
            text: c(p.text),
            // Secondary text: the palette's own muted, nudged only if a custom palette left it
            // under 4.5:1 on its panes and controls.
            text2: c(readable(p.muted, p.text, &[p.surface, p.bg, p.surface2], 4.5)),
            // Tertiary text (timestamps, placeholders, small labels): fainter than the secondary,
            // but still 4.5:1 on a pane and on the page, which the plain 30% mix did not reach.
            text3: c(readable(mix(p.muted, p.surface, 0.3), p.muted, &[p.surface, p.bg], 4.5)),
            accent,
            accent_soft: accent.opacity(if dark { 0.16 } else { 0.12 }),
            accent_text: c(readable(p.accent, p.text, &[p.surface, p.bg], 4.5)),
            on_accent: if luminance(p.accent) > 0.55 { c(0x0b0c10) } else { gpui::white() },
            critical: c(p.danger),
            quiet: c(if dark { mix(p.danger, p.text, 0.28) } else { mix(p.danger, p.muted, 0.3) }),
            success: c(p.live),
            caution: c(p.warn),
            stage: c(p.video),
            scrim: gpui::black().opacity(if dark { 0.55 } else { 0.35 }),
            popover: c(if dark { mix(p.surface2, p.bg, 0.25) } else { 0xffffff }),
            card: c(p.surface),
            backdrop: None,
            scene: None,
        }
    }

    /// A status colour as a soft background behind its own text.
    pub fn tint(&self, color: Hsla) -> Hsla {
        color.opacity(if self.dark { 0.14 } else { 0.12 })
    }

    /// A status colour as it reads over video or a dark glass, whatever the palette: light and
    /// a little calmer, so it shows without shouting.
    pub fn over_video(&self, color: Hsla) -> Hsla {
        gpui::hsla(color.h, color.s.min(0.8), 0.74, 1.)
    }
}

/// Follows Windows: its light or dark mode, and its accent colour, or Ciranda's own blue when
/// that is a grey.
fn system(window: WindowAppearance) -> Palette {
    let dark = matches!(window, WindowAppearance::Dark | WindowAppearance::VibrantDark);
    let accent = windows_accent(dark).map(|a| {
        let to = |v: f32| (v.clamp(0., 1.) * 255.).round() as u32;
        to(a.r) << 16 | to(a.g) << 8 | to(a.b)
    });
    if dark {
        let mut p = pal(
            [0x111214, 0x1a1b1f, 0x26282d, 0x2c2e34, 0x3c3f46, 0xf3f4f6, 0x9da3ad, 0x6682ff, 0xff6b78, 0x6ccb5f, 0xfcc419, 0x08090b],
            false,
        );
        p.accent = accent.unwrap_or(p.accent);
        p
    } else {
        let mut p = pal(
            [0xf2f3f5, 0xffffff, 0xf1f2f4, 0xdfe1e5, 0xc4c7cd, 0x1b1c1f, 0x5f6570, 0x0b2ecb, 0xc42b1c, 0x0f7b0f, 0x9d5d00, 0x16181d],
            true,
        );
        p.accent = accent.unwrap_or(p.accent);
        p
    }
}

/// The Windows accent colour in the shade Windows itself uses on this background, or `None`
/// when it is a grey that would not read as an accent.
#[cfg(not(windows))]
fn windows_accent(_: bool) -> Option<Rgba> {
    None
}

#[cfg(windows)]
fn windows_accent(dark: bool) -> Option<Rgba> {
    // The window draws the theme on every frame the root redraws; the registry is asked at most
    // twice a second, which still follows a change of accent in Settings almost at once.
    thread_local! {
        static LAST: std::cell::RefCell<Option<(std::time::Instant, Option<Vec<u8>>)>> = const { std::cell::RefCell::new(None) };
    }
    let palette = LAST.with(|last| {
        let mut last = last.borrow_mut();
        match &*last {
            Some((at, value)) if at.elapsed() < std::time::Duration::from_millis(500) => value.clone(),
            _ => {
                let value = registry_binary(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent", "AccentPalette");
                *last = Some((std::time::Instant::now(), value.clone()));
                value
            }
        }
    })?;
    // Eight RGBA entries, lightest to darkest; the accent itself is the fourth.
    let i = if dark { 1 } else { 4 };
    let px = palette.get(i * 4..i * 4 + 3)?;
    let (r, g, b) = (px[0], px[1], px[2]);
    let chroma = r.max(g).max(b) - r.min(g).min(b);
    (chroma >= 48).then(|| rgb(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)))
}

#[cfg(windows)]
fn registry_binary(key: &str, value: &str) -> Option<Vec<u8>> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW};
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let mut buf = vec![0u8; 64];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(key).as_ptr(),
            wide(value).as_ptr(),
            RRF_RT_REG_BINARY,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    (ok == 0).then(|| {
        buf.truncate(len as usize);
        buf
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_palettes_derive_like_the_old_client() {
        let p = derive(CustomColors { bg: 0x000000, surface: 0x101010, text: 0xffffff, accent: 0xff0000 });
        assert!(!p.light);
        assert_eq!(p.muted, mix(0xffffff, 0x000000, 0.4));
        assert_eq!(p.border, mix(0x101010, 0xffffff, 0.13));
        let light = derive(CustomColors { bg: 0xffffff, surface: 0xf0f0f0, text: 0x000000, accent: 0x0000ff });
        assert!(light.light);
        assert_eq!(light.danger, 0xc4303a);
    }

    fn rgb_of(c: Hsla) -> u32 {
        let c = Rgba::from(c);
        let to = |v: f32| (v * 255.).round() as u32;
        to(c.r) << 16 | to(c.g) << 8 | to(c.b)
    }

    #[test]
    fn every_palette_keeps_its_text_readable() {
        for (name, _) in palettes() {
            let t = Theme::new(name, WindowAppearance::Dark, None);
            let (pane, base) = (rgb_of(t.pane), rgb_of(t.base));
            for (what, c) in [("text", t.text), ("text2", t.text2), ("text3", t.text3), ("accent_text", t.accent_text)] {
                for (on, bg) in [("pane", pane), ("page", base)] {
                    let ratio = contrast(rgb_of(c), bg);
                    assert!(ratio >= 4.4, "{name}: {what} on the {on} is {ratio:.2}:1");
                }
            }
        }
    }

    /// Within a step of rounding on each channel.
    fn near(a: u32, b: u32) -> bool {
        [16, 8, 0].iter().all(|s| (((a >> s) & 0xff) as i32 - ((b >> s) & 0xff) as i32).abs() <= 2)
    }

    #[test]
    fn vice_city_is_discords_to_the_colour() {
        // What Discord's own stylesheet computes for its Vice City theme, read from the running
        // client (`.dev/vice-city/tokens.html`).
        let t = Theme::new("vicecity", WindowAppearance::Dark, None);
        for (what, got, discord) in [
            ("text", rgb_of(t.text), 0xf6f4f0),
            ("text2", rgb_of(t.text2), 0xc2c0c0),
            ("text3", rgb_of(t.text3), 0xb4b2b4),
            ("card", rgb_of(t.card), 0x1d2333),
            ("popover", rgb_of(t.popover), 0x212737),
            ("stroke", rgb_of(t.stroke), 0x585f70),
        ] {
            assert!(near(got, discord), "{what}: {got:06x}, Discord has {discord:06x}");
        }
        assert_eq!(rgb_of(t.accent), 0x5865f2);
        // A pixel of Discord's chat at (1200, 800) of a 2560 by 1380 window: the gradient there
        // under the chat's 64% black.
        let b = t.backdrop.unwrap();
        let chat = mix(0x000000, b.at(1200.5, 800.5, 2560., 1380.), 1. - 0.64);
        assert!(near(chat, 0x4e1347), "{chat:06x}");
        let (page, pane) = (t.base.a, t.pane.a);
        assert!((1. - (1. - page) * (1. - pane) - 0.64).abs() < 1e-4);
    }

    #[test]
    fn colour_themes_keep_their_text_readable() {
        for (name, _) in scenes() {
            let sc = scene(name).unwrap();
            let t = Theme::from_scene(sc);
            for bg in Theme::scene_panes(sc) {
                for (what, c) in [("text", t.text), ("text2", t.text2), ("text3", t.text3), ("accent_text", t.accent_text)] {
                    let ratio = contrast(rgb_of(c), bg);
                    assert!(ratio >= 4.4, "{name}: {what} on a pane over {bg:06x} is {ratio:.2}:1");
                }
            }
            for (what, c) in [("text", t.text), ("text2", t.text2), ("text3", t.text3)] {
                for on in [t.card, t.popover] {
                    let ratio = contrast(rgb_of(c), rgb_of(on));
                    assert!(ratio >= 4.5, "{name}: {what} on a card is {ratio:.2}:1");
                }
            }
        }
    }

    #[test]
    fn a_backdrop_runs_corner_to_corner() {
        let b = scene("vicecity").unwrap().backdrop;
        assert_eq!(b.at(0., 0., 400., 200.), 0x4645ff);
        assert_eq!(b.at(400., 200., 400., 200.), 0xf89a03);
        assert_eq!(b.along(0.5), mix(0xaf01f3, 0xff6698, 0.5));
    }

    #[test]
    fn hsl_round_trips() {
        for c in [0xff0000, 0x4645ff, 0xf89a03, 0x36373e, 0x808080] {
            assert!(near(from_hsl(to_hsl(c).map(|v| if v.is_nan() { 0. } else { v })), c), "{c:06x}");
        }
    }

    #[test]
    fn a_readable_colour_is_left_alone() {
        assert_eq!(readable(0xffffff, 0xffffff, &[0x000000], 4.5), 0xffffff);
        let nudged = readable(0x777777, 0x000000, &[0xffffff], 4.5);
        assert!(contrast(nudged, 0xffffff) >= 4.5);
    }

    #[test]
    fn hex_reads_short_and_long_forms() {
        assert_eq!(hex("#fff"), Some(0xffffff));
        assert_eq!(hex("5b8cff"), Some(0x5b8cff));
        assert_eq!(hex("nope"), None);
    }

    #[test]
    fn hsv_round_trips() {
        for c in [0x000000, 0xffffff, 0xff0000, 0x00ff00, 0x0000ff, 0x5b8cff, 0x0f1116, 0xe6e8ee, 0xff7a4d, 0x808080] {
            assert_eq!(from_hsv(to_hsv(c)), c, "{c:06x}");
        }
        assert_eq!(from_hsv([1. / 3., 1., 1.]), 0x00ff00);
        assert_eq!(from_hsv([1., 1., 1.]), 0xff0000);
    }
}
