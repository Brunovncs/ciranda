//! Drawing on a screen share. Someone watching turns the pen on and draws over the picture; each
//! stroke goes, a few points at a time, through the server to everyone else in the call. They see
//! it over the same share, and the one sharing sees it over their own screen, in a window that
//! lets every click through (`ScreenInk`). Nothing is kept: a stroke fades a few seconds after it
//! is finished, so drawings don't pile up over somebody's game.
//!
//! Points travel as ten-thousandths of the picture, so a stroke lands in the same place however
//! big each person watches it.

use crate::core::realtime::RequestError;
use crate::core::types::{ErrorCode, Place, UserId};
use crate::session::Session;
use crate::ui::float;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Bounds, Context, Entity, Global, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, PathBuilder, Pixels, Point, Render, StatefulInteractiveElement, Styled, Task, Window, WindowHandle,
    canvas, div, point, px,
};
use serde_json::{Value, json};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How long a finished stroke stays, and then how long it takes to fade.
pub const HOLD: Duration = Duration::from_secs(4);
pub const FADE: Duration = Duration::from_millis(700);
/// A stroke nobody has added to in this long is taken as finished: its sender went away mid-line.
const STALE: Duration = Duration::from_secs(8);
/// How often a stroke being drawn sends what it has.
const SEND_EVERY: Duration = Duration::from_millis(40);
const SCALE: f32 = 10_000.;
const COLORS: [u32; 8] = [0xff4d6d, 0xffd166, 0x06d6a0, 0x4cc9f0, 0xc77dff, 0xff9f1c, 0x80ed99, 0xf72585];

/// A screen share, by its call and its sharer's slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Share {
    pub place: Place,
    pub mid: i64,
}

pub struct Stroke {
    pub share: Share,
    pub by: UserId,
    pub id: u32,
    pub color: Hsla,
    /// Where it went, from 0 to 1 across and down the picture.
    pub points: Vec<(f32, f32)>,
    ended: Option<Instant>,
    touched: Instant,
}

impl Stroke {
    fn finished_at(&self) -> Option<Instant> {
        self.ended.or_else(|| (self.touched.elapsed() > STALE).then_some(self.touched))
    }

    /// How much of it shows now: whole until it has been finished a while, then fading out.
    pub fn alpha(&self, now: Instant) -> f32 {
        match self.finished_at() {
            None => 1.,
            Some(t) => 1. - (now.saturating_duration_since(t + HOLD).as_secs_f32() / FADE.as_secs_f32()).min(1.),
        }
    }
}

/// Your colour: the same on every screen it is seen on.
pub fn color_of(user: UserId) -> u32 {
    COLORS[user.rem_euclid(COLORS.len() as i64) as usize]
}

/// The stroke you are drawing, and what of it has not gone out yet.
struct Pen {
    session: Entity<Session>,
    share: Share,
    id: u32,
    unsent: Vec<(f32, f32)>,
    ended: bool,
}

/// Every stroke on every share in view, and your pen.
#[derive(Default)]
pub struct Board {
    strokes: Vec<Stroke>,
    /// The pointer draws on others' shares instead of clicking.
    pub pen: bool,
    drawing: Option<Pen>,
    next_id: u32,
    sending: bool,
    ticking: Option<Task<()>>,
    /// The server answered that it doesn't know ink: said once, then nothing more is sent.
    refused: bool,
}

struct InkGlobal(Entity<Board>);
impl Global for InkGlobal {}

pub fn board(cx: &mut App) -> Entity<Board> {
    if let Some(b) = cx.try_global::<InkGlobal>() {
        return b.0.clone();
    }
    let b = cx.new(|_| Board::default());
    cx.set_global(InkGlobal(b.clone()));
    b
}

impl Board {
    pub fn on(&self, share: Share) -> impl Iterator<Item = &Stroke> {
        self.strokes.iter().filter(move |s| s.share == share)
    }

    pub fn any_on(&self, share: Share) -> bool {
        self.strokes.iter().any(|s| s.share == share)
    }

    pub fn set_pen(&mut self, on: bool, cx: &mut Context<Self>) {
        self.pen = on;
        if !on {
            self.end_stroke(cx);
        }
        cx.notify();
    }

    /// A piece of a stroke from someone else; `sharer` is who shares `share`, whose clear
    /// wipes everyone's strokes on it (anyone else's clear only their own).
    #[allow(clippy::too_many_arguments)]
    fn apply(
        &mut self,
        share: Share,
        sharer: Option<UserId>,
        by: UserId,
        id: u32,
        color: u32,
        points: Vec<(f32, f32)>,
        end: bool,
        clear: bool,
        cx: &mut Context<Self>,
    ) {
        let now = Instant::now();
        if clear {
            self.strokes.retain(|s| s.share != share || (Some(by) != sharer && s.by != by));
        } else if let Some(s) = self.strokes.iter_mut().find(|s| s.share == share && s.by == by && s.id == id) {
            s.points.extend(points);
            s.touched = now;
            if end {
                s.ended = Some(now);
            }
        } else {
            self.strokes.push(Stroke { share, by, id, color: gpui::rgb(color).into(), points, ended: end.then_some(now), touched: now });
        }
        self.changed(cx);
    }

    /// Redraws, and keeps redrawing while anything is on the board, for the fading.
    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        if self.ticking.is_some() || self.strokes.is_empty() {
            return;
        }
        self.ticking = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(33)).await;
                let more = this
                    .update(cx, |b, cx| {
                        let now = Instant::now();
                        b.strokes.retain(|s| s.alpha(now) > 0.);
                        cx.notify();
                        let more = !b.strokes.is_empty();
                        if !more {
                            b.ticking = None;
                        }
                        more
                    })
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
        }));
    }

    /// Starts a stroke of yours at `at` on `share`.
    pub fn begin(&mut self, session: Entity<Session>, share: Share, at: (f32, f32), cx: &mut Context<Self>) {
        self.end_stroke(cx);
        let me = session.read(cx).me.id;
        self.next_id = self.next_id.wrapping_add(1) & 0x7fff_ffff;
        let id = self.next_id;
        let now = Instant::now();
        self.strokes.push(Stroke { share, by: me, id, color: gpui::rgb(color_of(me)).into(), points: vec![at], ended: None, touched: now });
        self.drawing = Some(Pen { session, share, id, unsent: vec![at], ended: false });
        self.send_soon(cx);
        self.changed(cx);
    }

    pub fn extend(&mut self, at: (f32, f32), cx: &mut Context<Self>) {
        let Some(pen) = self.drawing.as_mut() else { return };
        let (share, id) = (pen.share, pen.id);
        let me = pen.session.read(cx).me.id;
        if let Some(s) = self.strokes.iter_mut().find(|s| s.share == share && s.by == me && s.id == id) {
            if s.points.last().is_some_and(|l| (l.0 - at.0).abs() + (l.1 - at.1).abs() < 0.0015) {
                return;
            }
            s.points.push(at);
            s.touched = Instant::now();
        }
        pen.unsent.push(at);
        self.send_soon(cx);
        cx.notify();
    }

    pub fn end_stroke(&mut self, cx: &mut Context<Self>) {
        let Some(pen) = self.drawing.as_mut() else { return };
        pen.ended = true;
        let (share, id) = (pen.share, pen.id);
        let me = pen.session.read(cx).me.id;
        if let Some(s) = self.strokes.iter_mut().find(|s| s.share == share && s.by == me && s.id == id) {
            s.ended = Some(Instant::now());
        }
        self.send_soon(cx);
        self.changed(cx);
    }

    pub fn drawing_on(&self, share: Share) -> bool {
        self.drawing.as_ref().is_some_and(|p| p.share == share && !p.ended)
    }

    /// Wipes `share`: everyone's strokes when it is yours, your own on someone else's.
    pub fn clear(&mut self, session: Entity<Session>, share: Share, mine: bool, cx: &mut Context<Self>) {
        let me = session.read(cx).me.id;
        self.strokes.retain(|s| s.share != share || (!mine && s.by != me));
        self.changed(cx);
        if self.refused {
            return;
        }
        let payload = share.place.with(json!({ "mid": share.mid, "id": 0, "color": hex(color_of(me)), "points": [], "clear": true }));
        let reply = Session::request(session.read(cx).realtime.clone(), "ink", payload);
        cx.spawn(async move |_, _| {
            let _ = reply.await;
        })
        .detach();
    }

    /// Sends what the pen has, a beat from now, and again while there is more.
    fn send_soon(&mut self, cx: &mut Context<Self>) {
        if self.sending || self.refused {
            return;
        }
        self.sending = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SEND_EVERY).await;
                let Ok(Some(reply)) = this.update(cx, |b, cx| b.batch(cx)) else { break };
                if let Err(e) = reply.await {
                    let _ = this.update(cx, |b, cx| b.refusal(e, cx));
                }
            }
            let _ = this.update(cx, |b, _| b.sending = false);
        })
        .detach();
    }

    /// The next piece of your stroke, sent; none when there is nothing to send.
    fn batch(&mut self, cx: &mut Context<Self>) -> Option<impl Future<Output = Result<Value, RequestError>> + use<>> {
        let pen = self.drawing.as_mut()?;
        if pen.unsent.is_empty() && !pen.ended {
            return None;
        }
        let points: Vec<u32> = pen.unsent.drain(..).flat_map(|(x, y)| [to_int(x), to_int(y)]).collect();
        let me = pen.session.read(cx).me.id;
        let payload = pen.share.place.with(json!({
            "mid": pen.share.mid,
            "id": pen.id,
            "color": hex(color_of(me)),
            "points": points,
            "end": pen.ended,
        }));
        let reply = Session::request(pen.session.read(cx).realtime.clone(), "ink", payload);
        if pen.ended {
            self.drawing = None;
        }
        Some(reply)
    }

    fn refusal(&mut self, e: RequestError, cx: &mut Context<Self>) {
        if e.code == ErrorCode::UnknownType && !self.refused {
            self.refused = true;
            self.drawing = None;
            crate::ui::overlay::toast(
                tr!(
                    "This server can't pass drawings on yet. Update it to draw on shares.",
                    "Este servidor ainda não repassa rabiscos. Atualize-o para rabiscar nas telas."
                ),
                cx,
            );
        }
    }
}

fn to_int(v: f32) -> u32 {
    (v.clamp(0., 1.) * SCALE).round() as u32
}

fn hex(c: u32) -> String {
    format!("#{c:06x}")
}

/// A drawing from the server, as `Session` received it; `sharer` is whoever shares in that slot.
pub fn receive(place: Place, sharer: Option<UserId>, v: &Value, cx: &mut App) {
    let int = |k: &str| v.get(k).and_then(Value::as_i64);
    let (Some(mid), Some(by), Some(id)) = (int("mid"), int("by"), int("id")) else { return };
    let color =
        v.get("color").and_then(Value::as_str).and_then(|c| u32::from_str_radix(c.trim_start_matches('#'), 16).ok()).unwrap_or(COLORS[0]);
    let flat: Vec<f32> = v
        .get("points")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_f64).map(|p| p as f32 / SCALE).collect())
        .unwrap_or_default();
    let points = flat.chunks_exact(2).map(|p| (p[0].clamp(0., 1.), p[1].clamp(0., 1.))).collect();
    let end = v.get("end").and_then(Value::as_bool).unwrap_or(false);
    let clear = v.get("clear").and_then(Value::as_bool).unwrap_or(false);
    board(cx).update(cx, |b, cx| b.apply(Share { place, mid }, sharer, by, id as u32, color, points, end, clear, cx));
}

/// Draws the strokes of `share` over `video`, the picture's place on the screen.
pub fn paint(strokes: &[(Vec<(f32, f32)>, Hsla)], video: Bounds<Pixels>, window: &mut Window) {
    let width = (f32::from(video.size.height) / 220.).clamp(2.5, 7.);
    let at = |(x, y): (f32, f32)| video.origin + point(video.size.width * x, video.size.height * y);
    for (points, color) in strokes {
        match points.as_slice() {
            [] => {}
            [one] => {
                let c = at(*one);
                let r = px(width / 2. + 0.5);
                window.paint_quad(gpui::fill(Bounds::new(c - point(r, r), gpui::size(r * 2., r * 2.)), *color).corner_radii(r));
            }
            [first, rest @ ..] => {
                let mut path = PathBuilder::stroke(px(width));
                path.move_to(at(*first));
                for p in rest {
                    path.line_to(at(*p));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, *color);
                }
            }
        }
    }
}

/// What of the board is drawn on `share` now, with the fading applied.
fn visible(board: &Board, share: Share) -> Vec<(Vec<(f32, f32)>, Hsla)> {
    let now = Instant::now();
    board.on(share).map(|s| (s.points.clone(), s.color.opacity(s.alpha(now)))).collect()
}

/// Where a picture of `frame` (width, height) lands in `bounds` when fitted whole, as the video
/// is drawn.
pub fn contain(bounds: Bounds<Pixels>, frame: (f32, f32)) -> Bounds<Pixels> {
    let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let (fw, fh) = (frame.0.max(1.), frame.1.max(1.));
    let s = (bw / fw).min(bh / fh);
    let (w, h) = (fw * s, fh * s);
    Bounds::new(bounds.origin + point(px((bw - w) / 2.), px((bh - h) / 2.)), gpui::size(px(w), px(h)))
}

/// The drawings over a share in the window, and the pen on it: a view of its own, so the fading
/// redraws it alone.
pub struct InkLayer {
    board: Entity<Board>,
    session: Entity<Session>,
    pub share: Share,
    /// The picture's size, for where it sits in the tile.
    pub frame: (f32, f32),
    /// Drawing on it is possible: someone else's share, on a server that passes ink on.
    pub drawable: bool,
    video: Rc<Cell<Bounds<Pixels>>>,
}

impl InkLayer {
    pub fn new(session: Entity<Session>, share: Share, cx: &mut Context<Self>) -> InkLayer {
        let board = board(cx);
        cx.observe(&board, |_, _, cx| cx.notify()).detach();
        InkLayer { board, session, share, frame: (16., 9.), drawable: false, video: Default::default() }
    }

    fn spot(&self, p: Point<Pixels>) -> (f32, f32) {
        let v = self.video.get();
        let x = (f32::from(p.x - v.origin.x) / f32::from(v.size.width).max(1.)).clamp(0., 1.);
        let y = (f32::from(p.y - v.origin.y) / f32::from(v.size.height).max(1.)).clamp(0., 1.);
        (x, y)
    }
}

impl Render for InkLayer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let strokes = visible(self.board.read(cx), self.share);
        let pen = self.drawable && self.board.read(cx).pen;
        let (video, frame) = (self.video.clone(), self.frame);
        div()
            .id("ink")
            .absolute()
            .inset_0()
            .child(
                canvas(
                    move |bounds, _, _| {
                        let v = contain(bounds, frame);
                        video.set(v);
                        v
                    },
                    move |_, v, window, _| paint(&strokes, v, window),
                )
                .size_full(),
            )
            .when(pen, |d| {
                d.cursor(gpui::CursorStyle::Crosshair)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            let (session, share, at) = (this.session.clone(), this.share, this.spot(e.position));
                            this.board.update(cx, |b, cx| b.begin(session, share, at, cx));
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                        if e.pressed_button == Some(MouseButton::Left) && this.board.read(cx).drawing_on(this.share) {
                            let at = this.spot(e.position);
                            this.board.update(cx, |b, cx| b.extend(at, cx));
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            cx.stop_propagation();
                            this.board.update(cx, |b, cx| b.end_stroke(cx));
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| this.board.update(cx, |b, cx| b.end_stroke(cx))),
                    )
                    .on_click(|_, _, cx| cx.stop_propagation())
            })
    }
}

/// What you share, for the window that shows its drawings over it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shared {
    Screen(u64),
    Window(u64),
}

impl Shared {
    fn rect(self) -> Option<float::Rect> {
        match self {
            Shared::Screen(id) => float::screen_rect(id),
            Shared::Window(id) => float::window_rect(id),
        }
    }
}

/// The drawings on your share, over the screen or window you share: they float above it,
/// clicks go through, and the share itself doesn't show them (viewers draw their own).
pub struct ScreenInk {
    board: Entity<Board>,
    share: Share,
    shared: Shared,
    _follow: Task<()>,
}

impl ScreenInk {
    fn new(share: Share, shared: Shared, window: &mut Window, cx: &mut Context<Self>) -> ScreenInk {
        let board = board(cx);
        cx.observe(&board, |_, _, cx| cx.notify()).detach();
        // Follows a shared window as it moves, and stays above what comes over it.
        let follow = cx.spawn_in(window, async move |this, cx| {
            loop {
                let Ok((hwnd, rect)) = this.update_in(cx, |this, window, _| (float::hwnd(window), this.shared.rect())) else { break };
                if let Some(hwnd) = hwnd {
                    float::place(hwnd, rect.unwrap_or(float::Rect { x: -32000, y: -32000, w: 1, h: 1 }));
                }
                cx.background_executor().timer(Duration::from_millis(500)).await;
            }
        });
        ScreenInk { board, share, shared, _follow: follow }
    }
}

impl Render for ScreenInk {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let strokes = visible(self.board.read(cx), self.share);
        canvas(|bounds, _, _| bounds, move |_, b, window, _| paint(&strokes, b, window)).size_full()
    }
}

struct ScreenInkGlobal(Option<(Share, Shared, WindowHandle<ScreenInk>)>);
impl Global for ScreenInkGlobal {}

/// Shows others' drawings over what you share (`None` when you share nothing, or the setting is
/// off): opens, moves or closes the window that holds them.
pub fn show_on_screen(target: Option<(Share, Shared)>, cx: &mut App) {
    let current = cx.try_global::<ScreenInkGlobal>().and_then(|g| g.0.as_ref().map(|(s, d, w)| (*s, *d, *w)));
    if current.map(|(s, d, _)| (s, d)) == target {
        return;
    }
    if let Some((_, _, handle)) = current {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
    let opened = target.and_then(|(share, shared)| {
        float::open(cx, |window, cx| cx.new(|cx| ScreenInk::new(share, shared, window, cx))).map(|h| (share, shared, h))
    });
    cx.set_global(ScreenInkGlobal(opened));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_fits_whole_and_centred() {
        let b = Bounds::new(point(px(10.), px(20.)), gpui::size(px(400.), px(300.)));
        let v = contain(b, (1920., 1080.));
        assert_eq!(v.size.width, px(400.));
        assert_eq!(v.size.height, px(225.));
        assert_eq!(v.origin, point(px(10.), px(20. + 37.5)));
    }

    #[test]
    fn a_finished_stroke_fades_after_a_while() {
        let now = Instant::now();
        let mut s = Stroke {
            share: Share { place: Place::Channel(1), mid: 0 },
            by: 1,
            id: 1,
            color: gpui::rgb(COLORS[0]).into(),
            points: vec![(0.1, 0.1)],
            ended: None,
            touched: now,
        };
        assert_eq!(s.alpha(now + HOLD * 2), 1., "still being drawn");
        s.ended = Some(now);
        assert_eq!(s.alpha(now + HOLD / 2), 1.);
        assert!((s.alpha(now + HOLD + FADE / 2) - 0.5).abs() < 0.01);
        assert_eq!(s.alpha(now + HOLD + FADE), 0.);
    }

    #[test]
    fn points_travel_as_ten_thousandths() {
        assert_eq!(to_int(0.5), 5000);
        assert_eq!(to_int(1.2), 10_000);
        assert_eq!(hex(0x06d6a0), "#06d6a0");
        assert_eq!(color_of(-3), color_of(5));
    }
}
