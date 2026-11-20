//! Behind the window on a colour theme: its gradient, its picture (`scene_art`, drawn as SVG on a
//! background thread and kept for the window's size), the page's darkening, and what moves.
//!
//! The moving things tick on a timer at the scene's own rate, only while the window is in front
//! and the setting is on, and never when the system asks for less motion; otherwise they hold
//! still where they were. The window's contents are cached views, so a tick redraws this layer
//! and not the chat.

use super::{backdrop, scene_art};
use crate::prefs::prefs;
use crate::theme;
use gpui::{Context, IntoElement, ParentElement, Render, RenderImage, Styled, Task, Window, div, img};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The picture drawn for one scene at one size.
type Key = (&'static str, u32, u32, u32);

pub struct SceneFx {
    start: Instant,
    /// The time shown when the effects last stopped, so they hold where they were.
    held: f32,
    live: bool,
    ticker: Option<Task<()>>,
    /// The picture for the window now, and the one being drawn for a new size or scene.
    art: Option<(Key, Arc<RenderImage>)>,
    drawing: Option<(Key, Task<()>)>,
}

impl SceneFx {
    pub fn new() -> SceneFx {
        SceneFx { start: Instant::now(), held: 0., live: false, ticker: None, art: None, drawing: None }
    }

    fn now(&self) -> f32 {
        if self.live { self.start.elapsed().as_secs_f32() } else { self.held }
    }

    /// Ticks while live, at `fps`; the timer ends itself once it is not.
    fn tick(&mut self, fps: u64, cx: &mut Context<Self>) {
        if self.ticker.is_some() {
            return;
        }
        let every = Duration::from_millis(1000 / fps.max(1));
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(every).await;
                let go = this
                    .update(cx, |s, cx| {
                        if s.live {
                            cx.notify();
                        } else {
                            s.ticker = None;
                        }
                        s.live
                    })
                    .unwrap_or(false);
                if !go {
                    break;
                }
            }
        }));
    }

    /// The picture for `scene` at this size: the one ready, while a new one is drawn when the
    /// size settles (to the nearest 32 pixels, so resizing does not draw at every step).
    fn art(&mut self, scene: &'static str, w: f32, h: f32, scale: f32, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        let q = |v: f32| ((v / 32.).ceil() * 32.).max(32.) as u32;
        let key = (scene, q(w), q(h), (scale * 100.).round() as u32);
        let ready = self.art.as_ref().filter(|(k, _)| k.0 == scene).map(|(_, i)| i.clone());
        let have = self.art.as_ref().is_some_and(|(k, _)| *k == key) || self.drawing.as_ref().is_some_and(|(k, _)| *k == key);
        if !have {
            let task = cx.spawn(async move |this, cx| {
                // Let a resize settle before drawing for it.
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let still = this.update(cx, |s, _| s.drawing.as_ref().is_some_and(|(k, _)| *k == key)).unwrap_or(false);
                if !still {
                    return;
                }
                let image = cx
                    .background_executor()
                    .spawn(async move {
                        let (w, h, s) = (key.1 as f32, key.2 as f32, key.3 as f32 / 100.);
                        draw(scene, (w, h), ((w * s) as i32, (h * s) as i32))
                    })
                    .await;
                let _ = this.update(cx, |s, cx| {
                    s.drawing = None;
                    let Some(image) = image else { return };
                    if let Some((_, old)) = s.art.replace((key, image)) {
                        cx.drop_image(old, None);
                    }
                    cx.notify();
                });
            });
            self.drawing = Some((key, task));
        }
        ready
    }

    /// Lets the picture go, when the theme has none.
    fn forget(&mut self, cx: &mut Context<Self>) {
        self.drawing = None;
        if let Some((_, old)) = self.art.take() {
            cx.drop_image(old, None);
        }
    }
}

/// `scene`'s picture laid out for an area `size` (logical pixels) and drawn `pixels` big. Slow
/// for a window: off the main thread.
pub fn draw(scene: &str, (w, h): (f32, f32), (pw, ph): (i32, i32)) -> Option<Arc<RenderImage>> {
    let svg = scene_art::art(scene, w, h)?;
    let renderer = gpui::SvgRenderer::new(Arc::new(()));
    let tree = renderer.parse_svg(svg.as_bytes()).ok()?;
    let size = gpui::size(gpui::DevicePixels(pw.max(1)), gpui::DevicePixels(ph.max(1)));
    renderer.render_parsed(&tree, gpui::SvgSize::ExactSize(size)).ok()
}

/// Whether `scene`'s picture lies under the page's darkening.
pub fn under_the_page(scene: &str) -> bool {
    scene_art::under_the_page(scene)
}

impl Render for SceneFx {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::current();
        let layer = div().absolute().inset_0();
        let Some(b) = t.backdrop else {
            self.forget(cx);
            return layer;
        };
        let size = window.viewport_size();
        let (w, h): (f32, f32) = (size.width.into(), size.height.into());
        let scene = t.scene.unwrap_or("");
        let art = if scene_art::has_art(scene) {
            self.art(scene, w, h, window.scale_factor(), cx)
        } else {
            self.forget(cx);
            None
        };
        let fps = scene_art::fps(scene);
        let effects = fps.is_some() && prefs(cx).theme_effects;
        let live = effects && !cx.reduce_motion() && window.is_window_active();
        if live != self.live {
            if live {
                self.start = Instant::now() - Duration::from_secs_f32(self.held);
            } else {
                self.held = self.start.elapsed().as_secs_f32();
            }
            self.live = live;
        }
        if let (true, Some(fps)) = (live, fps) {
            self.tick(fps, cx);
        }
        let moving = if effects { scene_art::particles(scene, self.now(), w, h) } else { Vec::new() };
        // A world in daylight is darkened with the page so text reads over it; a night scene is
        // drawn dark already, and goes over the page so its lights stay bright.
        let under = scene_art::under_the_page(scene);
        let (art_under, art_over) = if under { (art, None) } else { (None, art) };
        let (behind, front) = if under { (moving, Vec::new()) } else { (Vec::new(), moving) };
        let picture = |a: Arc<RenderImage>| img(a).absolute().inset_0().size_full();
        layer
            .child(backdrop::fill(b, w, h, cx))
            .children(art_under.map(picture))
            .children(behind)
            .child(div().absolute().inset_0().bg(t.base))
            .children(art_over.map(picture))
            .children(front)
    }
}
