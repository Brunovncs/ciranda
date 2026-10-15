//! The in-game overlay: who is in your call, lit while they talk, in a corner of the screen
//! you're playing on, over the game. Off until turned on in Settings; a hotkey hides and shows
//! it. Clicks go through it, it never takes the focus, and a screen share doesn't show it.
//! Short notices ("Replay saved") show in it too, since a toast in the window would go unseen.

use crate::media::call::Call;
use crate::prefs::prefs;
use crate::session::Session;
use crate::theme::px;
use crate::ui::float;
use crate::widgets::{avatar, icon};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, FontWeight, Global, IntoElement, ParentElement, Render, SharedString, Styled, Task, Window,
    WindowHandle, div,
};
use std::time::{Duration, Instant};

/// The overlay's size, in the window's own pixels; what isn't drawn on is see-through.
const WIDTH: f32 = 300.;
const HEIGHT: f32 = 460.;
/// Its distance from the screen's edges.
const MARGIN: f32 = 18.;
const NOTICE: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct GameOverlay {
    window: Option<WindowHandle<CallOverlay>>,
    /// The hotkey hid it.
    hidden: bool,
    call: Option<(Entity<Call>, Entity<Session>)>,
}

impl Global for GameOverlay {}

fn state(cx: &mut App) -> &mut GameOverlay {
    if cx.try_global::<GameOverlay>().is_none() {
        cx.set_global(GameOverlay::default());
    }
    cx.global_mut::<GameOverlay>()
}

/// The call in progress, or none: the overlay shows it, and goes with it.
pub fn set_call(call: Option<(Entity<Call>, Entity<Session>)>, cx: &mut App) {
    let s = state(cx);
    if s.call.as_ref().map(|c| c.0.entity_id()) == call.as_ref().map(|c| c.0.entity_id()) {
        return;
    }
    s.call = call;
    if let Some(handle) = s.window.take() {
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        });
    }
    sync(cx);
}

/// The hotkey: hides the overlay, or shows it again. Says what it did, false for hidden.
pub fn toggle(cx: &mut App) -> bool {
    let s = state(cx);
    s.hidden = !s.hidden;
    let shown = !s.hidden;
    sync(cx);
    shown
}

/// Opens or closes the window as the settings, the hotkey and the call say: once the update in
/// progress is over, since a window opening or closing calls into the others (see `float`).
pub fn sync(cx: &mut App) {
    cx.defer(apply);
}

fn apply(cx: &mut App) {
    let enabled = prefs(cx).overlay_enabled;
    let s = state(cx);
    let want = enabled && !s.hidden && s.call.is_some();
    match (want, s.window.is_some()) {
        (true, false) => {
            let Some((call, session)) = s.call.clone() else { return };
            let handle = float::open(cx, |window, cx| cx.new(|cx| CallOverlay::new(call, session, window, cx)));
            state(cx).window = handle;
        }
        (false, true) => {
            if let Some(handle) = state(cx).window.take() {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
        }
        _ => {
            if let Some(handle) = s.window {
                let _ = handle.update(cx, |_, _, cx| cx.notify());
            }
        }
    }
}

/// A line shown for a few seconds at the top of the overlay, when it is up.
pub fn notice(text: impl Into<SharedString>, cx: &mut App) {
    let Some(handle) = cx.try_global::<GameOverlay>().and_then(|s| s.window) else { return };
    let text = text.into();
    let _ = handle.update(cx, |v, _, cx| {
        v.notice = Some((text, Instant::now()));
        cx.notify();
        v.notice_end = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE).await;
            let _ = this.update(cx, |v, cx| {
                v.notice = None;
                cx.notify();
            });
        });
    });
}

pub struct CallOverlay {
    call: Entity<Call>,
    session: Entity<Session>,
    notice: Option<(SharedString, Instant)>,
    notice_end: Task<()>,
    _follow: Task<()>,
}

impl CallOverlay {
    fn new(call: Entity<Call>, session: Entity<Session>, window: &mut Window, cx: &mut Context<Self>) -> CallOverlay {
        cx.observe(&call, |_, _, cx| cx.notify()).detach();
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        // Follows the game to whichever monitor it is on, and stays on top of it.
        let follow = cx.spawn_in(window, async move |this, cx| {
            loop {
                let Ok(spot) = this.update_in(cx, |_, window, cx| float::hwnd(window).zip(spot(window, cx))) else { break };
                if let Some((hwnd, rect)) = spot {
                    float::place(hwnd, rect);
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        });
        CallOverlay { call, session, notice: None, notice_end: Task::ready(()), _follow: follow }
    }
}

/// Its corner of the screen the focused window is on.
fn spot(window: &Window, cx: &App) -> Option<float::Rect> {
    let (_, work) = float::foreground_monitor()?;
    let s = window.scale_factor() * crate::theme::scale();
    let (w, h, m) = ((WIDTH * s) as i32, (HEIGHT * s) as i32, (MARGIN * s) as i32);
    let corner = prefs(cx).overlay_corner.clone();
    let x = if corner.ends_with("right") { work.x + work.w - w - m } else { work.x + m };
    let y = if corner.starts_with("bottom") { work.y + work.h - h - m } else { work.y + m };
    Some(float::Rect { x, y, w, h })
}

impl Render for CallOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = prefs(cx);
        let (bottom, right, only_speaking) =
            (p.overlay_corner.starts_with("bottom"), p.overlay_corner.ends_with("right"), p.overlay_only_speaking);
        let call = self.call.read(cx);
        let place = call.place;
        let roster = self.session.read(cx).roster(place).cloned().unwrap_or_default();
        let white = gpui::white();
        let green = gpui::rgb(0x3ddc84).into();
        let mut rows = Vec::new();
        for m in &roster {
            let speaking = self.call.read(cx).is_speaking(m.user_id, cx);
            if only_speaking && !speaking {
                continue;
            }
            let name = self.session.read(cx).display_name(Some(m.user_id), Some(&m.nickname));
            let image = self.session.update(cx, |s, cx| s.avatar(m.user_id, cx));
            let quiet = m.muted || m.silenced();
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(34.))
                    .pl(px(4.))
                    .pr(px(12.))
                    .rounded_full()
                    .bg(gpui::black().opacity(if speaking { 0.72 } else { 0.5 }))
                    .child(avatar(&name, image, 26., speaking.then_some(green)))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(if speaking { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                            .text_color(if speaking { white } else { white.opacity(0.8) })
                            .child(name),
                    )
                    .when(quiet, |d| d.child(icon("mic-off", 13., white.opacity(0.7))))
                    .when(m.deafened, |d| d.child(icon("headphones-off", 13., white.opacity(0.7)))),
            );
        }
        let notice = self.notice.as_ref().map(|(text, _)| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(34.))
                .px(px(12.))
                .rounded_full()
                .bg(gpui::black().opacity(0.78))
                .child(icon("check", 14., green))
                .child(div().text_size(px(13.)).text_color(white).child(text.clone()))
        });
        let mut column = div().flex().flex_col().gap(px(6.)).size_full().when(right, |d| d.items_end()).when(!right, |d| d.items_start());
        column = if bottom { column.justify_end().children(rows).children(notice) } else { column.children(notice).children(rows) };
        column
    }
}
