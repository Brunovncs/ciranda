//! Composite sharing: several screens, windows and the camera arranged on one canvas and sent as
//! one ordinary screen share, so neither the server nor anyone watching needs anything new. Each
//! screen or window is captured on a thread of its own, which scales its frames straight to the
//! part of the canvas it covers; the camera's frames come from whichever capture has the device.
//! The canvas thread puts the newest of each together, bottom first, at the share's frame rate,
//! whether the window is on screen or not, and hands it to the encoder like any screen's frame.
//!
//! The layout is kept in fractions of the canvas, so another resolution keeps the arrangement, and
//! a crop in fractions of the source, so it survives the source changing size. Every edit is only
//! a change to what is drawn: it is on viewers' screens with the next frame, nothing renegotiated.

use super::camera::{self, Capture, Tap};
use super::screen::{self, EncoderFrame, ScreenChoice, ScreenSource, SourceKind};
use super::video::{self, FrameSlot};
use super::yuv;
use crate::core::settings::{Background, CompositeLayer};
use libwebrtc::desktop_capturer::{CaptureError, DesktopFrame};
use libwebrtc::native::yuv_helper;
use libwebrtc::prelude::VideoBuffer;
use libwebrtc::video_frame::{I420Buffer, NV12Buffer, VideoFrame, VideoRotation};
use libwebrtc::video_source::native::NativeVideoSource;
use parking_lot::{Mutex, MutexGuard};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How near the canvas's edges or middle a dragged source has to come to sit on them.
pub const SNAP: f32 = 0.012;
/// A crop never leaves less than this much of a side.
const MIN_VISIBLE: f32 = 0.05;
/// The smallest a source can be made, as a part of the canvas.
const MIN_SIZE: f32 = 0.03;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Screen(u64),
    Window(u64),
    /// By device id.
    Camera(String),
}

impl Source {
    pub fn is_camera(&self) -> bool {
        matches!(self, Source::Camera(_))
    }
}

/// A part of the canvas, in fractions of it: it may reach past the edges.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }

    /// `w` × `h` around the same middle.
    fn around(self, w: f32, h: f32) -> Rect {
        let (cx, cy) = (self.x + self.w / 2., self.y + self.h / 2.);
        Rect { x: cx - w / 2., y: cy - h / 2., w, h }
    }
}

/// What is trimmed off each side of a source, in fractions of it.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Crop {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

impl Crop {
    pub fn any(&self) -> bool {
        self.l > 0. || self.t > 0. || self.r > 0. || self.b > 0.
    }

    /// The part of the source's width and height left.
    fn kept(&self) -> (f32, f32) {
        (1. - self.l - self.r, 1. - self.t - self.b)
    }
}

/// A grip on a source's box: its corners resize, and every one of them crops or stretches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Nw,
    N,
    Ne,
    E,
    Se,
    S,
    Sw,
    W,
}

impl Handle {
    pub const ALL: [Handle; 8] = [Handle::Nw, Handle::N, Handle::Ne, Handle::E, Handle::Se, Handle::S, Handle::Sw, Handle::W];

    pub fn west(self) -> bool {
        matches!(self, Handle::Nw | Handle::W | Handle::Sw)
    }

    pub fn east(self) -> bool {
        matches!(self, Handle::Ne | Handle::E | Handle::Se)
    }

    pub fn north(self) -> bool {
        matches!(self, Handle::Nw | Handle::N | Handle::Ne)
    }

    pub fn south(self) -> bool {
        matches!(self, Handle::Sw | Handle::S | Handle::Se)
    }

    pub fn corner(self) -> bool {
        (self.west() || self.east()) && (self.north() || self.south())
    }

    /// Where it sits on the box, in fractions of it from the top left.
    pub fn spot(self) -> (f32, f32) {
        let x = if self.west() {
            0.
        } else if self.east() {
            1.
        } else {
            0.5
        };
        let y = if self.north() {
            0.
        } else if self.south() {
            1.
        } else {
            0.5
        };
        (x, y)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    /// Its own in this composition, whatever it shows.
    pub id: u64,
    pub source: Source,
    pub name: String,
    /// A window's program file, to find it again once its handle has changed.
    pub app: String,
    pub rect: Rect,
    /// It has a place on the canvas: one that wasn't in the saved layout gets it once its size
    /// is known, and isn't drawn until then.
    pub placed: bool,
    pub crop: Crop,
    /// The source's own size, as last seen.
    pub size: Option<(u32, u32)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub canvas: (u32, u32),
    /// Bottom first: drawn in order, so the last is on top.
    pub layers: Vec<Layer>,
    next: u64,
}

impl Layout {
    pub fn new(canvas: (u32, u32)) -> Layout {
        Layout { canvas, layers: Vec::new(), next: 0 }
    }

    /// A source on top, placed once its size is known.
    pub fn add(&mut self, source: Source, name: String, app: String) -> u64 {
        self.next += 1;
        let id = self.next;
        self.layers.push(Layer { id, source, name, app, rect: Rect::default(), placed: false, crop: Crop::default(), size: None });
        id
    }

    /// A source on top, where it was before.
    pub fn add_at(&mut self, source: Source, name: String, app: String, rect: Rect, crop: Crop) -> u64 {
        let id = self.add(source, name, app);
        if let Some(l) = self.layers.last_mut() {
            (l.rect, l.crop, l.placed) = (rect, crop, true);
        }
        id
    }

    pub fn layer(&self, id: u64) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    fn index(&self, id: u64) -> Option<usize> {
        self.layers.iter().position(|l| l.id == id)
    }

    pub fn remove(&mut self, id: u64) -> Option<Layer> {
        self.index(id).map(|i| self.layers.remove(i))
    }

    pub fn to_front(&mut self, id: u64) {
        if let Some(i) = self.index(id) {
            let l = self.layers.remove(i);
            self.layers.push(l);
        }
    }

    pub fn to_back(&mut self, id: u64) {
        if let Some(i) = self.index(id) {
            let l = self.layers.remove(i);
            self.layers.insert(0, l);
        }
    }

    /// One step up or down the stack.
    pub fn step(&mut self, id: u64, up: bool) {
        let Some(i) = self.index(id) else { return };
        let j = if up { i + 1 } else { i.wrapping_sub(1) };
        if j < self.layers.len() {
            self.layers.swap(i, j);
        }
    }

    /// The topmost source under a point of the canvas.
    pub fn hit(&self, x: f32, y: f32) -> Option<u64> {
        self.layers.iter().rev().find(|l| l.placed && l.rect.contains(x, y)).map(|l| l.id)
    }

    /// The shape a source's visible part has on the canvas: its width over its height, both in
    /// fractions of the canvas.
    pub fn aspect(&self, l: &Layer) -> Option<f32> {
        let (sw, sh) = l.size?;
        let (kw, kh) = l.crop.kept();
        let (cw, ch) = self.canvas;
        (sw > 0 && sh > 0 && cw > 0 && ch > 0).then(|| (sw as f32 * kw) / (sh as f32 * kh) * ch as f32 / cw as f32)
    }

    /// Records a source's size; one still without a place gets one now. True when it changed.
    pub fn seen(&mut self, id: u64, size: (u32, u32)) -> bool {
        let Some(i) = self.index(id) else { return false };
        if self.layers[i].size == Some(size) {
            return false;
        }
        self.layers[i].size = Some(size);
        if !self.layers[i].placed {
            self.place(i);
        }
        true
    }

    /// Where a new source goes: the bottom one fills the canvas, letterboxed; a camera goes in the
    /// corner, as on every stream overlay; anything else half as wide, each a step further in.
    fn place(&mut self, i: usize) {
        let Some(aspect) = self.aspect(&self.layers[i]) else { return };
        let camera = self.layers[i].source.is_camera();
        let w = if i == 0 {
            aspect.min(1.)
        } else if camera {
            0.25
        } else {
            0.5
        };
        let h = w / aspect;
        let (x, y) = if i == 0 {
            ((1. - w) / 2., (1. - h) / 2.)
        } else if camera {
            (1. - w - 0.02, 1. - h - 0.03)
        } else {
            let step = (i % 5) as f32 * 0.04;
            (0.1 + step, 0.1 + step)
        };
        let l = &mut self.layers[i];
        (l.rect, l.placed) = (Rect { x, y, w, h }, true);
    }

    fn with_aspect(&mut self, id: u64, f: impl FnOnce(&mut Layer, f32, (u32, u32))) {
        let Some(i) = self.index(id) else { return };
        let Some(aspect) = self.aspect(&self.layers[i]) else { return };
        let canvas = self.canvas;
        f(&mut self.layers[i], aspect, canvas);
    }

    /// As big as fits, in the middle, its shape kept.
    pub fn fit(&mut self, id: u64) {
        self.with_aspect(id, |l, aspect, _| {
            let (w, h) = if aspect >= 1. { (1., 1. / aspect) } else { (aspect, 1.) };
            l.rect = Rect { x: (1. - w) / 2., y: (1. - h) / 2., w, h };
        });
    }

    /// The whole canvas covered, its shape kept: what doesn't fit is cropped off the two long
    /// sides evenly.
    pub fn fill(&mut self, id: u64) {
        let Some(i) = self.index(id) else { return };
        let (Some((sw, sh)), (cw, ch)) = (self.layers[i].size, self.canvas) else { return };
        if sh == 0 || ch == 0 {
            return;
        }
        let (canvas, source) = (cw as f32 / ch as f32, sw as f32 / sh as f32);
        let mut crop = Crop::default();
        if source > canvas {
            crop.l = (1. - canvas / source) / 2.;
            crop.r = crop.l;
        } else {
            crop.t = (1. - source / canvas) / 2.;
            crop.b = crop.t;
        }
        let l = &mut self.layers[i];
        (l.crop, l.rect) = (crop, Rect { x: 0., y: 0., w: 1., h: 1. });
    }

    /// One source pixel to one canvas pixel, around the same middle.
    pub fn original_size(&mut self, id: u64) {
        let Some(i) = self.index(id) else { return };
        let (Some((sw, sh)), (cw, ch)) = (self.layers[i].size, self.canvas) else { return };
        let l = &mut self.layers[i];
        let (kw, kh) = l.crop.kept();
        l.rect = l.rect.around(sw as f32 * kw / cw.max(1) as f32, sh as f32 * kh / ch.max(1) as f32);
    }

    /// A part of the canvas's width, its shape kept, around the same middle.
    pub fn scale_to(&mut self, id: u64, width: f32) {
        self.with_aspect(id, |l, aspect, _| l.rect = l.rect.around(width, width / aspect));
    }

    /// The whole source again: the width kept, the height following its shape.
    pub fn reset_crop(&mut self, id: u64) {
        let Some(i) = self.index(id) else { return };
        self.layers[i].crop = Crop::default();
        self.with_aspect(id, |l, aspect, _| l.rect.h = l.rect.w / aspect);
    }

    /// Puts a source where a drag left it.
    pub fn set(&mut self, id: u64, rect: Rect, crop: Crop) {
        if let Some(i) = self.index(id) {
            let l = &mut self.layers[i];
            (l.rect, l.crop) = (rect, crop);
        }
    }
}

/// A source dragged by (`dx`, `dy`) from `orig`, sitting on the canvas's edges and middle when it
/// comes near them: putting a camera exactly in the corner is what anyone does most here.
pub fn moved(orig: Rect, dx: f32, dy: f32) -> Rect {
    let snap = |v: f32, size: f32| {
        if v.abs() < SNAP {
            0.
        } else if (v + size - 1.).abs() < SNAP {
            1. - size
        } else if (v + size / 2. - 0.5).abs() < SNAP {
            0.5 - size / 2.
        } else {
            v
        }
    };
    Rect { x: snap(orig.x + dx, orig.w), y: snap(orig.y + dy, orig.h), ..orig }
}

/// A corner dragged, the shape kept (a stretched screen helps nobody): the size follows whichever
/// way the pointer went further. `None` for an edge.
pub fn resized(orig: Rect, handle: Handle, dx: f32, dy: f32) -> Option<Rect> {
    if !handle.corner() || orig.h <= 0. {
        return None;
    }
    let aspect = orig.w / orig.h;
    let sx = if handle.west() { -1. } else { 1. };
    let sy = if handle.north() { -1. } else { 1. };
    let by_x = orig.w + sx * dx;
    let by_y = (orig.h + sy * dy) * aspect;
    let w = if (by_x - orig.w).abs() > (by_y - orig.w).abs() { by_x } else { by_y }.max(MIN_SIZE);
    let h = w / aspect;
    let x = if handle.west() { orig.x + orig.w - w } else { orig.x };
    let y = if handle.north() { orig.y + orig.h - h } else { orig.y };
    Some(Rect { x, y, w, h })
}

/// Any grip dragged any way, the source stretched to the box: for stretching on purpose. `None`
/// once it would be too small.
pub fn stretched(orig: Rect, handle: Handle, dx: f32, dy: f32) -> Option<Rect> {
    let mut r = orig;
    if handle.west() {
        (r.x, r.w) = (orig.x + dx, orig.w - dx);
    }
    if handle.east() {
        r.w = orig.w + dx;
    }
    if handle.north() {
        (r.y, r.h) = (orig.y + dy, orig.h - dy);
    }
    if handle.south() {
        r.h = orig.h + dy;
    }
    (r.w >= MIN_SIZE && r.h >= MIN_SIZE).then_some(r)
}

/// An edge trimmed in place, what is left not rescaled: dragging the left edge in by a tenth of
/// the canvas hides just that much of the source and moves the edge with it, as cropping a photo.
pub fn cropped(orig: Rect, crop: Crop, handle: Handle, dx: f32, dy: f32) -> (Rect, Crop) {
    let (kw, kh) = crop.kept();
    // Source fraction per canvas fraction, at the size the drag started at.
    let (per_x, per_y) = (kw / orig.w.max(f32::EPSILON), kh / orig.h.max(f32::EPSILON));
    let more = |delta: f32, side: f32, opposite: f32| delta.max(-side).min(1. - MIN_VISIBLE - opposite - side);
    let (mut r, mut c) = (orig, crop);
    if handle.west() {
        let d = more(dx * per_x, c.l, c.r);
        c.l += d;
        (r.x, r.w) = (orig.x + d / per_x, orig.w - d / per_x);
    }
    if handle.east() {
        let d = more(-dx * per_x, c.r, c.l);
        c.r += d;
        r.w = orig.w - d / per_x;
    }
    if handle.north() {
        let d = more(dy * per_y, c.t, c.b);
        c.t += d;
        (r.y, r.h) = (orig.y + d / per_y, orig.h - d / per_y);
    }
    if handle.south() {
        let d = more(-dy * per_y, c.b, c.t);
        c.b += d;
        r.h = orig.h - d / per_y;
    }
    (r, c)
}

/// Where a source lands in pixels: the part of it shown (its corner on even pixels) and the part
/// of the canvas it fills (all even, so chroma lines up), only what is on the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub src: yuv::Rect,
    pub dst: yuv::Rect,
}

/// A source of `size` pixels at `rect` with `crop`, on a canvas of `canvas` pixels; `None` when
/// none of it is on the canvas.
pub fn placement(rect: Rect, crop: Crop, (sw, sh): (u32, u32), (cw, ch): (u32, u32)) -> Option<Placement> {
    if sw < 2 || sh < 2 || cw < 2 || ch < 2 || rect.w <= 0. || rect.h <= 0. {
        return None;
    }
    let even = |v: f64, max: u32| ((v / 2.).round() * 2.).clamp(0., max as f64) as u32;
    let (x0, x1) = (rect.x as f64 * cw as f64, (rect.x + rect.w) as f64 * cw as f64);
    let (y0, y1) = (rect.y as f64 * ch as f64, (rect.y + rect.h) as f64 * ch as f64);
    let (dx0, dx1, dy0, dy1) = (even(x0, cw), even(x1, cw), even(y0, ch), even(y1, ch));
    if dx1 < dx0 + 2 || dy1 < dy0 + 2 {
        return None;
    }
    // The canvas span back onto the cropped source.
    let span = |(d0, d1): (u32, u32), (a0, a1): (f64, f64), (c0, c1): (f32, f32), s: u32| {
        let (k0, k1) = (c0 as f64 * s as f64, (1. - c1 as f64) * s as f64);
        let at = |d: u32| k0 + (d as f64 - a0) / (a1 - a0) * (k1 - k0);
        let s0 = (at(d0).floor() as i64).clamp(0, s as i64 - 2) as u32 & !1;
        let s1 = (at(d1).round() as i64).clamp(s0 as i64 + 2, s as i64) as u32;
        (s0, s1 - s0)
    };
    let (sx, w) = span((dx0, dx1), (x0, x1), (crop.l, crop.r), sw);
    let (sy, h) = span((dy0, dy1), (y0, y1), (crop.t, crop.b), sh);
    Some(Placement { src: (sx, sy, w, h), dst: (dx0, dy0, dx1 - dx0, dy1 - dy0) })
}

/// Black, as the encoder takes it: limited-range YUV.
pub fn clear(b: &mut I420Buffer) {
    let (y, u, v) = b.data_mut();
    y.fill(16);
    u.fill(128);
    v.fill(128);
}

/// `pic` copied onto `canvas` with its corner at (`x`, `y`), both even; what would fall off the
/// canvas is left out.
pub fn blit(canvas: &mut I420Buffer, pic: &I420Buffer, (x, y): (u32, u32)) {
    let (cw, ch) = (canvas.width(), canvas.height());
    if x >= cw || y >= ch {
        return;
    }
    debug_assert!(x % 2 == 0 && y % 2 == 0, "odd corner");
    let (w, h) = (pic.width().min(cw - x), pic.height().min(ch - y));
    let (ps, cs) = (pic.strides(), canvas.strides());
    let (py, pu, pv) = pic.data();
    let (qy, qu, qv) = canvas.data_mut();
    let copy = |src: &[u8], ss: u32, dst: &mut [u8], ds: u32, (x, y, w, h): (u32, u32, u32, u32)| {
        for r in 0..h {
            let from = &src[(r * ss) as usize..][..w as usize];
            dst[((y + r) * ds + x) as usize..][..w as usize].copy_from_slice(from);
        }
    };
    copy(py, ps.0, qy, cs.0, (x, y, w, h));
    let chroma = (x / 2, y / 2, w.div_ceil(2), h.div_ceil(2));
    copy(pu, ps.1, qu, cs.1, chroma);
    copy(pv, ps.2, qv, cs.2, chroma);
}

/// The layout as settings keep it: the sources with a place, bottom first.
pub fn saved(layout: &Layout) -> Vec<CompositeLayer> {
    layout
        .layers
        .iter()
        .filter(|l| l.placed)
        .map(|l| {
            let (kind, id) = match &l.source {
                Source::Screen(id) => ("screen", id.to_string()),
                Source::Window(id) => ("window", id.to_string()),
                Source::Camera(device) => ("camera", device.clone()),
            };
            CompositeLayer {
                kind: kind.into(),
                id,
                title: l.name.clone(),
                app: l.app.clone(),
                x: l.rect.x,
                y: l.rect.y,
                w: l.rect.w,
                h: l.rect.h,
                crop: [l.crop.l, l.crop.t, l.crop.r, l.crop.b],
            }
        })
        .collect()
}

impl CompositeLayer {
    pub fn rect(&self) -> Rect {
        Rect { x: self.x, y: self.y, w: self.w, h: self.h }
    }

    pub fn crop(&self) -> Crop {
        let [l, t, r, b] = self.crop;
        Crop { l, t, r, b }
    }
}

/// A saved source among those there now, with its name and program: a screen by its id, a window
/// by its handle while that still names the same program or title, else by program and title,
/// program alone, or title alone; a camera by its device.
pub fn find(
    saved: &CompositeLayer,
    screens: &[ScreenSource],
    windows: &[(ScreenSource, String)],
    cameras: &[camera::Device],
) -> Option<(Source, String, String)> {
    match saved.kind.as_str() {
        "screen" => {
            let s = screens.iter().find(|s| s.id.to_string() == saved.id).or_else(|| screens.iter().find(|s| s.title == saved.title))?;
            Some((Source::Screen(s.id), s.title.clone(), String::new()))
        }
        "window" => {
            let app = |a: &str| !saved.app.is_empty() && a.eq_ignore_ascii_case(&saved.app);
            let (s, a) = windows
                .iter()
                .find(|(s, a)| s.id.to_string() == saved.id && (s.title == saved.title || app(a)))
                .or_else(|| windows.iter().find(|(s, a)| app(a) && s.title == saved.title))
                .or_else(|| windows.iter().find(|(_, a)| app(a)))
                .or_else(|| windows.iter().find(|(s, _)| s.title == saved.title))?;
            Some((Source::Window(s.id), s.title.clone(), a.clone()))
        }
        "camera" => {
            let c = cameras.iter().find(|c| c.id == saved.id)?;
            Some((Source::Camera(c.id.clone()), c.name.clone(), String::new()))
        }
        _ => None,
    }
}

/// The program file of the process that owns a window, such as "obs64.exe"; empty if unknown.
#[cfg(windows)]
pub fn window_app(hwnd: u64) -> String {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
    let Some(pid) = super::loopback::window_process(hwnd) else { return String::new() };
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: the handle is checked and closed here; the buffer's length is passed with it.
    let ok = unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return String::new();
        }
        let ok = QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(process);
        ok
    };
    if ok == 0 {
        return String::new();
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    path.rsplit(['\\', '/']).next().unwrap_or_default().to_string()
}

#[cfg(not(windows))]
pub fn window_app(_: u64) -> String {
    String::new()
}

/// A composition being shared: its layout, edited from the window while the canvas thread draws
/// it.
pub struct Composition {
    layout: Mutex<Layout>,
    /// Bumped by every change, so the canvas knows to draw again.
    version: AtomicU64,
    /// The editor is open: your own preview comes bigger and more often.
    pub editing: AtomicBool,
    /// Every source it had has ended by itself (windows closed, a screen unplugged).
    gone: AtomicBool,
    /// For a camera the composition has to open itself.
    background: Background,
}

impl Composition {
    pub fn new(layout: Layout, background: Background) -> Arc<Composition> {
        Arc::new(Composition {
            layout: Mutex::new(layout),
            version: AtomicU64::new(1),
            editing: AtomicBool::new(false),
            gone: AtomicBool::new(false),
            background,
        })
    }

    pub fn layout(&self) -> MutexGuard<'_, Layout> {
        self.layout.lock()
    }

    pub fn edit<T>(&self, f: impl FnOnce(&mut Layout) -> T) -> T {
        let out = f(&mut self.layout.lock());
        self.version.fetch_add(1, Ordering::Release);
        out
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    pub fn gone(&self) -> bool {
        self.gone.load(Ordering::Relaxed)
    }

    fn seen(&self, id: u64, size: (u32, u32)) {
        let mut layout = self.layout.lock();
        if layout.seen(id, size) {
            self.version.fetch_add(1, Ordering::Release);
        }
    }

    fn placement_of(&self, id: u64) -> Option<Placement> {
        let layout = self.layout.lock();
        let l = layout.layer(id).filter(|l| l.placed)?;
        placement(l.rect, l.crop, l.size?, layout.canvas)
    }
}

#[derive(Default)]
struct Feed {
    /// The newest picture of a screen or window, made by its thread at the size it has on the
    /// canvas.
    fresh: Mutex<Option<I420Buffer>>,
    /// A picture the canvas is done with, for the next one to be drawn into.
    spare: Mutex<Option<I420Buffer>>,
    stop: AtomicBool,
    /// Closed, or unplugged.
    ended: AtomicBool,
}

/// Captures one screen or window for the canvas until told to stop or it ends.
fn capture(kind: SourceKind, id: u64, layer: u64, comp: Arc<Composition>, feed: Arc<Feed>, fps: Arc<AtomicU32>) {
    let found = screen::capturer(kind).and_then(|c| {
        let target = c.get_source_list().into_iter().find(|s| s.id() == id)?;
        Some((c, target))
    });
    let Some((mut c, target)) = found else {
        log::info!("composite: the {kind:?} {id} is gone");
        feed.ended.store(true, Ordering::Relaxed);
        return;
    };
    let (f, cmp) = (feed.clone(), comp.clone());
    let mut bgra = Vec::new();
    c.start_capture(Some(target), move |frame| match frame {
        Ok(frame) => draw(&frame, layer, &cmp, &f, &mut bgra),
        Err(CaptureError::Permanent) => f.ended.store(true, Ordering::Relaxed),
        Err(CaptureError::Temporary) => {}
    });
    let mut checked = Instant::now();
    while !feed.stop.load(Ordering::Relaxed) && !feed.ended.load(Ordering::Relaxed) {
        let tick = Instant::now();
        c.capture_frame();
        // A closed window may only stop giving frames; the list says for sure.
        if checked.elapsed() >= Duration::from_secs(1) {
            checked = Instant::now();
            if !c.get_source_list().iter().any(|s| s.id() == id) {
                log::info!("composite: the {kind:?} {id} closed");
                feed.ended.store(true, Ordering::Relaxed);
            }
        }
        let frame = Duration::from_secs_f64(1. / fps.load(Ordering::Relaxed).max(1) as f64);
        if let Some(rest) = frame.checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

/// A captured frame scaled to the part of the canvas its source covers now, and left for the
/// canvas. Scaling goes first, in BGRA, so only the pixels shown are converted.
fn draw(f: &DesktopFrame, layer: u64, comp: &Composition, feed: &Feed, bgra: &mut Vec<u8>) {
    let (w, h) = (f.width().max(0) as u32 & !1, f.height().max(0) as u32 & !1);
    if w < 2 || h < 2 {
        return;
    }
    comp.seen(layer, (w, h));
    let Some(at) = comp.placement_of(layer) else { return };
    let ((sx, sy, sw, sh), (_, _, dw, dh)) = (at.src, at.dst);
    let stride = f.stride();
    let Some(data) = f.data().get((sy * stride + sx * 4) as usize..) else { return };
    bgra.resize((dw * dh * 4) as usize, 0);
    yuv::scale_bgra(data, stride, sw, sh, bgra, dw, dh);
    let spare = feed.spare.lock().take().filter(|b| (b.width(), b.height()) == (dw, dh));
    let mut buf = spare.unwrap_or_else(|| I420Buffer::new(dw, dh));
    let (ys, us, vs) = buf.strides();
    let (y, u, v) = buf.data_mut();
    yuv_helper::argb_to_i420(bgra, dw * 4, y, ys, u, us, v, vs, dw as i32, dh as i32);
    let old = feed.fresh.lock().replace(buf);
    if let Some(old) = old {
        *feed.spare.lock() = Some(old);
    }
}

/// How long nobody has to have the camera before a composition opens it itself, at first; it
/// waits longer after each try that got no frame.
const CAMERA_FREE: Duration = Duration::from_millis(800);

/// A camera on the canvas: the frames of whichever capture has the device, the call's camera
/// or a preview, or else of one the composition opens, which hands the device over to them.
struct CameraFeed {
    device: String,
    tap: Option<Arc<Tap>>,
    own: Option<Capture>,
    free_since: Option<Instant>,
    tries: u32,
    serial: u64,
    picture: Option<I420Buffer>,
    at: Option<Placement>,
}

impl CameraFeed {
    fn new(device: String) -> CameraFeed {
        CameraFeed { device, tap: None, own: None, free_since: None, tries: 0, serial: 0, picture: None, at: None }
    }

    fn follow(&mut self, background: &Background) {
        if self.tap.as_ref().is_some_and(|t| !t.ended.load(Ordering::Relaxed)) {
            return;
        }
        if self.own.take().is_some() && self.serial == 0 {
            self.tries += 1;
        }
        self.tap = None;
        match camera::borrow(&self.device) {
            Ok(tap) => (self.tap, self.free_since) = (Some(tap), None),
            // Another camera is on: this one waits for it to go.
            Err(true) => self.free_since = None,
            Err(false) => {
                let since = *self.free_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= (CAMERA_FREE * 2u32.pow(self.tries.min(4))).min(Duration::from_secs(10)) {
                    let own = Capture::lending(&self.device, background.clone());
                    (self.tap, self.own, self.free_since) = (Some(own.tap.clone()), Some(own), None);
                }
            }
        }
    }

    /// Scales the newest frame to where the camera is now, when either changed; true if it did.
    fn pull(&mut self, l: &Layer, comp: &Composition, canvas: (u32, u32)) -> bool {
        self.follow(&comp.background);
        let Some(tap) = self.tap.clone() else { return false };
        let serial = tap.serial();
        let at = l.size.and_then(|s| if l.placed { placement(l.rect, l.crop, s, canvas) } else { None });
        if serial == self.serial && at == self.at {
            return false;
        }
        if serial != self.serial {
            self.tries = 0;
        }
        self.serial = serial;
        let picture = &mut self.picture;
        let drawn = tap.with(|f| {
            let size = (f.width() & !1, f.height() & !1);
            if l.size != Some(size) {
                comp.seen(l.id, size);
                return None;
            }
            let at = at?;
            let (_, _, dw, dh) = at.dst;
            yuv::scale_rect(f, at.src, yuv::pooled(picture, dw, dh));
            Some(at)
        });
        self.at = drawn.flatten();
        true
    }
}

/// What feeds one source's pictures to the canvas.
enum Feeding {
    Desktop { feed: Arc<Feed>, current: Option<I420Buffer> },
    Camera(Box<CameraFeed>),
}

impl Feeding {
    fn start(l: &Layer, comp: &Arc<Composition>, fps: &Arc<AtomicU32>) -> Feeding {
        let (kind, id) = match &l.source {
            Source::Screen(id) => (SourceKind::Screen, *id),
            Source::Window(id) => (SourceKind::Window, *id),
            Source::Camera(device) => return Feeding::Camera(Box::new(CameraFeed::new(device.clone()))),
        };
        let feed = Arc::new(Feed::default());
        let (layer, cmp, f, rate) = (l.id, comp.clone(), feed.clone(), fps.clone());
        let started = std::thread::Builder::new().name("ciranda-composite".into()).spawn(move || capture(kind, id, layer, cmp, f, rate));
        if let Err(e) = started {
            log::warn!("no thread for a composite source: {e}");
            feed.ended.store(true, Ordering::Relaxed);
        }
        Feeding::Desktop { feed, current: None }
    }

    fn ended(&self) -> bool {
        match self {
            Feeding::Desktop { feed, .. } => feed.ended.load(Ordering::Relaxed),
            Feeding::Camera(_) => false,
        }
    }

    /// Takes in the source's newest picture; true if there was one.
    fn pull(&mut self, l: &Layer, comp: &Composition, canvas: (u32, u32)) -> bool {
        match self {
            Feeding::Desktop { feed, current } => {
                let Some(fresh) = feed.fresh.lock().take() else { return false };
                if let Some(old) = current.replace(fresh) {
                    *feed.spare.lock() = Some(old);
                }
                true
            }
            Feeding::Camera(c) => c.pull(l, comp, canvas),
        }
    }

    fn picture(&self) -> Option<&I420Buffer> {
        match self {
            Feeding::Desktop { current, .. } => current.as_ref(),
            Feeding::Camera(c) => c.at.and(c.picture.as_ref()),
        }
    }
}

impl Drop for Feeding {
    fn drop(&mut self) {
        if let Feeding::Desktop { feed, .. } = self {
            feed.stop.store(true, Ordering::Relaxed);
        }
    }
}

/// Draws the canvas from the newest picture of each source, bottom first; one whose picture was
/// made for another size (it was just resized) is scaled to fit until its next one comes.
fn compose(canvas: &mut I420Buffer, layers: &[Layer], feeds: &HashMap<u64, Feeding>, scratch: &mut Option<I420Buffer>) {
    clear(canvas);
    let size = (canvas.width(), canvas.height());
    for l in layers.iter().filter(|l| l.placed) {
        let (Some(src), Some(pic)) = (l.size, feeds.get(&l.id).and_then(Feeding::picture)) else { continue };
        let Some(at) = placement(l.rect, l.crop, src, size) else { continue };
        let (x, y, w, h) = at.dst;
        if (pic.width(), pic.height()) == (w, h) {
            blit(canvas, pic, (x, y));
        } else {
            let fitted = yuv::pooled(scratch, w, h);
            yuv::scale(pic, fitted);
            blit(canvas, fitted, (x, y));
        }
    }
}

/// The canvas's size: the shape of the screen its source names, 4K wide at most, at the
/// resolution sent.
pub fn canvas_size(choice: &ScreenChoice) -> (u32, u32) {
    let (w, h) = choice.source.size.unwrap_or((1920, 1080));
    let (w, h) = if w > 3840 { (3840, (h as u64 * 3840 / w as u64) as u32) } else { (w, h) };
    screen::fit(w.max(2), h.max(2), choice.resolution)
}

/// The canvas thread: runs the composition in `choice` until the share stops (`None`) or is
/// switched to a single screen or window (that choice). A switch to another composition, or to
/// this one at another size or rate, carries on here.
pub(super) fn run(
    mut choice: ScreenChoice,
    switch: &mut dyn FnMut() -> Option<ScreenChoice>,
    source: &NativeVideoSource,
    preview: &FrameSlot,
    stop: &AtomicBool,
    hardware: &dyn Fn() -> bool,
) -> Option<ScreenChoice> {
    let fallback = || Composition::new(Layout::new((1920, 1080)), Background::None);
    let mut comp = choice.composition.clone().unwrap_or_else(fallback);
    let fps = Arc::new(AtomicU32::new(choice.fps));
    let mut feeds: HashMap<u64, Feeding> = HashMap::new();
    let (mut canvas, mut scratch) = (None::<I420Buffer>, None::<I420Buffer>);
    let mut drawn = 0u64;
    let mut frames = video::Scratch::default();
    let mut replay_scratch = Vec::new();
    let started = Instant::now();
    let mut preview_at = Instant::now() - Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {
        let tick = Instant::now();
        while let Some(next) = switch() {
            if next.source.kind != SourceKind::Composite {
                return Some(next);
            }
            if let Some(other) = next.composition.clone().filter(|c| !Arc::ptr_eq(c, &comp)) {
                (comp, drawn) = (other, 0);
                feeds.clear();
            }
            fps.store(next.fps, Ordering::Relaxed);
            choice = next;
        }
        let size = canvas_size(&choice);
        if comp.layout().canvas != size {
            comp.edit(|l| l.canvas = size);
        }
        let layers = comp.layout().layers.clone();
        feeds.retain(|id, _| layers.iter().any(|l| l.id == *id));
        for l in &layers {
            feeds.entry(l.id).or_insert_with(|| Feeding::start(l, &comp, &fps));
        }
        // A source that ended (its window closed) leaves the canvas, not the whole share; the
        // share ends once every one has.
        let ended: Vec<u64> = feeds.iter().filter(|(_, f)| f.ended()).map(|(id, _)| *id).collect();
        if !ended.is_empty() {
            let left = comp.edit(|l| {
                for id in &ended {
                    l.remove(*id);
                }
                l.layers.len()
            });
            feeds.retain(|id, _| !ended.contains(id));
            if left == 0 {
                log::info!("composite: every source has ended");
                comp.gone.store(true, Ordering::Relaxed);
            }
            continue;
        }
        let version = comp.version();
        let mut dirty = version != drawn;
        drawn = version;
        for l in &layers {
            if let Some(f) = feeds.get_mut(&l.id) {
                dirty |= f.pull(l, &comp, size);
            }
        }
        let fresh = canvas.as_ref().is_none_or(|c| (c.width(), c.height()) != size);
        let cv = yuv::pooled(&mut canvas, size.0, size.1);
        if dirty || fresh {
            compose(cv, &layers, &feeds, &mut scratch);
        }
        // Always a new buffer: the encoder reads it after `capture_frame` returns.
        let (w, h) = size;
        let (sy, su, sv) = cv.strides();
        let (py, pu, pv) = cv.data();
        let out = if hardware() {
            let mut out = NV12Buffer::new(w, h);
            let (dy, duv) = out.strides();
            let (qy, quv) = out.data_mut();
            yuv_helper::i420_to_nv12(py, sy, pu, su, pv, sv, qy, dy, quv, duv, w as i32, h as i32);
            EncoderFrame::Nv12(out)
        } else {
            let mut out = I420Buffer::new(w, h);
            yuv::scale(cv, &mut out);
            EncoderFrame::I420(out)
        };
        if super::replay::recording() {
            screen::keep_for_replay(&out, choice.fps, &mut replay_scratch);
        }
        let mut vf = VideoFrame::new(VideoRotation::VideoRotation0, out);
        vf.timestamp_us = started.elapsed().as_micros() as i64;
        source.capture_frame(&vf);
        // Your own preview: a few frames a second, more and bigger while you arrange it.
        let editing = comp.editing.load(Ordering::Relaxed);
        let every = Duration::from_millis(if editing { 50 } else { 200 });
        if !video::window_hidden() && preview.watched() && preview_at.elapsed() >= every {
            preview_at = Instant::now();
            let (tw, th) = screen::preview_size(w, h, if editing { 1280 } else { 640 });
            preview.put(frames.i420(preview.id, tw, th, |y, u, v| yuv::scale_into(cv, y, u, v, tw, th)));
        }
        let frame = Duration::from_secs_f64(1. / choice.fps.max(1) as f64);
        if let Some(rest) = frame.checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn rect_near(a: Rect, b: Rect) -> bool {
        near(a.x, b.x) && near(a.y, b.y) && near(a.w, b.w) && near(a.h, b.h)
    }

    #[test]
    fn a_dragged_source_snaps_to_the_edges_and_the_middle() {
        let orig = Rect { x: 0.3, y: 0.3, w: 0.25, h: 0.25 };
        // Near the left and top edges.
        assert_eq!(moved(orig, -0.295, -0.31), Rect { x: 0., y: 0., ..orig });
        // Near the right and bottom ones.
        assert!(rect_near(moved(orig, 0.445, 0.45), Rect { x: 0.75, y: 0.75, ..orig }));
        // Its middle near the canvas's.
        assert!(rect_near(moved(orig, 0.08, -0.05), Rect { x: 0.375, y: 0.25, ..orig }));
        // Anywhere else, where it was dropped.
        assert!(rect_near(moved(orig, 0.1, 0.2), Rect { x: 0.4, y: 0.5, ..orig }));
    }

    #[test]
    fn a_corner_resizes_keeping_the_shape() {
        let orig = Rect { x: 0.2, y: 0.2, w: 0.4, h: 0.2 };
        // The bottom right corner out: the top left stays.
        let r = resized(orig, Handle::Se, 0.2, 0.).unwrap();
        assert!(rect_near(r, Rect { x: 0.2, y: 0.2, w: 0.6, h: 0.3 }));
        // The top left in, by the height: the bottom right stays, the shape too.
        let r = resized(orig, Handle::Nw, 0., 0.1).unwrap();
        assert!(rect_near(r, Rect { x: 0.4, y: 0.3, w: 0.2, h: 0.1 }));
        assert!(near(r.w / r.h, 2.));
        // Never smaller than the least there is, and an edge doesn't resize.
        assert!(resized(orig, Handle::Se, -1., -1.).unwrap().w >= MIN_SIZE);
        assert_eq!(resized(orig, Handle::E, 0.1, 0.), None);
        // Free resize stretches from any grip, and refuses to vanish.
        assert!(rect_near(stretched(orig, Handle::E, 0.1, 0.5).unwrap(), Rect { w: 0.5, ..orig }));
        assert!(rect_near(stretched(orig, Handle::N, 0., 0.05).unwrap(), Rect { y: 0.25, h: 0.15, ..orig }));
        assert_eq!(stretched(orig, Handle::W, 0.39, 0.), None);
    }

    #[test]
    fn a_crop_trims_in_place_and_keeps_some_of_the_source() {
        // A source 0.4 of the canvas wide: dragging its left edge in by 0.1 hides a quarter of it.
        let orig = Rect { x: 0.2, y: 0.2, w: 0.4, h: 0.2 };
        let (r, c) = cropped(orig, Crop::default(), Handle::W, 0.1, 0.);
        assert!(rect_near(r, Rect { x: 0.3, w: 0.3, ..orig }));
        assert!(near(c.l, 0.25) && c.r == 0. && c.t == 0.);
        // From an existing crop, at its scale: the right edge in by 0.15 of the canvas.
        let (r2, c2) = cropped(r, c, Handle::E, -0.15, 0.);
        assert!(near(c2.r, 0.375) && near(r2.w, 0.15) && near(r2.x, 0.3));
        // However far it's dragged, a twentieth of the source stays.
        let (r3, c3) = cropped(orig, Crop::default(), Handle::Se, -5., -5.);
        assert!(near(c3.r, 1. - MIN_VISIBLE) && near(c3.b, 1. - MIN_VISIBLE));
        assert!(r3.w > 0. && r3.h > 0.);
        // Dragged out, a crop gives back no more than was cropped.
        let (r4, c4) = cropped(r, c, Handle::W, -1., 0.);
        assert_eq!(c4.l, 0.);
        assert!(rect_near(r4, orig));
    }

    fn layout_with(sizes: &[(Source, (u32, u32))]) -> Layout {
        let mut l = Layout::new((1920, 1080));
        for (s, size) in sizes {
            let id = l.add(s.clone(), "x".into(), String::new());
            l.seen(id, *size);
        }
        l
    }

    #[test]
    fn new_sources_are_placed_once_their_size_is_known() {
        let l = layout_with(&[
            (Source::Screen(1), (2560, 1440)),
            (Source::Window(7), (1000, 1000)),
            (Source::Camera("cam".into()), (640, 360)),
        ]);
        // The first fills the canvas (the same shape), the window is half as wide, the camera in
        // the bottom right corner.
        assert!(rect_near(l.layers[0].rect, Rect { x: 0., y: 0., w: 1., h: 1. }));
        let window = l.layers[1].rect;
        assert!(near(window.w, 0.5) && near(window.h, 0.5 * 1920. / 1080.));
        let cam = l.layers[2].rect;
        assert!(near(cam.w, 0.25) && near(cam.x + cam.w, 0.98) && near(cam.y + cam.h, 0.97));
        // A saved place is kept.
        let mut saved = Layout::new((1920, 1080));
        let r = Rect { x: 0.1, y: 0.1, w: 0.2, h: 0.2 };
        let id = saved.add_at(Source::Screen(1), "x".into(), String::new(), r, Crop::default());
        saved.seen(id, (1920, 1080));
        assert_eq!(saved.layers[0].rect, r);
    }

    #[test]
    fn fit_fill_and_sizes_keep_the_shape() {
        let mut l = layout_with(&[(Source::Window(1), (1000, 1000))]);
        let id = l.layers[0].id;
        l.fit(id);
        // A square on a 16:9 canvas: full height, centred.
        let r = l.layers[0].rect;
        assert!(near(r.h, 1.) && near(r.w, 1080. / 1920.) && near(r.x, (1. - r.w) / 2.));
        l.fill(id);
        let (r, c) = (l.layers[0].rect, l.layers[0].crop);
        assert_eq!(r, Rect { x: 0., y: 0., w: 1., h: 1. });
        assert!(near(c.t, c.b) && c.l == 0. && near(1. - c.t - c.b, 1080. / 1920.));
        l.reset_crop(id);
        assert!(!l.layers[0].crop.any() && near(l.layers[0].rect.h, 1920. / 1080.));
        l.scale_to(id, 0.25);
        let r = l.layers[0].rect;
        assert!(near(r.w, 0.25) && near(r.h, 0.25 * 1920. / 1080.));
        l.original_size(id);
        let r = l.layers[0].rect;
        assert!(near(r.w, 1000. / 1920.) && near(r.h, 1000. / 1080.));
    }

    #[test]
    fn the_stack_reorders_and_hits_the_topmost() {
        let mut l = layout_with(&[(Source::Screen(1), (1920, 1080)), (Source::Window(2), (960, 540)), (Source::Window(3), (960, 540))]);
        let ids: Vec<u64> = l.layers.iter().map(|x| x.id).collect();
        let order = |l: &Layout| l.layers.iter().map(|x| x.id).collect::<Vec<_>>();
        // The two windows overlap at (0.25, 0.25); the screen is under everything.
        assert_eq!(l.hit(0.25, 0.25), Some(ids[2]));
        assert_eq!(l.hit(0.95, 0.05), Some(ids[0]));
        l.to_back(ids[2]);
        assert_eq!(order(&l), vec![ids[2], ids[0], ids[1]]);
        assert_eq!(l.hit(0.25, 0.25), Some(ids[1]));
        l.to_front(ids[2]);
        assert_eq!(order(&l), vec![ids[0], ids[1], ids[2]]);
        l.step(ids[2], false);
        assert_eq!(order(&l), vec![ids[0], ids[2], ids[1]]);
        l.step(ids[1], true);
        assert_eq!(order(&l), vec![ids[0], ids[2], ids[1]]);
        assert!(l.remove(ids[0]).is_some() && l.hit(0.95, 0.05).is_none());
    }

    #[test]
    fn a_layout_maps_to_even_pixels_on_the_canvas() {
        let full = Rect { x: 0., y: 0., w: 1., h: 1. };
        let p = placement(full, Crop::default(), (2560, 1440), (1920, 1080)).unwrap();
        assert_eq!(p, Placement { src: (0, 0, 2560, 1440), dst: (0, 0, 1920, 1080) });
        // Half off the left edge: only its right half shows, at the left of the canvas.
        let half = Rect { x: -0.25, y: 0., w: 0.5, h: 0.5 };
        let p = placement(half, Crop::default(), (1920, 1080), (1920, 1080)).unwrap();
        assert_eq!(p, Placement { src: (960, 0, 960, 1080), dst: (0, 0, 480, 540) });
        // Cropped: the source's right half, over the whole canvas.
        let p = placement(full, Crop { l: 0.5, ..Crop::default() }, (1920, 1080), (1920, 1080)).unwrap();
        assert_eq!(p, Placement { src: (960, 0, 960, 1080), dst: (0, 0, 1920, 1080) });
        // Odd sizes land on even pixels, inside both pictures.
        let odd = Rect { x: 0.1234, y: 0.3333, w: 0.377, h: 0.2111 };
        let p = placement(odd, Crop { l: 0.013, t: 0.1, r: 0.2, b: 0.05 }, (1366, 769), (1280, 720)).unwrap();
        let ((sx, sy, sw, sh), (dx, dy, dw, dh)) = (p.src, p.dst);
        assert!([sx, sy, dx, dy, dw, dh].iter().all(|v| v % 2 == 0), "{p:?}");
        assert!(sx + sw <= 1366 && sy + sh <= 769 && dx + dw <= 1280 && dy + dh <= 720, "{p:?}");
        // Wholly off the canvas, or too small to draw: nothing.
        assert_eq!(placement(Rect { x: 1.2, ..full }, Crop::default(), (100, 100), (1280, 720)), None);
        assert_eq!(placement(Rect { w: 0.0001, ..full }, Crop::default(), (100, 100), (1280, 720)), None);
    }

    fn filled(w: u32, h: u32, y: u8, u: u8, v: u8) -> I420Buffer {
        let mut b = I420Buffer::new(w, h);
        let (py, pu, pv) = b.data_mut();
        py.fill(y);
        pu.fill(u);
        pv.fill(v);
        b
    }

    #[test]
    fn sources_composite_bottom_first_into_the_canvas() {
        let mut canvas = I420Buffer::new(16, 8);
        clear(&mut canvas);
        blit(&mut canvas, &filled(8, 8, 100, 90, 160), (0, 0));
        // Over the first, reaching past the right edge: cut off there.
        blit(&mut canvas, &filled(8, 4, 200, 60, 70), (12, 2));
        let (sy, su, _) = canvas.strides();
        let (y, u, v) = canvas.data();
        let luma = |x: u32, r: u32| y[(r * sy + x) as usize];
        assert_eq!((luma(0, 0), luma(7, 7)), (100, 100));
        assert_eq!((luma(8, 0), luma(11, 3)), (16, 16), "black where nothing is");
        assert_eq!((luma(12, 2), luma(15, 5), luma(12, 6)), (200, 200, 16));
        assert_eq!((u[(su + 6) as usize], v[(su + 6) as usize]), (60, 70));
        assert_eq!((u[0], v[0], u[5]), (90, 160, 128));
    }

    #[test]
    fn the_canvas_draws_each_picture_where_its_source_is() {
        let mut layout = Layout::new((64, 32));
        let a = layout.add_at(Source::Screen(1), "a".into(), String::new(), Rect { x: 0., y: 0., w: 0.5, h: 1. }, Crop::default());
        let b = layout.add_at(Source::Window(2), "b".into(), String::new(), Rect { x: 0.25, y: 0.5, w: 0.5, h: 0.5 }, Crop::default());
        layout.seen(a, (32, 32));
        layout.seen(b, (32, 16));
        let picture = |id: u64, y: u8| {
            let at = placement(layout.layer(id).unwrap().rect, Crop::default(), layout.layer(id).unwrap().size.unwrap(), (64, 32)).unwrap();
            // The bottom one was made at another size: it's scaled to fit.
            let (w, h) = if id == a { (8, 8) } else { (at.dst.2, at.dst.3) };
            Feeding::Desktop { feed: Arc::new(Feed::default()), current: Some(filled(w, h, y, 128, 128)) }
        };
        let feeds = HashMap::from([(a, picture(a, 50)), (b, picture(b, 150))]);
        let mut canvas = I420Buffer::new(64, 32);
        compose(&mut canvas, &layout.layers, &feeds, &mut None);
        let (sy, _, _) = canvas.strides();
        let luma = |x: u32, r: u32| canvas.data().0[(r * sy + x) as usize];
        assert_eq!((luma(0, 0), luma(31, 15)), (50, 50));
        assert_eq!((luma(16, 16), luma(47, 31)), (150, 150), "the top one over it");
        assert_eq!((luma(40, 0), luma(63, 31)), (16, 16));
    }

    /// A composition of the first screen and a window that is gone, run for a moment, its frames
    /// read back off a track as a connection would read them. It needs a desktop to capture, so
    /// it runs only when asked: `cargo test -- --ignored a_running_composition`.
    #[test]
    #[ignore]
    fn a_running_composition_sends_the_canvas() {
        use futures::StreamExt;
        use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
        use libwebrtc::video_source::VideoResolution;
        use libwebrtc::video_stream::native::NativeVideoStream;
        let first = screen::sources(SourceKind::Screen).into_iter().next().expect("a screen to capture");
        let mut layout = Layout::new((1280, 720));
        layout.add(Source::Window(0xdead_beef), "gone".into(), String::new());
        layout.add(Source::Screen(first.id), first.title.clone(), String::new());
        let comp = Composition::new(layout, Background::None);
        let source = ScreenSource { id: 0, kind: SourceKind::Composite, title: "c".into(), size: Some((1280, 720)) };
        let choice = ScreenChoice {
            source,
            resolution: screen::Resolution::P720,
            fps: 30,
            sharp: true,
            sound: screen::Sound::Off,
            composition: Some(comp.clone()),
        };
        let _rt = crate::core::runtime().enter();
        let video = NativeVideoSource::new(VideoResolution { width: 1280, height: 720 }, true);
        let mut stream = NativeVideoStream::new(super::super::rtc::factory().create_video_track("composite-test", video.clone()));
        let (stop, preview) = (AtomicBool::new(false), FrameSlot::default());
        let (frames, lit) = std::thread::scope(|s| {
            s.spawn(|| run(choice, &mut || None, &video, &preview, &stop, &|| false));
            let read = crate::core::runtime().block_on(async {
                let (mut frames, mut lit) = (0, false);
                let until = tokio::time::Instant::now() + Duration::from_secs(3);
                while let Ok(Some(f)) = tokio::time::timeout_at(until, stream.next()).await {
                    assert_eq!((f.buffer.width(), f.buffer.height()), (1280, 720));
                    let i420 = f.buffer.to_i420();
                    lit |= i420.data().0.iter().any(|&y| y != 16);
                    frames += 1;
                }
                (frames, lit)
            });
            stop.store(true, Ordering::Relaxed);
            read
        });
        println!("{frames} frames in 3 s");
        assert!(frames >= 30, "{frames} frames");
        assert!(lit, "the screen is on the canvas");
        let layout = comp.layout();
        assert_eq!(layout.layers.len(), 1, "the window that is gone left the canvas");
        assert!(layout.layers[0].placed && layout.layers[0].size.is_some());
        assert!(!comp.gone(), "the share goes on with the screen");
    }

    /// The camera on a composition: opened by it while nobody else has it, then handed to the
    /// call's camera when that turns on, whose frames the composition reads instead. Needs a
    /// camera: `cargo test -- --ignored a_composition_shares_the_camera`.
    #[test]
    #[ignore]
    fn a_composition_shares_the_camera() {
        let Some(cam) = camera::devices().into_iter().next() else {
            println!("no camera here");
            return;
        };
        let mut layout = Layout::new((1280, 720));
        layout.add(Source::Camera(cam.id.clone()), cam.name.clone(), String::new());
        let comp = Composition::new(layout, Background::None);
        let source = ScreenSource { id: 0, kind: SourceKind::Composite, title: "c".into(), size: Some((1280, 720)) };
        let choice = ScreenChoice {
            source,
            resolution: screen::Resolution::P720,
            fps: 30,
            sharp: true,
            sound: screen::Sound::Off,
            composition: Some(comp.clone()),
        };
        let _rt = crate::core::runtime().enter();
        let video = NativeVideoSource::new(libwebrtc::video_source::VideoResolution { width: 1280, height: 720 }, true);
        let (stop, preview) = (AtomicBool::new(false), FrameSlot::default());
        let wait_for = |what: &str, f: &dyn Fn() -> bool| {
            let until = Instant::now() + Duration::from_secs(12);
            while !f() {
                assert!(Instant::now() < until, "{what}");
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        std::thread::scope(|s| {
            s.spawn(|| run(choice, &mut || None, &video, &preview, &stop, &|| false));
            wait_for("the composition opens the camera", &|| comp.layout().layers[0].placed);
            // The call's camera asks for the device: the composition lets go and reads its frames.
            let call = Capture::start(&cam.id, Background::None);
            let before = call.tap.serial();
            wait_for("the call's camera gets the device", &|| call.tap.serial() > before + 10);
            assert!(camera::borrow(&cam.id).is_ok_and(|t| Arc::ptr_eq(&t, &call.tap)));
            assert!(call.error.lock().is_none());
            call.stop();
            stop.store(true, Ordering::Relaxed);
        });
    }

    #[test]
    fn a_saved_layout_finds_its_sources_again() {
        let mut l = Layout::new((1920, 1080));
        let r = Rect { x: 0.5, y: 0.1, w: 0.4, h: 0.3 };
        l.add_at(Source::Window(99), "Notes".into(), "notepad.exe".into(), r, Crop { l: 0.1, ..Crop::default() });
        l.add(Source::Camera("Cam".into()), "Cam".into(), String::new());
        let saved = saved(&l);
        assert_eq!(saved.len(), 1, "only sources with a place");
        assert_eq!((saved[0].rect(), saved[0].crop().l), (r, 0.1));
        let screen = |id: u64, title: &str| ScreenSource { id, kind: SourceKind::Window, title: title.into(), size: None };
        // After a restart the window has another handle and maybe another title.
        let windows = vec![(screen(5, "Other"), "code.exe".to_string()), (screen(6, "Notes - 2"), "notepad.exe".to_string())];
        assert_eq!(find(&saved[0], &[], &windows, &[]).map(|f| f.0), Some(Source::Window(6)));
        let windows = vec![(screen(5, "Notes"), String::new())];
        assert_eq!(find(&saved[0], &[], &windows, &[]).map(|f| f.0), Some(Source::Window(5)));
        assert_eq!(find(&saved[0], &[], &[], &[]), None);
        let cam = CompositeLayer { kind: "camera".into(), id: "Cam".into(), ..Default::default() };
        let cameras = [camera::Device { id: "Cam".into(), name: "Cam".into() }];
        assert_eq!(find(&cam, &[], &[], &cameras).map(|f| f.0), Some(Source::Camera("Cam".into())));
        let screens = [ScreenSource { id: 3, kind: SourceKind::Screen, title: "Screen 2".into(), size: None }];
        let scr = CompositeLayer { kind: "screen".into(), id: "3".into(), ..Default::default() };
        assert_eq!(find(&scr, &screens, &[], &[]).map(|f| f.0), Some(Source::Screen(3)));
    }
}
