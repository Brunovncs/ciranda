//! A colour theme's gradient (`theme::Backdrop`) as a picture: GPUI's own gradients take two
//! colours, and these have four or five. It is drawn small and stretched to fit, as a gradient has
//! no detail to lose; a few are kept (the window's, the swatches') and the rest let go.

use crate::theme::Backdrop;
use gpui::{App, Img, ObjectFit, RenderImage, Styled, StyledImage, img};
use smallvec::SmallVec;
use std::cell::RefCell;
use std::sync::Arc;

/// The widest a picture is drawn: plenty for a gradient stretched over a whole screen.
const MAX_WIDTH: f32 = 320.;
const KEEP: usize = 12;

type Key = (usize, u32, u32, u32);

thread_local! {
    static KEPT: RefCell<Vec<(Key, Arc<RenderImage>)>> = const { RefCell::new(Vec::new()) };
}

/// `b` filling an area `w` by `h` (logical pixels) of the element it is put in.
pub fn fill(b: Backdrop, w: f32, h: f32, cx: &mut App) -> Img {
    img(picture(b, w, h, cx)).size_full().object_fit(ObjectFit::Fill)
}

fn picture(b: Backdrop, w: f32, h: f32, cx: &mut App) -> Arc<RenderImage> {
    // A window being resized asks for a new size every frame; to the nearest 16 pixels its
    // gradient looks the same.
    let q = if w > 400. { 16. } else { 1. };
    let (w, h) = (((w / q).round() * q).max(1.), ((h / q).round() * q).max(1.));
    let key = (b.stops.as_ptr() as usize, b.angle.to_bits(), w as u32, h as u32);
    if let Some(hit) = KEPT.with(|k| k.borrow().iter().find(|(k, _)| *k == key).map(|(_, i)| i.clone())) {
        return hit;
    }
    let pw = w.min(MAX_WIDTH).round().max(1.);
    let ph = (h * pw / w).round().max(1.);
    let image = draw(b, pw as u32, ph as u32);
    let gone = KEPT.with(|k| {
        let mut k = k.borrow_mut();
        k.push((key, image.clone()));
        let over = k.len().saturating_sub(KEEP);
        k.drain(..over).map(|(_, i)| i).collect::<Vec<_>>()
    });
    for old in gone {
        cx.drop_image(old, None);
    }
    image
}

/// The gradient `w` by `h` pixels, in the BGRA GPUI takes.
fn draw(b: Backdrop, w: u32, h: u32) -> Arc<RenderImage> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let c = b.at(x as f32 + 0.5, y as f32 + 0.5, w as f32, h as f32);
            px.extend_from_slice(&[c as u8, (c >> 8) as u8, (c >> 16) as u8, 255]);
        }
    }
    let frame = image::RgbaImage::from_raw(w, h, px).expect("backdrop size");
    Arc::new(RenderImage::new(SmallVec::from_elem(image::Frame::new(frame), 1)))
}
