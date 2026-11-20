//! Animated names: an effect someone picks for their name (Settings → Profile), kept on the
//! server and drawn by every client, letter by letter, as a band of colour moving through it.
//!
//! Where a list is small and redraws on its own (the members, who is in voice, a profile card) the
//! colours move; in the chat they hold still, as moving there would redraw the whole chat every
//! frame for a name. A system that asks for less motion gets them still everywhere (GPUI holds
//! repeating animations at their start).
//!
//! Name styles go with the colour themes (`crate::theme::scenes`), as Discord's display name
//! styles do: a typeface of their own, and colour on some. Servers that know them say
//! `name-styles`.

use crate::theme::{Theme, px};
use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt, AnyElement, ElementId, FontWeight, HighlightStyle, Hsla, IntoElement, ParentElement, Styled, StyledText, div,
};
use std::sync::OnceLock;
use std::time::Duration;

/// The effects, in the picker's order: the server's name for each, and what people see.
pub fn all() -> [(&'static str, &'static str); 6] {
    [
        ("rainbow", tr!("Rainbow", "Arco-íris")),
        ("aurora", tr!("Aurora", "Aurora")),
        ("fire", tr!("Fire", "Fogo")),
        ("ocean", tr!("Ocean", "Oceano")),
        ("gold", tr!("Gold", "Ouro")),
        ("candy", tr!("Candy", "Doce")),
    ]
}

/// The name styles, in the picker's order, after the effects.
pub fn styles() -> [(&'static str, &'static str); 7] {
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

/// How a style colours a name.
#[derive(Clone, Copy)]
enum Paint {
    /// As a plain name would be.
    Plain,
    /// One colour: a light shade for dark palettes, a deep one for light ones.
    Solid(u32, u32),
    /// One of the effects' moving bands.
    Band(&'static str),
    /// The plain colour over a shadow a quarter as bright, down and to the right, as the world
    /// of blocks writes everything.
    Shadow,
    /// One colour that now and then tears into cyan and red, shaking, as a glitch does.
    Glitch(u32, u32),
}

struct Style {
    font: &'static str,
    weight: FontWeight,
    paint: Paint,
    /// In capitals, as the game's interface writes names.
    upper: bool,
    /// Letters spread apart, as a title is set.
    spaced: bool,
}

fn style(key: &str) -> Option<Style> {
    let s = |font, weight, paint, upper| Style { font, weight, paint, upper, spaced: false };
    Some(match key {
        "vicecity" => s(vice_city_font(), FontWeight::NORMAL, Paint::Plain, false),
        "middleearth" => s("Cinzel Decorative", FontWeight::BOLD, Paint::Band("gold"), false),
        "bullworth" => s("Permanent Marker", FontWeight::NORMAL, Paint::Plain, false),
        "division" => s("Chakra Petch", FontWeight::SEMIBOLD, Paint::Solid(0xff7a1f, 0xb34a00), true),
        // Minecraft-Font: Idrees Hassan's free rendition of the game's own lettering.
        "blocks" => s("Minecraft", FontWeight::MEDIUM, Paint::Shadow, false),
        // Rajdhani is Cyberpunk 2077's interface typeface.
        "nightcity" => s("Rajdhani", FontWeight::BOLD, Paint::Glitch(0xfcee0a, 0x6f6500), true),
        // The title's lettering is geometric and spread wide, in the campfire's orange; Jura is
        // the nearest free face, and the fan-made one is used where it is installed.
        "outerwilds" => Style { spaced: true, ..s(outer_wilds_font(), FontWeight::SEMIBOLD, Paint::Solid(0xef7b39, 0xa8480f), true) },
        _ => return None,
    })
}

/// How often a glitching name tears, and for how much of that.
const GLITCH_EVERY: Duration = Duration::from_millis(3200);
const GLITCH_FOR: f32 = 0.09;

/// `text` drawn as `style` paints it.
fn styled(id: ElementId, text: gpui::SharedString, st: Style, live: bool, plain: Hsla, dark: bool) -> AnyElement {
    let d = div().font_family(st.font).font_weight(st.weight);
    let pick = |on_dark: u32, on_light: u32| -> Hsla { gpui::rgb(if dark { on_dark } else { on_light }).into() };
    match st.paint {
        Paint::Plain => d.text_color(plain).child(text).into_any_element(),
        Paint::Solid(on_dark, on_light) => d.text_color(pick(on_dark, on_light)).child(text).into_any_element(),
        Paint::Band(band) if live => d
            .with_animation(id, Animation::new(CYCLE).repeat().with_max_fps(FPS), move |d, p| d.child(painted(text.clone(), band, p, dark)))
            .into_any_element(),
        Paint::Band(band) => d.child(painted(text, band, 0.15, dark)).into_any_element(),
        Paint::Shadow => {
            let shadow = Hsla { l: plain.l * 0.25, ..plain };
            d.relative()
                .child(div().absolute().top(px(1.5)).left(px(1.5)).text_color(shadow).child(text.clone()))
                .child(div().relative().text_color(plain).child(text))
                .into_any_element()
        }
        Paint::Glitch(on_dark, on_light) => {
            let colour = pick(on_dark, on_light);
            if !live {
                return d.text_color(colour).child(text).into_any_element();
            }
            let (cyan, red) = (pick(0x02d7f2, 0x007f91), pick(0xff003c, 0xb3002a));
            d.relative()
                .with_animation(id, Animation::new(GLITCH_EVERY).repeat().with_max_fps(FPS), move |d, p| {
                    let on = p > 1. - GLITCH_FOR;
                    // A new jolt every frame of the tear, from the frame's number alone.
                    let frame = (p * 1000.) as u32 / 10;
                    let jolt = |salt: u32| (if on { (frame.wrapping_mul(2_654_435_761) >> (salt * 3)) % 5 } else { 2 }) as f32 - 2.;
                    d.when(on, |d| {
                        d.child(
                            div()
                                .absolute()
                                .top(px(jolt(1) * 0.5))
                                .left(px(-2. + jolt(2) * 0.6))
                                .text_color(cyan.opacity(0.9))
                                .child(text.clone()),
                        )
                        .child(
                            div()
                                .absolute()
                                .top(px(jolt(3) * 0.5))
                                .left(px(2. + jolt(4) * 0.6))
                                .text_color(red.opacity(0.9))
                                .child(text.clone()),
                        )
                    })
                    .child(div().relative().left(px(jolt(5) * 0.5)).text_color(colour).child(text.clone()))
                })
                .into_any_element()
        }
    }
}

static PRICEDOWN: OnceLock<Option<String>> = OnceLock::new();
static OUTER_WILDS: OnceLock<Option<String>> = OnceLock::new();

/// Outer Wilds' typeface: Falc's fan-made one where installed (it may not be shipped), Jura
/// otherwise.
fn outer_wilds_font() -> &'static str {
    OUTER_WILDS.get().and_then(|f| f.as_deref()).unwrap_or("Jura")
}

/// Vice City's typeface: Discord's is Pricedown, which may not be shipped inside an app without
/// a licence of its own, so it is used when this computer has it installed, and Luckiest Guy,
/// the nearest free one, otherwise.
fn vice_city_font() -> &'static str {
    PRICEDOWN.get().and_then(|f| f.as_deref()).unwrap_or("Luckiest Guy")
}

/// Whether this computer has Pricedown, for Vice City's names.
pub fn has_pricedown() -> bool {
    PRICEDOWN.get().is_some_and(Option::is_some)
}

/// Looks for Pricedown among the fonts installed, once, at start.
pub fn find_fonts(installed: &[String]) {
    let found = installed.iter().find(|f| f.to_lowercase().starts_with("pricedown")).cloned();
    if let Some(f) = &found {
        log::info!("name styles: Vice City in {f}, installed here");
    }
    let _ = PRICEDOWN.set(found);
    let found = installed.iter().find(|f| f.to_lowercase().replace(' ', "").starts_with("outerwilds")).cloned();
    if let Some(f) = &found {
        log::info!("name styles: Outer Wilds in {f}, installed here");
    }
    let _ = OUTER_WILDS.set(found);
}

/// One trip of the colours through a name.
const CYCLE: Duration = Duration::from_millis(2800);
/// Enough for colour drifting through a name, and far less than a frame every refresh.
const FPS: f32 = 30.;

/// A known effect, or None for a plain name (none, or one this client has never heard of).
fn known(fx: Option<&str>) -> Option<&'static str> {
    let fx = fx?;
    all().into_iter().chain(styles()).map(|(k, _)| k).find(|k| *k == fx)
}

/// `name` wearing `fx`, in `plain` when it has none. `live` lets the colours move; `id` keeps
/// the motion's place between draws and must be unique where it is drawn.
pub fn name(id: impl Into<ElementId>, name: &str, fx: Option<&str>, live: bool, plain: Hsla, t: &Theme) -> AnyElement {
    let text: gpui::SharedString = name.to_string().into();
    let Some(fx) = known(fx) else { return div().text_color(plain).child(text).into_any_element() };
    let dark = t.dark;
    if let Some(st) = style(fx) {
        let text = if st.upper { name.to_uppercase() } else { name.to_string() };
        // A hair space between letters spreads them as a title is spaced.
        let text = if st.spaced { text.chars().map(String::from).collect::<Vec<_>>().join("\u{200a}") } else { text };
        let text = text.into();
        return styled(id.into(), text, st, live, plain, dark);
    }
    if !live {
        return div().child(painted(text, fx, 0.15, dark)).into_any_element();
    }
    div()
        .with_animation(id, Animation::new(CYCLE).repeat().with_max_fps(FPS), move |d, p| d.child(painted(text.clone(), fx, p, dark)))
        .into_any_element()
}

/// The name with each letter in its colour at `phase` (0..1 through the cycle).
fn painted(text: gpui::SharedString, fx: &'static str, phase: f32, dark: bool) -> StyledText {
    let letters: Vec<(usize, char)> = text.char_indices().collect();
    // Short names still show a stretch of the band, long ones a little more of it.
    let span = (letters.len() as f32 / 9.).clamp(0.45, 1.);
    let highlights: Vec<_> = letters
        .iter()
        .enumerate()
        .map(|(i, (at, c))| {
            let u = i as f32 / letters.len().max(1) as f32 * span;
            let style = HighlightStyle {
                color: Some(colour(fx, u - phase, phase, i as f32 / letters.len().max(1) as f32, dark)),
                ..Default::default()
            };
            (*at..*at + c.len_utf8(), style)
        })
        .collect();
    StyledText::new(text).with_highlights(highlights)
}

/// The colour at `u` along the effect's band (wrapping). `x` is the letter's place in the name
/// (0..1), for the gold's shine that sweeps across once per cycle.
fn colour(fx: &str, u: f32, phase: f32, x: f32, dark: bool) -> Hsla {
    // Lighter on dark palettes, deeper on light ones, so the name reads on either.
    let (s, l) = if dark { (0.85, 0.7) } else { (0.8, 0.42) };
    let u = u.rem_euclid(1.);
    let hue = |deg: f32| deg.rem_euclid(360.) / 360.;
    match fx {
        "rainbow" => gpui::hsla(u, s, l, 1.),
        "gold" => {
            // A warm gold with a bright shine passing over it.
            let shine = (1. - ((x - (phase * 1.6 - 0.3)).abs() / 0.18)).clamp(0., 1.);
            let base = gpui::hsla(hue(42.), 0.9, if dark { 0.62 } else { 0.4 }, 1.);
            let bright = gpui::hsla(hue(50.), 1., if dark { 0.88 } else { 0.58 }, 1.);
            crate::motion::mix(base, bright, shine * shine)
        }
        _ => {
            let stops: &[f32] = match fx {
                "aurora" => &[150., 178., 205., 268.],
                "fire" => &[355., 15., 32., 48.],
                "ocean" => &[188., 202., 218., 236.],
                _ => &[325., 290., 258., 200.], // candy
            };
            // There and back along the stops, so the band wraps without a seam.
            let n = stops.len();
            let pos = (1. - (2. * u - 1.).abs()) * (n - 1) as f32;
            let i = (pos.floor() as usize).min(n - 2);
            let f = pos - i as f32;
            let (a, b) = (stops[i], stops[i + 1]);
            // The short way round the wheel (fire crosses red at 0).
            let d = ((b - a + 540.) % 360.) - 180.;
            gpui::hsla(hue(a + d * f), s, l, 1.)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_effect_is_a_plain_name() {
        assert_eq!(known(Some("rainbow")), Some("rainbow"));
        assert_eq!(known(Some("vicecity")), Some("vicecity"));
        assert_eq!(known(Some("glitter-from-the-future")), None);
        assert_eq!(known(None), None);
    }

    #[test]
    fn every_style_has_a_typeface_and_matches_the_server() {
        // The server's NAME_FX takes the same names (server/src/accounts.js).
        let server = include_str!("../../server/src/accounts.js");
        for (k, _) in all().into_iter().chain(styles()) {
            assert!(server.contains(&format!("'{k}'")), "the server does not take {k}");
        }
        for (k, _) in styles() {
            assert!(style(k).is_some(), "{k} has no style");
        }
    }

    #[test]
    fn every_colour_reads_on_its_palette() {
        for (fx, _) in all() {
            for i in 0..40 {
                let u = i as f32 / 40.;
                let dark = colour(fx, u, u, u, true);
                let light = colour(fx, u, u, u, false);
                assert!(dark.l >= 0.55, "{fx} on dark at {u}: {}", dark.l);
                assert!(light.l <= 0.6, "{fx} on light at {u}: {}", light.l);
            }
        }
    }
}
