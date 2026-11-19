//! Animated names: an effect someone picks for their name (Settings → Profile), kept on the
//! server and drawn by every client, letter by letter, as a band of colour moving through it.
//!
//! Where a list is small and redraws on its own (the members, who is in voice, a profile card) the
//! colours move; in the chat they hold still, as moving there would redraw the whole chat every
//! frame for a name. A system that asks for less motion gets them still everywhere (GPUI holds
//! repeating animations at their start).

use crate::theme::Theme;
use gpui::{Animation, AnimationExt, AnyElement, ElementId, HighlightStyle, Hsla, IntoElement, ParentElement, Styled, StyledText, div};
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

/// One trip of the colours through a name.
const CYCLE: Duration = Duration::from_millis(2800);
/// Enough for colour drifting through a name, and far less than a frame every refresh.
const FPS: f32 = 30.;

/// A known effect, or None for a plain name (none, or one this client has never heard of).
fn known(fx: Option<&str>) -> Option<&'static str> {
    let fx = fx?;
    all().into_iter().map(|(k, _)| k).find(|k| *k == fx)
}

/// `name` wearing `fx`, in `plain` when it has none. `live` lets the colours move; `id` keeps
/// the motion's place between draws and must be unique where it is drawn.
pub fn name(id: impl Into<ElementId>, name: &str, fx: Option<&str>, live: bool, plain: Hsla, t: &Theme) -> AnyElement {
    let text: gpui::SharedString = name.to_string().into();
    let Some(fx) = known(fx) else { return div().text_color(plain).child(text).into_any_element() };
    let dark = t.dark;
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
        assert_eq!(known(Some("glitter-from-the-future")), None);
        assert_eq!(known(None), None);
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
