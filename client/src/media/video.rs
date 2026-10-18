//! Cameras and screens on the stage. Each is a tile: someone else's stream, watched over WHEP
//! with its sound in the mix (screens only), or your own, previewed from the frames being sent.
//! Frames stay YUV, as decoders give them, and the GPU converts and scales them as it draws. A
//! tile is its own view, woken by the frames themselves, so a new frame redraws that tile and
//! nothing that merely contains it. Whatever a frame needs before it can be drawn is done as it
//! is put in its slot, on the thread that made it, never on the UI thread.
//!
//! How a frame reaches the GPU depends on the renderer (see vendor/GPUI_VENDORED.md):
//! - Windows: the frame's planes as they are; the renderer uploads them into textures it keeps
//!   for each stream.
//! - macOS: a CoreVideo pixel buffer, which Metal reads where it lies (IOSurface). A hardware
//!   decoder's (VideoToolbox) is drawn as it comes, with no copy at all; any other frame is
//!   written into one of its stream's pool in a single pass, as NV12.
//! - Elsewhere: a BGRA picture through the window's sprite atlas, in buffers reused from frame to
//!   frame, each let go of the atlas when the next one replaces it.

use super::audio::{self, Source, SourceKind};
use super::rtc::Track;
use super::voice::{Voice, Watch};
use super::yuv;
use crate::core;
use crate::core::types::*;
use futures::StreamExt;
use gpui::{AnyElement, App, AppContext, Context, Entity, Pixels, SurfaceFormat, SurfaceFrame, SurfacePlane, Task};
use libwebrtc::audio_stream::native::NativeAudioStream;
use libwebrtc::prelude::*;
use libwebrtc::video_stream::native::NativeVideoStream;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TileKind {
    Camera,
    Screen,
}

impl TileKind {
    pub fn path(self) -> &'static str {
        match self {
            TileKind::Camera => "c",
            TileKind::Screen => "s",
        }
    }
}

/// A frame as the window's renderer draws it (see the module's notes).
#[derive(Clone)]
pub struct Shown {
    #[cfg(windows)]
    frame: SurfaceFrame,
    #[cfg(target_os = "macos")]
    buffer: mac::PixelBuffer,
    #[cfg(not(any(windows, target_os = "macos")))]
    image: Arc<gpui::RenderImage>,
    width: u32,
    height: u32,
}

impl Shown {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

/// The newest frame, handed from the decoding thread to the window.
pub struct FrameSlot {
    /// Names the stream to the renderer, which keeps its textures by it.
    pub id: u64,
    frame: Mutex<Option<Shown>>,
    fresh: AtomicBool,
    /// Wakes the tile showing this slot; one pending wake covers any number of frames.
    wake: Mutex<Option<async_channel::Sender<()>>>,
    /// Nobody can see this stream now (a text channel is open), so its frames aren't converted;
    /// one flag for every stream in the call.
    pub hidden: Arc<AtomicBool>,
    /// The size the tile is drawn at, in device pixels; frames much bigger are scaled down to it.
    pub want_w: AtomicU32,
    pub want_h: AtomicU32,
    /// What fed this slot died while its stream went on: the tile hangs up, so the stream is
    /// subscribed again rather than left frozen on its last frame.
    broken: AtomicBool,
    /// The stream's pixel buffer pool, and room for a mirrored picture's chroma.
    #[cfg(target_os = "macos")]
    maker: Mutex<mac::Maker>,
    /// Pictures the tile has let go of, whose buffers the next frames are written into once
    /// nothing else holds them.
    #[cfg(not(any(windows, target_os = "macos")))]
    retired: Mutex<Vec<Arc<gpui::RenderImage>>>,
}

impl Default for FrameSlot {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        FrameSlot {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            frame: Mutex::default(),
            fresh: AtomicBool::default(),
            wake: Mutex::default(),
            hidden: Arc::default(),
            want_w: AtomicU32::default(),
            want_h: AtomicU32::default(),
            broken: AtomicBool::default(),
            #[cfg(target_os = "macos")]
            maker: Mutex::default(),
            #[cfg(not(any(windows, target_os = "macos")))]
            retired: Mutex::default(),
        }
    }
}

/// Pictures a stream keeps for reuse once its tile let go of them (see `FrameSlot::retired`).
#[cfg(not(any(windows, target_os = "macos")))]
const RETIRED: usize = 4;

impl FrameSlot {
    /// Shows `frame` in the tile watching this slot, readied for the renderer here, on the
    /// caller's thread.
    pub fn put(&self, frame: SurfaceFrame) {
        #[cfg(windows)]
        let shown = Some(Shown { width: frame.width(), height: frame.height(), frame });
        #[cfg(not(windows))]
        let shown = self.make(planes_of(&frame), (frame.width(), frame.height()), frame.is_mirrored());
        if let Some(shown) = shown {
            self.show(shown);
        }
    }

    fn show(&self, shown: Shown) {
        // A frame replaced before its tile took it was never drawn.
        let unseen = self.frame.lock().replace(shown);
        self.fresh.store(true, Ordering::Release);
        self.wake();
        #[cfg(not(any(windows, target_os = "macos")))]
        if let Some(old) = unseen {
            self.recycle(old.image);
        }
        #[cfg(any(windows, target_os = "macos"))]
        drop(unseen);
    }

    fn wake(&self) {
        if let Some(wake) = &*self.wake.lock() {
            let _ = wake.try_send(());
        }
    }

    /// A tile shows this slot: frames put here are seen.
    pub fn watched(&self) -> bool {
        self.wake.lock().is_some()
    }

    /// The newest frame, if it wasn't taken yet. Taken, not copied: the slot holds no frame the
    /// window has, which on macOS would keep one more of a decoder's buffers from it.
    fn take_fresh(&self) -> Option<Shown> {
        self.fresh.swap(false, Ordering::Acquire).then(|| self.frame.lock().take()).flatten()
    }

    /// What feeds this slot failed; its tile hangs up (see `broken`).
    fn break_off(&self) {
        self.broken.store(true, Ordering::Release);
        self.wake();
    }

    /// A `w` × `h` frame from `planes` as a pixel buffer of this stream's pool.
    #[cfg(target_os = "macos")]
    fn make(&self, planes: yuv::Planes, size: (u32, u32), mirror: bool) -> Option<Shown> {
        let buffer = self.maker.lock().make(planes, size, mirror)?;
        Some(Shown { buffer, width: size.0, height: size.1 })
    }

    /// A `w` × `h` frame from `planes` as a BGRA picture, in a retired picture's buffer if one is
    /// free.
    #[cfg(not(any(windows, target_os = "macos")))]
    fn make(&self, planes: yuv::Planes, (w, h): (u32, u32), mirror: bool) -> Option<Shown> {
        let len = w as usize * h as usize * 4;
        let mut px = self.reuse(len).unwrap_or_else(|| vec![0; len]);
        yuv::to_bgra(planes, (w, h), mirror, &mut px);
        let img = image::RgbaImage::from_raw(w, h, px).expect("frame size");
        // Moved in, not `SmallVec::from_elem`, which would copy the picture.
        let frames: smallvec::SmallVec<[image::Frame; 1]> = smallvec::smallvec![image::Frame::new(img)];
        let image = Arc::new(gpui::RenderImage::new(frames));
        Some(Shown { image, width: w, height: h })
    }

    /// The buffer of a retired picture of `len` bytes that nothing holds any more.
    #[cfg(not(any(windows, target_os = "macos")))]
    fn reuse(&self, len: usize) -> Option<Vec<u8>> {
        let mut retired = self.retired.lock();
        let i = retired.iter().position(|p| Arc::strong_count(p) == 1 && p.as_bytes(0).is_some_and(|b| b.len() == len))?;
        let image = Arc::try_unwrap(retired.swap_remove(i)).ok()?;
        drop(retired);
        Some(image.into_frames().into_iter().next()?.into_buffer().into_raw())
    }

    /// Keeps `image`'s buffer for a later frame; the oldest kept goes when there are too many.
    #[cfg(not(any(windows, target_os = "macos")))]
    fn recycle(&self, image: Arc<gpui::RenderImage>) {
        let mut retired = self.retired.lock();
        retired.push(image);
        if retired.len() > RETIRED {
            retired.remove(0);
        }
    }
}

/// A frame's planes, for converting it.
#[cfg(not(windows))]
fn planes_of(frame: &SurfaceFrame) -> yuv::Planes<'_> {
    match frame.format() {
        SurfaceFormat::I420 => yuv::Planes::I420 { y: frame.plane(0), u: frame.plane(1), v: frame.plane(2) },
        SurfaceFormat::Nv12 => yuv::Planes::Nv12 { y: frame.plane(0), uv: frame.plane(1) },
    }
}

static WINDOW_HIDDEN: AtomicBool = AtomicBool::new(false);

/// The window is minimised or hidden to the tray: nobody can see any stream or preview.
pub fn set_window_hidden(hidden: bool) {
    WINDOW_HIDDEN.store(hidden, Ordering::Relaxed);
}

pub fn window_hidden() -> bool {
    WINDOW_HIDDEN.load(Ordering::Relaxed)
}

/// Frames a stream can have out at once (in its slot, its tile and the window's last two scenes)
/// whose buffers are kept for reuse; any more are allocated and let go.
const POOL: usize = 6;

/// What one stream keeps between frames: the buffers of frames the window is done with, so a new
/// frame doesn't allocate.
#[derive(Default)]
pub struct Scratch {
    pool: Vec<Arc<[u8]>>,
}

impl Scratch {
    /// `len` bytes written by `fill`, in one of the pool's buffers that nothing holds any more if
    /// there is one.
    fn buffer(&mut self, len: usize, fill: impl FnOnce(&mut [u8])) -> Arc<[u8]> {
        self.pool.retain(|b| b.len() == len);
        if let Some(i) = self.pool.iter_mut().position(|b| Arc::get_mut(b).is_some()) {
            fill(Arc::get_mut(&mut self.pool[i]).expect("unshared"));
            return self.pool[i].clone();
        }
        let mut b: Arc<[u8]> = std::iter::repeat_n(0, len).collect();
        fill(Arc::get_mut(&mut b).expect("new"));
        if self.pool.len() < POOL {
            self.pool.push(b.clone());
        }
        b
    }

    /// A `w` × `h` I420 frame of stream `id` in one buffer, its Y, U and V planes written by `fill`.
    pub fn i420(&mut self, id: u64, w: u32, h: u32, fill: impl FnOnce(&mut [u8], &mut [u8], &mut [u8])) -> SurfaceFrame {
        let data = self.buffer(yuv::i420_len(w, h), |buf| {
            let (y, u, v) = yuv::i420_planes(buf, w, h);
            fill(y, u, v)
        });
        let planes = yuv::i420_layout(w, h).map(|(offset, stride)| SurfacePlane { offset, stride });
        SurfaceFrame::new(id, SurfaceFormat::I420, w, h, data, &planes)
    }

    /// A frame of stream `id` from planes (bytes, stride) copied as they lie, one after another.
    fn copied(&mut self, id: u64, format: SurfaceFormat, w: u32, h: u32, planes: &[(&[u8], u32)]) -> SurfaceFrame {
        let mut layout = Vec::with_capacity(planes.len());
        let mut len = 0;
        for (bytes, stride) in planes {
            layout.push(SurfacePlane { offset: len, stride: *stride as usize });
            len += bytes.len();
        }
        let data = self.buffer(len, |buf| {
            for ((bytes, _), at) in planes.iter().zip(&layout) {
                buf[at.offset..at.offset + bytes.len()].copy_from_slice(bytes);
            }
        });
        SurfaceFrame::new(id, format, w, h, data, &layout)
    }

    /// An I420 picture as a frame of stream `id`.
    pub fn of_i420(&mut self, id: u64, b: &I420Buffer) -> SurfaceFrame {
        let (y, u, v) = b.data();
        let (sy, su, sv) = b.strides();
        self.copied(id, SurfaceFormat::I420, b.width(), b.height(), &[(y, sy), (u, su), (v, sv)])
    }
}

/// The size to scale a `sw` × `sh` frame down to before it is uploaded, when it has more than half
/// again the pixels of what fits in `max`: they would cost upload bandwidth only for the GPU to
/// throw them away. Smaller ones are uploaded whole and scaled by the GPU as it draws.
fn shrink_to(sw: u32, sh: u32, (mw, mh): (u32, u32)) -> Option<(u32, u32)> {
    if mw == 0 || mh == 0 || sw == 0 || sh == 0 {
        return None;
    }
    let s = (mw as f64 / sw as f64).min(mh as f64 / sh as f64);
    let side = |n: u32| ((n as f64 * s).round() as u32).max(2) & !1;
    (s * s * 1.5 < 1.).then(|| (side(sw), side(sh)))
}

/// A decoded frame as a frame of stream `id` for the window: its planes as the decoder left
/// them, or scaled down on the CPU when it is much bigger than `max` (see `shrink_to`).
pub fn surface_frame(buffer: &dyn VideoBuffer, id: u64, max: (u32, u32), scratch: &mut Scratch) -> SurfaceFrame {
    let (sw, sh) = (buffer.width(), buffer.height());
    if let Some((w, h)) = shrink_to(sw, sh, max) {
        // Decoders hand over I420, which is scaled from where it lies; `to_i420` would copy it first.
        let converted;
        let src = match buffer.as_i420() {
            Some(b) => b,
            None => {
                converted = buffer.to_i420();
                &converted
            }
        };
        return scratch.i420(id, w, h, |y, u, v| yuv::scale_into(src, y, u, v, w, h));
    }
    if let Some(b) = buffer.as_i420() {
        return scratch.of_i420(id, b);
    }
    if let Some(b) = buffer.as_nv12() {
        let (y, uv) = b.data();
        let (sy, suv) = b.strides();
        return scratch.copied(id, SurfaceFormat::Nv12, sw, sh, &[(y, sy), (uv, suv)]);
    }
    scratch.of_i420(id, &buffer.to_i420())
}

/// A decoded frame readied for `slot`'s tile, where the renderer can't take YUV planes as they
/// are: written once into what it draws, from the planes where the decoder left them (or scaled
/// down first, through `scratch`, when much bigger than `max`). On macOS a hardware decoder's
/// pixel buffer is drawn as it is.
#[cfg(not(windows))]
fn shown(buffer: &dyn VideoBuffer, slot: &FrameSlot, max: (u32, u32), scratch: &mut Scratch) -> Option<Shown> {
    #[cfg(target_os = "macos")]
    if let Some(native) = buffer.as_native()
        && let Some(buffer) = mac::PixelBuffer::of_decoder(native.get_cv_pixel_buffer())
    {
        let (width, height) = buffer.size();
        return Some(Shown { buffer, width, height });
    }
    let size = (buffer.width(), buffer.height());
    if shrink_to(size.0, size.1, max).is_some() {
        let small = surface_frame(buffer, slot.id, max, scratch);
        return slot.make(planes_of(&small), (small.width(), small.height()), false);
    }
    if let Some(b) = buffer.as_i420() {
        return slot.make(yuv::Planes::of_i420(b), size, false);
    }
    if let Some(b) = buffer.as_nv12() {
        return slot.make(yuv::Planes::of_nv12(b), size, false);
    }
    slot.make(yuv::Planes::of_i420(&buffer.to_i420()), size, false)
}

/// Marks a slot broken if the pump feeding it unwinds. A pump that is stopped (aborted) is
/// dropped without unwinding, and leaves it be.
struct Feeding<'a>(&'a FrameSlot);

impl Drop for Feeding<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.break_off();
        }
    }
}

/// Starts feeding a watched track into a tile: video frames into its slot, sound into its source.
pub fn pump(track: Track, slot: &Arc<FrameSlot>, sound: Option<&Arc<Source>>) -> Option<tokio::task::JoinHandle<()>> {
    match track {
        Track::Video(v) => {
            let slot = slot.clone();
            Some(core::runtime().spawn(async move {
                let _feeding = Feeding(&slot);
                let mut stream = NativeVideoStream::new(v);
                let mut scratch = Scratch::default();
                while let Some(frame) = stream.next().await {
                    if slot.hidden.load(Ordering::Relaxed) || window_hidden() {
                        continue;
                    }
                    let want = (slot.want_w.load(Ordering::Relaxed), slot.want_h.load(Ordering::Relaxed));
                    let (into, mut kept) = (slot.clone(), std::mem::take(&mut scratch));
                    // Off the network runtime's workers. The stream holds only the newest frame
                    // meanwhile, so a slow copy or scale skips frames rather than queueing them.
                    let converted = tokio::task::spawn_blocking(move || {
                        #[cfg(windows)]
                        into.put(surface_frame(frame.buffer.as_ref(), into.id, want, &mut kept));
                        #[cfg(not(windows))]
                        if let Some(shown) = shown(frame.buffer.as_ref(), &into, want, &mut kept) {
                            into.show(shown);
                        }
                        kept
                    });
                    match converted.await {
                        Ok(s) => scratch = s,
                        Err(e) => {
                            // Packets still arrive, so the stall check would never notice.
                            log::warn!("video frame conversion failed: {e}; subscribing again");
                            slot.break_off();
                            break;
                        }
                    }
                }
            }))
        }
        Track::Audio(a) => {
            let src = sound?.clone();
            Some(core::runtime().spawn(async move {
                let mut stream = NativeAudioStream::new(a, audio::RATE as i32, 2);
                while let Some(frame) = stream.next().await {
                    src.push_i16(&frame.data, frame.num_channels as usize);
                }
            }))
        }
    }
}

pub struct Tile {
    pub user: UserId,
    pub mid: i64,
    pub kind: TileKind,
    pub local: bool,
    pub slot: Arc<FrameSlot>,
    /// What is on screen now.
    pub frame: Option<Shown>,
    /// The screen's sound, for someone else's screen.
    pub sound: Option<Arc<Source>>,
    /// Someone else's stream: the subscription, which `Voice` keeps alive.
    pub watch: Watch,
    /// Several attempts in a row have failed (it keeps trying).
    pub failed: bool,
    _redraw: Task<()>,
}

impl Tile {
    fn new(user: UserId, mid: i64, kind: TileKind, local: bool, slot: Arc<FrameSlot>, cx: &mut Context<Self>) -> Tile {
        let (wake, woken) = async_channel::bounded(1);
        *slot.wake.lock() = Some(wake);
        let redraw = cx.spawn(async move |this, cx| {
            loop {
                let Ok(()) = this.update(cx, |t, cx| {
                    if t.slot.broken.swap(false, Ordering::Acquire) {
                        log::info!("{} from slot {}: its video stopped; subscribing again", t.kind.path(), t.mid);
                        t.watch.hang_up();
                        t.watch.backoff.fail(Instant::now());
                        cx.notify();
                    }
                    if let Some(frame) = t.slot.take_fresh() {
                        let old = t.frame.replace(frame);
                        #[cfg(not(any(windows, target_os = "macos")))]
                        if let Some(old) = old {
                            retire(&t.slot, old, cx);
                        }
                        #[cfg(any(windows, target_os = "macos"))]
                        drop(old);
                        cx.notify();
                    }
                }) else {
                    break;
                };
                if woken.recv().await.is_err() {
                    break;
                }
            }
        });
        // A tile dropped without being closed lets go of its picture too.
        #[cfg(not(any(windows, target_os = "macos")))]
        cx.on_release(|t: &mut Tile, cx| {
            if let Some(old) = t.frame.take() {
                retire(&t.slot, old, cx);
            }
        })
        .detach();
        Tile { user, mid, kind, local, slot, frame: None, sound: None, watch: Watch::default(), failed: false, _redraw: redraw }
    }

    /// A tile showing your own camera or screen, from the frames being published.
    pub fn local(user: UserId, mid: i64, kind: TileKind, slot: Arc<FrameSlot>, cx: &mut App) -> Entity<Tile> {
        cx.new(|cx| Tile::new(user, mid, kind, true, slot, cx))
    }

    /// Someone else's camera or screen, not yet connected: `Voice` subscribes and retries.
    pub fn remote(user: UserId, mid: i64, kind: TileKind, hidden: Arc<AtomicBool>, cx: &mut App) -> Entity<Tile> {
        cx.new(|cx| {
            let mut tile = Tile::new(user, mid, kind, false, Arc::new(FrameSlot { hidden, ..Default::default() }), cx);
            tile.sound = (kind == TileKind::Screen).then(|| {
                let s = Source::new(SourceKind::Stream, 1., false);
                audio::audio().mixer.add(s.clone());
                s
            });
            tile
        })
    }

    pub fn set_gain(&mut self, gain: f32, cx: &mut Context<Self>) {
        if let Some(s) = &self.sound {
            s.gain.set(gain.clamp(0., audio::MAX_GAIN));
        }
        cx.notify();
    }

    pub fn gain(&self) -> f32 {
        self.sound.as_ref().map(|s| s.gain.get()).unwrap_or(1.)
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.watch.hang_up();
        if let Some(s) = self.sound.take() {
            audio::audio().mixer.remove(&s);
        }
        let old = self.frame.take();
        #[cfg(not(any(windows, target_os = "macos")))]
        if let Some(old) = old {
            retire(&self.slot, old, cx);
        }
        #[cfg(any(windows, target_os = "macos"))]
        drop(old);
        *self.slot.wake.lock() = None;
        self._redraw = Task::ready(());
        cx.notify();
    }
}

impl Tile {
    /// The frame on screen now, filling the space it is given with its shape kept and its corners
    /// rounded by `radius`.
    pub fn picture(&self, radius: Pixels) -> Option<AnyElement> {
        #[cfg(not(any(windows, target_os = "macos")))]
        use gpui::StyledImage;
        use gpui::{ObjectFit, Styled};
        let shown = self.frame.as_ref()?;
        #[cfg(windows)]
        let el = gpui::surface(shown.frame.clone());
        #[cfg(target_os = "macos")]
        let el = gpui::surface(shown.buffer.source());
        #[cfg(not(any(windows, target_os = "macos")))]
        let el = gpui::img(shown.image.clone());
        Some(gpui::IntoElement::into_any_element(el.size_full().rounded(radius).object_fit(ObjectFit::Contain)))
    }
}

/// Lets go of a picture the tile no longer shows: out of every window's atlas (after the current
/// update, when no window is lent out and so skipped) and its buffer back to the slot for reuse.
#[cfg(not(any(windows, target_os = "macos")))]
fn retire(slot: &FrameSlot, old: Shown, cx: &mut App) {
    let image = old.image.clone();
    cx.defer(move |cx| cx.drop_image(image, None));
    slot.recycle(old.image);
}

/// Tiles the person closed, by (user, kind); they stay closed until that stream ends.
#[derive(Default)]
pub struct Closed(pub HashSet<(UserId, TileKind)>);

/// Opens a tile for every camera and screen others publish, and closes the ones that ended.
pub fn sync_tiles(voice: &mut Voice, roster: &[Member], me: UserId, cx: &mut Context<Voice>) {
    let live: Vec<(UserId, i64, TileKind)> = roster
        .iter()
        .filter(|m| m.user_id != me)
        .flat_map(|m| {
            let mut v = Vec::new();
            if m.publishes("c") {
                v.push((m.user_id, m.mid, TileKind::Camera));
            }
            if m.publishes("s") {
                v.push((m.user_id, m.mid, TileKind::Screen));
            }
            v
        })
        .collect();
    let mut keep = Vec::new();
    for tile in voice.tiles.drain(..) {
        let (user, mid, kind, local) = {
            let t = tile.read(cx);
            (t.user, t.mid, t.kind, t.local)
        };
        if local || live.iter().any(|(u, m, k)| *u == user && *m == mid && *k == kind) {
            keep.push(tile);
        } else {
            tile.update(cx, |t, cx| t.close(cx));
        }
    }
    voice.tiles = keep;
    let closed: HashSet<(UserId, TileKind)> = voice.closed.0.clone();
    voice.closed.0.retain(|(u, k)| live.iter().any(|(lu, _, lk)| lu == u && lk == k));
    for (user, mid, kind) in live {
        let exists = voice.tiles.iter().any(|t| {
            let t = t.read(cx);
            t.user == user && t.kind == kind && !t.local
        });
        if exists || closed.contains(&(user, kind)) {
            continue;
        }
        voice.tiles.push(Tile::remote(user, mid, kind, voice.streams_hidden.clone(), cx));
    }
}

/// CoreVideo pixel buffers, which Metal reads where they lie: declared here, as the parts used are
/// few, plain C and in frameworks GPUI links already.
#[cfg(target_os = "macos")]
mod mac {
    use super::yuv;
    use std::ffi::c_void;
    use std::ptr::{self, NonNull};

    type CFTypeRef = *const c_void;
    type CVReturn = i32;

    /// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`: NV12, luma 16-235, what the frames the
    /// client makes are (BT.601, as libyuv converts).
    const VIDEO_RANGE: u32 = u32::from_be_bytes(*b"420v");
    /// `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`: NV12, luma 0-255, what VideoToolbox
    /// decodes into for WebRTC.
    const FULL_RANGE: u32 = u32::from_be_bytes(*b"420f");
    /// `kCFNumberSInt32Type`.
    const SINT32: isize = 3;

    /// A CoreFoundation struct only ever passed by address.
    #[repr(C)]
    struct Opaque {
        _private: [u8; 0],
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFTypeDictionaryKeyCallBacks: Opaque;
        static kCFTypeDictionaryValueCallBacks: Opaque;
        static kCFBooleanTrue: CFTypeRef;
        fn CFDictionaryCreate(
            allocator: CFTypeRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            count: isize,
            key_callbacks: *const Opaque,
            value_callbacks: *const Opaque,
        ) -> CFTypeRef;
        fn CFNumberCreate(allocator: CFTypeRef, kind: isize, value: *const c_void) -> CFTypeRef;
        fn CFRelease(cf: CFTypeRef);
    }

    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        static kCVPixelBufferPixelFormatTypeKey: CFTypeRef;
        static kCVPixelBufferWidthKey: CFTypeRef;
        static kCVPixelBufferHeightKey: CFTypeRef;
        static kCVPixelBufferIOSurfacePropertiesKey: CFTypeRef;
        static kCVPixelBufferMetalCompatibilityKey: CFTypeRef;
        fn CVPixelBufferPoolCreate(
            allocator: CFTypeRef,
            pool_attributes: CFTypeRef,
            buffer_attributes: CFTypeRef,
            pool: *mut *mut c_void,
        ) -> CVReturn;
        fn CVPixelBufferPoolCreatePixelBuffer(allocator: CFTypeRef, pool: *mut c_void, buffer: *mut *mut c_void) -> CVReturn;
        fn CVPixelBufferPoolRelease(pool: *mut c_void);
        fn CVPixelBufferRetain(buffer: *mut c_void) -> *mut c_void;
        fn CVPixelBufferRelease(buffer: *mut c_void);
        fn CVPixelBufferGetWidth(buffer: *mut c_void) -> usize;
        fn CVPixelBufferGetHeight(buffer: *mut c_void) -> usize;
        fn CVPixelBufferGetPixelFormatType(buffer: *mut c_void) -> u32;
        fn CVPixelBufferLockBaseAddress(buffer: *mut c_void, flags: u64) -> CVReturn;
        fn CVPixelBufferUnlockBaseAddress(buffer: *mut c_void, flags: u64) -> CVReturn;
        fn CVPixelBufferGetBaseAddressOfPlane(buffer: *mut c_void, plane: usize) -> *mut c_void;
        fn CVPixelBufferGetBytesPerRowOfPlane(buffer: *mut c_void, plane: usize) -> usize;
        fn CVPixelBufferGetHeightOfPlane(buffer: *mut c_void, plane: usize) -> usize;
    }

    /// One reference to a bi-planar 4:2:0 pixel buffer, let go of when dropped.
    pub struct PixelBuffer(NonNull<c_void>);

    // SAFETY: CoreVideo's retain and release may be called from any thread, and a buffer is no
    // longer written once it is shared.
    unsafe impl Send for PixelBuffer {}
    unsafe impl Sync for PixelBuffer {}

    impl Clone for PixelBuffer {
        fn clone(&self) -> Self {
            // SAFETY: the buffer is alive while `self` holds it.
            unsafe { CVPixelBufferRetain(self.0.as_ptr()) };
            PixelBuffer(self.0)
        }
    }

    impl Drop for PixelBuffer {
        fn drop(&mut self) {
            // SAFETY: `self` holds one reference.
            unsafe { CVPixelBufferRelease(self.0.as_ptr()) }
        }
    }

    impl PixelBuffer {
        /// A decoder's buffer (`CVPixelBufferRef`, or null when the frame has none), held as long
        /// as the window draws it, when the renderer can draw it as it is.
        pub fn of_decoder(raw: *mut c_void) -> Option<PixelBuffer> {
            let raw = NonNull::new(raw)?;
            // SAFETY: a non-null pointer from `NativeBuffer::get_cv_pixel_buffer` is a pixel
            // buffer the frame holds, alive for this call.
            let format = unsafe { CVPixelBufferGetPixelFormatType(raw.as_ptr()) };
            if format != FULL_RANGE && format != VIDEO_RANGE {
                return None;
            }
            // SAFETY: as above; the new reference is the one `PixelBuffer` lets go of.
            unsafe { CVPixelBufferRetain(raw.as_ptr()) };
            Some(PixelBuffer(raw))
        }

        pub fn size(&self) -> (u32, u32) {
            // SAFETY: the buffer is alive while `self` holds it.
            unsafe { (CVPixelBufferGetWidth(self.0.as_ptr()) as u32, CVPixelBufferGetHeight(self.0.as_ptr()) as u32) }
        }

        /// The buffer for a `surface` element, which holds a reference of its own while the
        /// window draws it.
        pub fn source(&self) -> gpui::SurfaceSource {
            // SAFETY: the buffer is alive while `self` holds it, and the reference taken here is
            // handed over to the source.
            unsafe { gpui::SurfaceSource::from_cv_pixel_buffer(CVPixelBufferRetain(self.0.as_ptr())) }
        }

        /// Writes the Y and UV planes, each with its stride, with the buffer locked; false if it
        /// could not be.
        fn write(&self, fill: impl FnOnce(&mut [u8], usize, &mut [u8], usize)) -> bool {
            let raw = self.0.as_ptr();
            // SAFETY: the buffer is alive and not yet shared; once locked, each plane's base
            // address holds its height times its bytes per row until it is unlocked.
            unsafe {
                if CVPixelBufferLockBaseAddress(raw, 0) != 0 {
                    return false;
                }
                let plane = |i: usize| {
                    let base = CVPixelBufferGetBaseAddressOfPlane(raw, i).cast::<u8>();
                    let stride = CVPixelBufferGetBytesPerRowOfPlane(raw, i);
                    (base, stride, stride * CVPixelBufferGetHeightOfPlane(raw, i))
                };
                let ((y, sy, ly), (uv, suv, luv)) = (plane(0), plane(1));
                let written = !y.is_null() && !uv.is_null();
                if written {
                    fill(std::slice::from_raw_parts_mut(y, ly), sy, std::slice::from_raw_parts_mut(uv, luv), suv);
                }
                CVPixelBufferUnlockBaseAddress(raw, 0);
                written
            }
        }
    }

    /// Video-range NV12 pixel buffers of one size, IOSurface-backed so Metal reads them without a
    /// copy; a buffer goes back to the pool when its last reference does.
    struct Pool {
        raw: NonNull<c_void>,
        size: (u32, u32),
    }

    // SAFETY: a pool may be used from any thread; `Maker`'s lock keeps it to one at a time.
    unsafe impl Send for Pool {}

    impl Drop for Pool {
        fn drop(&mut self) {
            // SAFETY: `self` holds the reference the pool was created with. Buffers still out
            // keep what they need.
            unsafe { CVPixelBufferPoolRelease(self.raw.as_ptr()) }
        }
    }

    impl Pool {
        fn new((w, h): (u32, u32)) -> Result<Pool, CVReturn> {
            // SAFETY: every object created here is released here once the pool holds what it
            // needs; the key and callback statics are CoreFoundation's and CoreVideo's own.
            unsafe {
                let number = |n: i32| CFNumberCreate(ptr::null(), SINT32, (&raw const n).cast());
                let dictionary = |keys: &[CFTypeRef], values: &[CFTypeRef]| {
                    CFDictionaryCreate(
                        ptr::null(),
                        keys.as_ptr(),
                        values.as_ptr(),
                        keys.len() as isize,
                        &raw const kCFTypeDictionaryKeyCallBacks,
                        &raw const kCFTypeDictionaryValueCallBacks,
                    )
                };
                let (format, width, height, surface) =
                    (number(VIDEO_RANGE as i32), number(w as i32), number(h as i32), dictionary(&[], &[]));
                let attributes = dictionary(
                    &[
                        kCVPixelBufferPixelFormatTypeKey,
                        kCVPixelBufferWidthKey,
                        kCVPixelBufferHeightKey,
                        kCVPixelBufferIOSurfacePropertiesKey,
                        kCVPixelBufferMetalCompatibilityKey,
                    ],
                    &[format, width, height, surface, kCFBooleanTrue],
                );
                let mut pool = ptr::null_mut();
                let status = CVPixelBufferPoolCreate(ptr::null(), ptr::null(), attributes, &mut pool);
                for object in [format, width, height, surface, attributes] {
                    CFRelease(object);
                }
                match NonNull::new(pool) {
                    Some(raw) if status == 0 => Ok(Pool { raw, size: (w, h) }),
                    _ => Err(status),
                }
            }
        }

        fn buffer(&self) -> Result<PixelBuffer, CVReturn> {
            let mut buffer = ptr::null_mut();
            // SAFETY: the pool is alive; the buffer comes with the reference `PixelBuffer` holds.
            let status = unsafe { CVPixelBufferPoolCreatePixelBuffer(ptr::null(), self.raw.as_ptr(), &mut buffer) };
            match NonNull::new(buffer) {
                Some(raw) if status == 0 => Ok(PixelBuffer(raw)),
                _ => Err(status),
            }
        }
    }

    /// Turns one stream's frames into pixel buffers: its pool, made again when the size changes,
    /// and room for a mirrored picture's chroma.
    #[derive(Default)]
    pub struct Maker {
        pool: Option<Pool>,
        chroma: Vec<u8>,
        /// A failure was logged; the stream's frames are dropped without saying so again.
        failed: bool,
    }

    impl Maker {
        /// A `w` × `h` picture from `planes` in one of the pool's buffers, in one pass, flipped
        /// left to right when `mirror`.
        pub fn make(&mut self, planes: yuv::Planes, size: (u32, u32), mirror: bool) -> Option<PixelBuffer> {
            if self.pool.as_ref().is_none_or(|p| p.size != size) {
                self.pool = None;
                self.pool = self.log(Pool::new(size), "buffer pool");
            }
            let buffer = self.pool.as_ref().map(Pool::buffer)?;
            let buffer = self.log(buffer, "buffer")?;
            let chroma = &mut self.chroma;
            buffer.write(|y, sy, uv, suv| yuv::to_nv12(planes, size, mirror, y, sy, uv, suv, chroma)).then_some(buffer)
        }

        fn log<T>(&mut self, made: Result<T, CVReturn>, what: &str) -> Option<T> {
            made.inspect_err(|status| {
                if !std::mem::replace(&mut self.failed, true) {
                    log::warn!("no pixel {what} for video frames: CVReturn {status}");
                }
            })
            .ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_frames_much_bigger_than_drawn_are_scaled_on_the_cpu() {
        // Four times the pixels of the tile: scaled to fit it.
        assert_eq!(shrink_to(1920, 1080, (960, 540)), Some((960, 540)));
        // Letterboxed in a square tile: fitted to its width.
        assert_eq!(shrink_to(1920, 1080, (640, 640)), Some((640, 360)));
        // Under half again as many pixels, or fewer: uploaded whole, for the GPU to scale.
        assert_eq!(shrink_to(1920, 1080, (1600, 900)), None);
        assert_eq!(shrink_to(1280, 720, (1920, 1080)), None);
        // A tile not laid out yet asks for nothing.
        assert_eq!(shrink_to(1920, 1080, (0, 0)), None);
    }

    /// Whether every pixel of one of the frame's planes is `value`.
    fn plane_is(frame: &SurfaceFrame, plane: usize, value: u8) -> bool {
        let (bytes, stride) = frame.plane(plane);
        let (w, rows) = frame.plane_size(plane);
        (0..rows).all(|r| bytes[r * stride..r * stride + w].iter().all(|&p| p == value))
    }

    #[test]
    fn frames_keep_their_planes_and_reuse_buffers_the_window_let_go() {
        let mut src = I420Buffer::new(64, 36);
        let (y, u, v) = src.data_mut();
        y.fill(100);
        u.fill(90);
        v.fill(160);
        let mut scratch = Scratch::default();
        let a = surface_frame(&src, 3, (64, 36), &mut scratch);
        assert_eq!((a.id(), a.width(), a.height(), a.format()), (3, 64, 36, SurfaceFormat::I420));
        assert!(plane_is(&a, 0, 100) && plane_is(&a, 1, 90) && plane_is(&a, 2, 160));
        let first = a.plane(0).0.as_ptr();
        drop(a);
        let b = surface_frame(&src, 3, (64, 36), &mut scratch);
        assert_eq!(b.plane(0).0.as_ptr(), first, "the first frame's buffer, free again");
        let c = surface_frame(&src, 3, (64, 36), &mut scratch);
        assert_ne!(c.plane(0).0.as_ptr(), first, "still held by the second frame");
        assert_ne!(b.serial(), c.serial());

        // Drawn at a quarter of the pixels: scaled down first, colours kept.
        let d = surface_frame(&src, 3, (32, 18), &mut scratch);
        assert_eq!((d.width(), d.height()), (32, 18));
        assert!(plane_is(&d, 0, 100) && plane_is(&d, 1, 90) && plane_is(&d, 2, 160));
    }

    fn grey(w: u32, h: u32, scratch: &mut Scratch) -> SurfaceFrame {
        scratch.i420(1, w, h, |y, u, v| {
            y.fill(100);
            u.fill(128);
            v.fill(128);
        })
    }

    #[test]
    fn a_slot_hands_over_only_its_newest_frame_and_keeps_none() {
        let (slot, mut scratch) = (FrameSlot::default(), Scratch::default());
        let (wake, woken) = async_channel::bounded(1);
        *slot.wake.lock() = Some(wake);
        slot.put(grey(64, 36, &mut scratch));
        slot.put(grey(32, 18, &mut scratch));
        assert!(woken.try_recv().is_ok(), "one wake for both");
        let shown = slot.take_fresh().expect("a frame");
        assert_eq!((shown.width(), shown.height()), (32, 18), "the newest");
        assert!(slot.take_fresh().is_none(), "taken once");
        assert!(slot.frame.lock().is_none(), "the slot keeps no frame the tile has");
    }

    #[test]
    fn a_pump_that_unwinds_breaks_its_slot_and_a_stopped_one_does_not() {
        let slot = FrameSlot::default();
        let (wake, woken) = async_channel::bounded(1);
        *slot.wake.lock() = Some(wake);
        drop(Feeding(&slot));
        assert!(!slot.broken.load(Ordering::Acquire) && woken.try_recv().is_err(), "stopped");
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _feeding = Feeding(&slot);
            panic!("a bad frame");
        }));
        assert!(unwound.is_err());
        assert!(slot.broken.load(Ordering::Acquire) && woken.try_recv().is_ok(), "broken, and the tile woken to see it");
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn pictures_are_written_into_buffers_the_window_let_go() {
        let (slot, mut scratch) = (FrameSlot::default(), Scratch::default());
        slot.put(grey(64, 36, &mut scratch));
        let first = slot.take_fresh().expect("a frame");
        let at = first.image.as_bytes(0).expect("pixels").as_ptr();
        slot.recycle(first.image.clone());
        slot.put(grey(64, 36, &mut scratch));
        let second = slot.take_fresh().expect("a frame");
        assert_ne!(second.image.as_bytes(0).unwrap().as_ptr(), at, "still held by the tile");
        drop(first);
        slot.recycle(second.image.clone());
        drop(second);
        slot.put(grey(64, 36, &mut scratch));
        let third = slot.take_fresh().expect("a frame");
        assert_eq!(third.image.as_bytes(0).unwrap().as_ptr(), at, "the first picture's buffer, free again");
        assert!(third.image.as_bytes(0).unwrap().chunks_exact(4).all(|p| p[3] == 255));
    }
}
