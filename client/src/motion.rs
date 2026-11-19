//! How things move. Short and soft: what opens rises a few pixels as it fades in, what closes
//! fades the other way, and what toggles (mute, a pane) rides a spring, so a change of mind
//! halfway turns it round without a jump. GPUI holds still when the system asks for less motion.

use crate::theme::px;
use gpui::{Animation, AnimationElement, AnimationExt, ElementId, Hsla, IntoElement, Rgba, SpringConfig, Styled};
use std::time::Duration;

/// Toggles and badges: quick, with a hint of bounce at the end.
pub const SNAP: SpringConfig = SpringConfig::new(620., 36., 1.);
/// Panes and markers that slide: quick and with no bounce.
pub const GLIDE: SpringConfig = SpringConfig::new(420., 41., 1.);

/// Content taking another's place: a channel, a conversation, a server.
pub const ENTER: Duration = Duration::from_millis(180);
/// A dialog or a menu on its way out.
pub const LEAVE: Duration = Duration::from_millis(130);

/// Rises `RISE` pixels into place as it fades in, keyed by `id`: an element with a new id plays
/// it again, so giving the content's identity as the id plays it on every change of content.
/// It moves by its offset, not its margin, so nothing around it is laid out again.
pub fn enter<E: IntoElement + Styled + 'static>(el: E, id: impl Into<ElementId>) -> AnimationElement<E> {
    const RISE: f32 = 6.;
    el.with_animation(id, Animation::new(ENTER).with_easing(gpui::ease_out_quint()), |d, p| {
        d.opacity(p).relative().top(px(RISE * (1. - p)))
    })
}

/// `a` turning into `b`, `t` from 0 to 1.
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b, t) = (Rgba::from(a), Rgba::from(b), t.clamp(0., 1.));
    let m = |x: f32, y: f32| x + (y - x) * t;
    Rgba { r: m(a.r, b.r), g: m(a.g, b.g), b: m(a.b, b.b), a: m(a.a, b.a) }.into()
}
